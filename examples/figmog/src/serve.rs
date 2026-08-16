//! `figmog serve` — an MCP stdio server with the sync loop built in.
//!
//! One process owns the store (build design §11): a reader thread turns
//! stdin lines into an `mpsc` channel; the main loop owns the [`fold`]
//! store and answers JSON-RPC requests between poll ticks. Every
//! `figmog_*` tool is a thin wrapper over the same `query::*` functions
//! the CLI prints — one source of truth for every answer. `figmog_sync`
//! and the background poll loop share the same pull mechanics
//! (`flatten_file` → `collect_sweepable` → `store::sync`) and the same
//! failure-backoff discipline as `figmog watch` (see `cli::pull_failure_wait`).
//!
//! Every `rtx`/`wtx` call against the store has to live at this concrete,
//! non-generic call site: `open_store!`'s pipeline type contains fn items
//! and can't be named, so it can't be threaded through a helper `fn`
//! generic over `P: Push<..>` (see the identical note in `cli::dispatch`).
//! The [`mcp::ToolHandler`] the loop hands to [`mcp::handle_message`] is
//! therefore a closure — wrapped in [`mcp::FnHandler`] — defined right
//! here, capturing the store by unique reference.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::api::{FigmaApi, UreqApi};
use crate::cli::{
    Db, PullError, do_pull, now_ms, pull_failure_wait, read_watermark, write_current,
};
use crate::flatten::flatten_file;
use crate::ident::parse_file_ref;
use crate::mcp::{self, FnHandler, ToolDef};
use crate::model::Id;
use crate::query;
use crate::store::{self, collect_sweepable};
use crate::watch::{BACKOFF_START, Tick, Watcher};

/// Run the MCP stdio server against `db`, serving `figmog_*` tools and —
/// unless `no_watch` — pulling inline whenever the file changes.
///
/// `file` resolves the mirrored key the same way `pull`/`watch` do (a
/// `--db` override alone is enough for a read-only, offline server; a key
/// is only required once network access is actually needed: `!no_watch`,
/// or a `figmog_sync` tool call).
pub(crate) fn run_serve(
    db: &Db,
    file: Option<String>,
    interval: u64,
    no_watch: bool,
) -> Result<(), String> {
    let key: Option<String> = db
        .key
        .clone()
        .or_else(|| file.and_then(|f| parse_file_ref(&f)));

    let interval_dur = Duration::from_secs(interval);
    let api: Option<UreqApi> = if no_watch {
        None
    } else {
        let resolved = key
            .clone()
            .ok_or_else(|| "no file key: pass a file key or figma.com URL".to_string())?;
        let token = std::env::var("FIGMA_TOKEN")
            .map_err(|_| "FIGMA_TOKEN not set — required for watch".to_string())?;
        if read_watermark(db).is_none() {
            do_pull(db, Some(resolved), None, false).map_err(|e| e.to_string())?;
        }
        Some(UreqApi::new(token))
    };

    eprintln!(
        "{} serving {} (watch {})",
        mcp::SERVER_NAME,
        key.as_deref().unwrap_or("<no file key>"),
        if no_watch { "off" } else { "on" }
    );

    // Reader thread: stdin lines -> mpsc. EOF (or any read error) drops
    // `tx`, which is how the main loop learns to exit (`recv`/`recv_timeout`
    // return `Disconnected`).
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut st = crate::open_store!(&db.path);
    let mut stored: Option<String> =
        st.rtx(|(_, _, _, _, _, _, meta)| meta.get(&0).map(|m| m.last_modified));
    let mut watcher = Watcher::new(stored.clone());
    let mut pull_backoff = BACKOFF_START;
    let tools = tool_registry();
    let mut next_deadline = Instant::now() + interval_dur;

    loop {
        let incoming = if no_watch {
            // No ticking to do, so a disconnect (stdin EOF) is the only
            // thing `recv` can report besides a line — exit clean rather
            // than falling into the (watch-only) timeout branch below.
            match rx.recv() {
                Ok(line) => Some(line),
                Err(mpsc::RecvError) => return Ok(()),
            }
        } else {
            let wait = next_deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(line) => Some(line),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        };

        let Some(line) = incoming else {
            // Timeout with watch enabled: poll, and pull inline on change.
            let api_ref = api
                .as_ref()
                .expect("api is Some whenever watch is enabled, the only way to reach a timeout");
            let watch_key = key
                .as_deref()
                .expect("key is resolved above whenever watch is enabled");
            match watcher.tick(api_ref, watch_key) {
                Tick::Unchanged => next_deadline = Instant::now() + interval_dur,
                Tick::Wait { after } => next_deadline = Instant::now() + after,
                Tick::Changed { .. } => {
                    let pull_result: Result<store::Churn, PullError> = (|| {
                        let resp = api_ref.file(watch_key)?;
                        let flattened = flatten_file(&resp).map_err(|e| e.to_string())?;
                        let prior: BTreeSet<Id> =
                            st.rtx(|((nodes, ..), components, component_sets, styles, ..)| {
                                collect_sweepable(&nodes, &components, &component_sets, &styles)
                            });
                        Ok(store::sync(&mut st, &prior, &flattened, now_ms()))
                    })();
                    match pull_result {
                        Ok(_churn) => {
                            stored = st.rtx(|(_, _, _, _, _, _, meta)| {
                                meta.get(&0).map(|m| m.last_modified)
                            });
                            pull_backoff = BACKOFF_START;
                            if let Some(k) = &db.key {
                                let _ = write_current(k);
                            }
                            eprintln!("figmog: synced");
                            next_deadline = Instant::now() + interval_dur;
                        }
                        Err(e) => {
                            eprintln!("figmog: pull failed: {e}");
                            // Reset to the last successfully-synced watermark
                            // so the same change is re-detected next tick —
                            // same discipline as `cmd_watch`.
                            watcher = Watcher::new(stored.clone());
                            let wait = pull_failure_wait(&e, &mut pull_backoff, interval_dur);
                            next_deadline = Instant::now() + wait;
                        }
                    }
                }
            }
            continue;
        };

        let mut handler = FnHandler(|name: &str, args: &Value| -> Result<Value, String> {
            match name {
                "figmog_status" => st.rtx(|((nodes, _, _, _, _, _, _), _, _, _, _, _, meta)| {
                    query::status(&nodes, &meta)
                }),
                "figmog_pages" => {
                    st.rtx(|((nodes, _, _, _, _, _, by_type), ..)| query::pages(&nodes, &by_type))
                }
                "figmog_tree" => {
                    let id = arg_str(args, "id");
                    let depth = arg_usize(args, "depth");
                    st.rtx(|((nodes, children, _, _, _, _, by_type), ..)| {
                        query::tree(&nodes, &children, &by_type, id, depth)
                    })
                }
                "figmog_node" => {
                    let id = require_str(args, "id")?;
                    let with_children = arg_bool(args, "children");
                    st.rtx(|((nodes, children, ..), ..)| {
                        query::node(&nodes, &children, id, with_children)
                    })
                }
                "figmog_find" => {
                    let node_type = require_str(args, "type")?;
                    let page = arg_str(args, "page");
                    st.rtx(|((nodes, _, _, _, _, _, by_type), ..)| {
                        query::find(&nodes, &by_type, node_type, page)
                    })
                }
                "figmog_search" => {
                    let q = require_str(args, "query")?;
                    let limit = arg_usize(args, "limit").unwrap_or(10);
                    st.rtx(|((nodes, _, text, ..), ..)| query::search(&text, &nodes, &q, limit))
                }
                "figmog_instances" => {
                    let target = require_str(args, "target")?;
                    st.rtx(
                        |((nodes, _, _, instances_of, ..), components, component_sets, ..)| {
                            query::instances(
                                &nodes,
                                &components,
                                &component_sets,
                                &instances_of,
                                &target,
                            )
                        },
                    )
                }
                "figmog_components" => st.rtx(|((nodes, ..), components, component_sets, ..)| {
                    query::components(&nodes, &components, &component_sets)
                }),
                "figmog_styles" => {
                    let style_type = arg_str(args, "type");
                    let values = arg_bool(args, "values");
                    st.rtx(|((nodes, _, _, _, styled_by, ..), _, _, styles, ..)| {
                        query::styles(&nodes, &styles, &styled_by, style_type, values)
                    })
                }
                "figmog_uses" => {
                    let id = require_str(args, "id")?;
                    st.rtx(|((nodes, _, _, _, styled_by, bound_to, _), ..)| {
                        query::uses(&nodes, &styled_by, &bound_to, &id)
                    })
                }
                "figmog_vars" => {
                    let id = arg_str(args, "id");
                    st.rtx(
                        |((nodes, ..), _, _, _, variables, variable_collections, _)| {
                            query::vars(&nodes, &variables, &variable_collections, id)
                        },
                    )
                }
                "figmog_sync" => {
                    let sync_key = key.clone().ok_or_else(|| {
                        "no file key: pass a file key or figma.com URL".to_string()
                    })?;
                    let token = std::env::var("FIGMA_TOKEN").map_err(|_| {
                        "FIGMA_TOKEN not set — required for figmog_sync".to_string()
                    })?;
                    let sync_api = UreqApi::new(token);
                    let pull_result: Result<store::Churn, PullError> = (|| {
                        let resp = sync_api.file(&sync_key)?;
                        let flattened = flatten_file(&resp).map_err(|e| e.to_string())?;
                        let prior: BTreeSet<Id> =
                            st.rtx(|((nodes, ..), components, component_sets, styles, ..)| {
                                collect_sweepable(&nodes, &components, &component_sets, &styles)
                            });
                        Ok(store::sync(&mut st, &prior, &flattened, now_ms()))
                    })();
                    // A failed manual sync still spends the same backoff
                    // budget as a failed background tick, and — when watch
                    // is enabled — the next tick must not fire back into a
                    // rate-limit window this call just learned about.
                    let churn = match pull_result {
                        Ok(c) => c,
                        Err(e) => {
                            let wait = pull_failure_wait(&e, &mut pull_backoff, interval_dur);
                            next_deadline = Instant::now() + wait;
                            return Err(e.to_string());
                        }
                    };
                    stored =
                        st.rtx(|(_, _, _, _, _, _, meta)| meta.get(&0).map(|m| m.last_modified));
                    pull_backoff = BACKOFF_START;
                    watcher = Watcher::new(stored.clone());
                    if let Some(k) = &db.key {
                        let _ = write_current(k);
                    }
                    serde_json::to_value(&churn).map_err(|e| e.to_string())
                }
                "figmog_stats" => st.rtx(
                    |(
                        (nodes, _, _, _, _, _, by_type),
                        components,
                        component_sets,
                        styles,
                        variables,
                        ..,
                    )| {
                        query::stats(
                            &nodes,
                            &components,
                            &component_sets,
                            &styles,
                            &variables,
                            &by_type,
                        )
                    },
                ),
                "figmog_path" => {
                    let id = require_str(args, "id")?;
                    st.rtx(|((nodes, ..), ..)| query::path(&nodes, id))
                }
                "figmog_text" => {
                    let page = arg_str(args, "page");
                    st.rtx(|((nodes, _, _, _, _, _, by_type), ..)| {
                        query::text(&nodes, &by_type, page)
                    })
                }
                "figmog_where" => {
                    let pointer = require_str(args, "pointer")?;
                    let equals = args.get("equals").cloned();
                    let page = arg_str(args, "page");
                    st.rtx(|((nodes, ..), ..)| query::where_(&nodes, &pointer, equals, page))
                }
                "figmog_at" => {
                    let x = require_f64(args, "x")?;
                    let y = require_f64(args, "y")?;
                    st.rtx(|((nodes, ..), ..)| query::at(&nodes, x, y))
                }
                other => Err(format!("unknown tool: {other}")),
            }
        });

        if let Some(resp) = mcp::handle_message(&line, &tools, &mut handler) {
            println!("{resp}");
            std::io::stdout().flush().map_err(|e| e.to_string())?;
        }
    }
}

// ---- arg extraction ----

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn require_str(args: &Value, key: &str) -> Result<String, String> {
    arg_str(args, key).ok_or_else(|| format!("missing required field: {key}"))
}

fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(Value::as_u64).map(|n| n as usize)
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn require_f64(args: &Value, key: &str) -> Result<f64, String> {
    args.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing required field: {key}"))
}

// ---- tool registry ----

/// The 17 `figmog_*` MCP tools: 12 core reads + 5 whole-file structural
/// queries (build design §11's two tables). Every tool but `figmog_sync`
/// reads the local mirror at zero Figma API cost.
fn tool_registry() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "figmog_status",
            description: "File name, version, last modified time, and node count — reads the local mirror (no Figma API cost).",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "figmog_pages",
            description: "List the file's pages (CANVAS nodes), in document order — reads the local mirror (no Figma API cost).",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "figmog_tree",
            description: "Subtree outline (id, name, type, children) rooted at a node, defaulting to the whole document — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Root node id; defaults to the DOCUMENT node."},
                    "depth": {"type": "integer", "description": "Max depth to descend; omitted means unlimited."}
                }
            }),
        },
        ToolDef {
            name: "figmog_node",
            description: "Full raw JSON of one node by id, optionally with a one-level children summary — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Node id (12:34 or 12-34 form)."},
                    "children": {"type": "boolean", "description": "Inline a one-level children summary."}
                },
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "figmog_find",
            description: "Nodes by Figma node type, optionally scoped to one page — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "type": {"type": "string", "description": "Figma node type, e.g. FRAME."},
                    "page": {"type": "string", "description": "Page (CANVAS) node id to scope to."}
                },
                "required": ["type"]
            }),
        },
        ToolDef {
            name: "figmog_search",
            description: "BM25 search over layer names and text content — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "description": "Max hits (default 10)."}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "figmog_instances",
            description: "Instances of a component, resolved by node id, global key, or component/component-set name — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "Node id, key, or name of a component or component set."}
                },
                "required": ["target"]
            }),
        },
        ToolDef {
            name: "figmog_components",
            description: "Design-system inventory: component sets with their variant axes, plus standalone components — reads the local mirror (no Figma API cost).",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "figmog_styles",
            description: "Styles with usage counts; `values` derives each style's definition from a consumer node — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "type": {"type": "string", "description": "Style type filter, e.g. FILL, TEXT."},
                    "values": {"type": "boolean", "description": "Derive each style's definition from a consumer node."}
                }
            }),
        },
        ToolDef {
            name: "figmog_uses",
            description: "Nodes using a style id or bound to a variable id — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {"id": {"type": "string", "description": "A style id or variable id."}},
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "figmog_vars",
            description: "Variables: the authoritative record if imported via figmog import-variables, else inferred from bindings — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {"id": {"type": "string", "description": "Variable id filter; omitted means all variables."}}
            }),
        },
        ToolDef {
            name: "figmog_sync",
            description: "Forces one pull from Figma and returns the sync churn (+added ~changed -removed) — fetches from Figma (spends Tier-1 rate budget).",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "figmog_stats",
            description: "Node counts by type and by page, component/set/style/variable totals, text-node count, max tree depth — reads the local mirror (no Figma API cost).",
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "figmog_path",
            description: "Ancestor chain from the document root to a node, as [{id, name, type}] — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }),
        },
        ToolDef {
            name: "figmog_text",
            description: "Every TEXT node's (id, characters, page_id), optionally scoped to one page, sorted by id — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {"page": {"type": "string"}}
            }),
        },
        ToolDef {
            name: "figmog_where",
            description: "Nodes whose raw JSON matches an RFC 6901 pointer, optionally filtered by value — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pointer": {"type": "string", "description": "RFC 6901 pointer into the node's raw JSON, e.g. /layoutMode."},
                    "equals": {"description": "JSON value to match; omitted means \"pointer exists\"."},
                    "page": {"type": "string"}
                },
                "required": ["pointer"]
            }),
        },
        ToolDef {
            name: "figmog_at",
            description: "Nodes whose absolute bounds contain a point, sorted by area ascending (deepest/smallest first) — reads the local mirror (no Figma API cost).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "x": {"type": "number"},
                    "y": {"type": "number"}
                },
                "required": ["x", "y"]
            }),
        },
    ]
}
