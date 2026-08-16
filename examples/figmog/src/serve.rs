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
//! **v3 (build design §12):** unless `--no-upstream`, figmog also probes
//! Figma's native desktop MCP server at startup and becomes the *only*
//! Figma MCP an agent needs — `tools/list` merges the 17 local `figmog_*`
//! tools with every upstream tool verbatim (`proxy::merge_registry`), and
//! `tools/call` routes by the namespace rule (`proxy::is_local_tool`):
//! local names answer from the store exactly as in v2; everything else is
//! proxied, with `get_*`/`list_*` calls against an explicit node id served
//! from (and written to) the version-keyed `proxy_cache` table
//! (`proxy::proxy_call`). No mid-session re-probe: an unreachable upstream
//! at startup means local-only tools for the life of the process.
//!
//! Every `rtx`/`wtx` call against the store has to live at this concrete,
//! non-generic call site: `open_store!`'s pipeline type contains fn items
//! and can't be named, so it can't be threaded through a helper `fn`
//! generic over `P: Push<..>` (see the identical note in `cli::dispatch`).
//! The [`mcp::ToolHandler`] the loop hands to [`mcp::handle_message`] is
//! therefore a closure — wrapped in [`mcp::FnHandler`] — defined right
//! here, capturing the store by unique reference. What *can* be shared
//! across call sites — because it only needs individual reader values, or
//! only `wtx`, never a raw `rtx` tuple pattern spelled out generically —
//! lives in `crate::dispatch` (local tool reads) and `crate::proxy`
//! (routing rules and the proxied-call execution), and both `run_serve`
//! and the CLI's `figmog call`/`figmog tools` (`cli.rs`) call into them.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::{FigmaApi, UreqApi};
use crate::cli::{
    Db, PullError, do_pull, now_ms, pull_failure_wait, read_watermark, write_current,
};
use crate::dispatch;
use crate::flatten::flatten_file;
use crate::ident::parse_file_ref;
use crate::mcp::{self, FnHandler};
use crate::model::Id;
use crate::proxy;
use crate::store::{self, collect_sweepable};
use crate::upstream::{HttpUpstream, UpstreamMcp};
use crate::watch::{BACKOFF_START, Tick, Watcher};

/// Default streamable-HTTP URL of Figma desktop app's Dev Mode MCP server
/// (build design §12).
pub const DEFAULT_UPSTREAM_URL: &str = "http://127.0.0.1:3845/mcp";

/// Run the MCP stdio server against `db`, serving `figmog_*` tools and —
/// unless `no_watch` — pulling inline whenever the file changes. Unless
/// `no_upstream`, also attaches Figma's native desktop MCP server at
/// `upstream_url` as a cached proxy (build design §12); a failed probe
/// degrades to local-only tools with one stderr line, never a hard error.
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
    upstream_url: String,
    no_upstream: bool,
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

    // Upstream probe: no mid-session re-probe in v3 — an unreachable
    // desktop server at startup means local-only tools for the process's
    // whole life (build design §12).
    let mut upstream: Option<HttpUpstream> = if no_upstream {
        None
    } else {
        let mut client = HttpUpstream::new(upstream_url);
        match client.initialize() {
            Ok(()) => Some(client),
            Err(e) => {
                eprintln!("figmog: upstream unreachable, serving local tools only: {e}");
                None
            }
        }
    };
    let upstream_status: &'static str = if no_upstream {
        "disabled"
    } else if upstream.is_some() {
        "connected"
    } else {
        "unreachable"
    };

    let (tools, dropped) = match &upstream {
        Some(u) => proxy::merge_registry(dispatch::tool_registry(), u.tools()),
        None => (dispatch::tool_registry(), Vec::new()),
    };
    for name in &dropped {
        eprintln!("figmog: dropping upstream tool named like a local tool: {name}");
    }

    eprintln!(
        "{} serving {} (watch {}, upstream {upstream_status})",
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
        st.rtx(|(_, _, _, _, _, _, meta, _)| meta.get(&0).map(|m| m.last_modified));
    let mut watcher = Watcher::new(stored.clone());
    let mut pull_backoff = BACKOFF_START;
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
                            stored = st.rtx(|(_, _, _, _, _, _, meta, _)| {
                                meta.get(&0).map(|m| m.last_modified)
                            });
                            // Sweep any proxy_cache rows the new version made
                            // stale (spec §12; a no-op if the version didn't
                            // actually move — see `store.rs`'s eviction note).
                            let version = st.rtx(|(_, _, _, _, _, _, meta, _)| {
                                meta.get(&0).map(|m| m.version.clone())
                            });
                            if let Some(version) = version {
                                let stale = st.rtx(|(_, _, _, _, _, _, _, cache)| {
                                    store::stale_cache_ids(&cache, &version)
                                });
                                if !stale.is_empty() {
                                    store::evict_stale_cache(&mut st, &stale);
                                }
                            }
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
            if name == "figmog_sync" {
                let sync_key = key
                    .clone()
                    .ok_or_else(|| "no file key: pass a file key or figma.com URL".to_string())?;
                let token = std::env::var("FIGMA_TOKEN")
                    .map_err(|_| "FIGMA_TOKEN not set — required for figmog_sync".to_string())?;
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
                    st.rtx(|(_, _, _, _, _, _, meta, _)| meta.get(&0).map(|m| m.last_modified));
                // Sweep any proxy_cache rows the new version made stale
                // (spec §12; a no-op if the version didn't actually move).
                let version =
                    st.rtx(|(_, _, _, _, _, _, meta, _)| meta.get(&0).map(|m| m.version.clone()));
                if let Some(version) = version {
                    let stale = st.rtx(|(_, _, _, _, _, _, _, cache)| {
                        store::stale_cache_ids(&cache, &version)
                    });
                    if !stale.is_empty() {
                        store::evict_stale_cache(&mut st, &stale);
                    }
                }
                pull_backoff = BACKOFF_START;
                watcher = Watcher::new(stored.clone());
                if let Some(k) = &db.key {
                    let _ = write_current(k);
                }
                return serde_json::to_value(&churn).map_err(|e| e.to_string());
            }

            if let Some(result) =
                st.rtx(|r| dispatch::dispatch_read_tool(name, args, upstream_status, r))
            {
                return result;
            }

            if proxy::is_local_tool(name) {
                return Err(format!("unknown tool: {name}"));
            }

            let up = upstream
                .as_mut()
                .ok_or_else(|| format!("upstream not attached: {name}"))?;
            let args_canonical = proxy::canonical_args(args);
            let version_and_hit = if proxy::is_cacheable(name, args) {
                st.rtx(|(_, _, _, _, _, _, meta, cache)| {
                    let version = meta.get(&0).map(|m| m.version.clone());
                    let hit = version
                        .as_ref()
                        .and_then(|v| crate::cache::lookup(&cache, name, &args_canonical, v));
                    (version, hit)
                })
            } else {
                (None, None)
            };
            let (value, trigger_poll) =
                proxy::proxy_call(&mut st, up, name, args, version_and_hit)?;
            if trigger_poll && !no_watch {
                next_deadline = Instant::now();
            }
            Ok(value)
        });

        if let Some(resp) = mcp::handle_message(&line, &tools, &mut handler) {
            println!("{resp}");
            std::io::stdout().flush().map_err(|e| e.to_string())?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::ToolDef;
    use crate::upstream::FakeUpstream;
    use serde_json::json;

    fn local_registry() -> Vec<ToolDef> {
        dispatch::tool_registry()
    }

    #[test]
    fn merged_registry_places_local_tools_first_then_upstream_verbatim() {
        let upstream = FakeUpstream::new(vec![json!({
            "name": "get_design_context",
            "description": "Design context for a node",
            "inputSchema": {"type": "object"},
        })]);
        let (tools, dropped) = proxy::merge_registry(local_registry(), upstream.tools());
        assert!(dropped.is_empty());
        assert_eq!(tools.len(), 18);
        assert!(tools[..17].iter().all(|t| t.name.starts_with("figmog_")));
        assert_eq!(tools[17].name, "get_design_context");
        assert!(tools[17].description.starts_with("[via Figma desktop] "));
    }

    #[test]
    fn routing_local_name_never_reaches_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let mut st = crate::open_store!(&db);
        let flattened = crate::flatten::flatten_file(&json!({
            "name": "F", "version": "1", "lastModified": "t",
            "document": {"id": "0:0", "name": "Document", "type": "DOCUMENT", "children": []},
            "components": {}, "componentSets": {}, "styles": {},
        }))
        .unwrap();
        store::sync(&mut st, &BTreeSet::new(), &flattened, 0);

        let result =
            st.rtx(|r| dispatch::dispatch_read_tool("figmog_status", &json!({}), "connected", r));
        let value = result
            .expect("figmog_status is a recognized local tool")
            .unwrap();
        assert_eq!(value["upstream"], json!("connected"));

        // A non-figmog_ name is simply not recognized by the local
        // dispatcher — proving the namespace rule routes it away from the
        // local path without needing a live upstream to demonstrate it.
        let result =
            st.rtx(|r| dispatch::dispatch_read_tool("get_code", &json!({}), "connected", r));
        assert!(result.is_none());
    }
}
