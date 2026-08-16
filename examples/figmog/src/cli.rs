//! Command-line surface. Read commands never touch the network: they open
//! the local store and read one snapshot.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use fold::pipeline::terminal::search::Bm25Reader;
use fold::pipeline::terminal::{InvertedIndexReader, MultimapReader, TableReader};
use fold::stream::Readable;

use crate::api::{ApiError, FigmaApi, UreqApi};
use crate::flatten::flatten_file;
use crate::ident::{normalize_node_id, parse_file_ref};
use crate::model::{
    ComponentRec, ComponentSetRec, FileMeta, Id, NodeRec, StyleRec, VariableCollectionRec,
    VariableRec,
};
use crate::store::{Churn, collect_sweepable, sync};
use crate::watch::{BACKOFF_CAP, BACKOFF_START, Tick, Watcher};

/// Read handle for the pipeline's `text` BM25 sink (its tokenizer type
/// param makes the full type unwieldy at every call site).
type TextReader<'tx, R> = Bm25Reader<'tx, R, String, fn(&str, &mut Vec<u8>)>;

#[derive(Parser)]
#[command(name = "figmog", about = "fold-backed local mirror of a Figma file")]
struct Cli {
    /// Emit machine-readable JSON on stdout.
    #[arg(long, global = true)]
    json: bool,
    /// Store directory (default: .figmog/<file-key>/db).
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fetch the file (or read a saved response) and sync the mirror.
    Pull {
        /// File key or figma.com URL. Optional after the first pull.
        file: Option<String>,
        /// Ingest a saved GET /v1/files/:key response instead of the network.
        #[arg(long)]
        from_file: Option<PathBuf>,
        /// Wipe the store and rebuild from scratch.
        #[arg(long)]
        fresh: bool,
    },
    /// Poll for changes and pull automatically.
    Watch {
        file: Option<String>,
        /// Poll interval in seconds.
        #[arg(long, default_value = "10")]
        interval: u64,
    },
    /// File name, version, last modified, node count.
    Status,
    /// List pages.
    Pages,
    /// Subtree outline (default: whole document).
    Tree {
        id: Option<String>,
        #[arg(long)]
        depth: Option<usize>,
    },
    /// Full raw JSON of one node.
    Get {
        id: String,
        #[arg(long)]
        children: bool,
    },
    /// Nodes by type, optionally within one page.
    Find {
        #[arg(long = "type")]
        node_type: String,
        #[arg(long)]
        page: Option<String>,
    },
    /// BM25 search over layer names and text content.
    Search {
        query: String,
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
    },
    /// Instances of a component (by node id, key, or name).
    Instances { target: String },
    /// Design-system inventory: sets, variant axes, standalone components.
    Components,
    /// Styles with usage counts; --values derives definitions from consumers.
    Styles {
        #[arg(long = "type")]
        style_type: Option<String>,
        #[arg(long)]
        values: bool,
    },
    /// Nodes using a style id or bound to a variable id.
    Uses { id: String },
    /// Variables: authoritative if imported, else inferred from bindings.
    Vars { id: Option<String> },
    /// Import a variables export (REST or plugin-console shape).
    ImportVariables { path: PathBuf },
}

/// Parse `argv`, dispatch, and return the process exit code (0 on success,
/// 1 with a one-line `figmog: <message>` on stderr otherwise).
pub fn run() -> i32 {
    let cli = Cli::parse();
    let json = cli.json;
    match dispatch(cli) {
        Ok(()) => 0,
        Err(e) => {
            if json {
                eprintln!("{}", json!({"error": e}));
            } else {
                eprintln!("figmog: {e}");
            }
            1
        }
    }
}

fn dispatch(cli: Cli) -> Result<(), String> {
    let db = resolve_db(&cli)?;
    match cli.cmd {
        Cmd::Pull {
            file,
            from_file,
            fresh,
        } => cmd_pull(&db, file, from_file, fresh, cli.json),
        Cmd::Watch { file, interval } => cmd_watch(&db, file, interval, cli.json),
        Cmd::ImportVariables { path } => cmd_import_variables(&db, path, cli.json),
        other => {
            // `open_store!`'s pipeline type contains fn items and can't be
            // named, so the store-reading dispatch below must live at this
            // concrete (non-generic) call site rather than in a helper `fn`
            // generic over `P: Push<..>` — `P::Reader<'tx, R>` would be an
            // opaque associated type there, and a tuple pattern can't
            // destructure an unconstrained associated type.
            let st = crate::open_store!(&db.path);
            let json = cli.json;
            match other {
                Cmd::Status => st.rtx(|((nodes, _, _, _, _, _, _), _, _, _, _, _, meta)| {
                    cmd_status(&nodes, &meta, json)
                }),
                Cmd::Pages => st
                    .rtx(|((nodes, _, _, _, _, _, by_type), ..)| cmd_pages(&nodes, &by_type, json)),
                Cmd::Tree { id, depth } => {
                    st.rtx(|((nodes, children, _, _, _, _, by_type), ..)| {
                        cmd_tree(&nodes, &children, &by_type, id, depth, json)
                    })
                }
                Cmd::Get {
                    id,
                    children: with_children,
                } => st.rtx(|((nodes, children, ..), ..)| {
                    cmd_get(&nodes, &children, id, with_children, json)
                }),
                Cmd::Find { node_type, page } => st.rtx(|((nodes, _, _, _, _, _, by_type), ..)| {
                    cmd_find(&nodes, &by_type, node_type, page, json)
                }),
                Cmd::Search { query, limit } => st.rtx(|((nodes, _, text, ..), ..)| {
                    cmd_search(&nodes, &text, query, limit, json)
                }),
                Cmd::Instances { target } => st.rtx(
                    |((nodes, _, _, instances_of, ..), components, component_sets, ..)| {
                        cmd_instances(
                            &nodes,
                            &instances_of,
                            &components,
                            &component_sets,
                            target,
                            json,
                        )
                    },
                ),
                Cmd::Components => st.rtx(|((nodes, ..), components, component_sets, ..)| {
                    cmd_components(&component_sets, &components, &nodes, json)
                }),
                Cmd::Styles { style_type, values } => {
                    st.rtx(|((nodes, _, _, _, styled_by, ..), _, _, styles, ..)| {
                        cmd_styles(&styles, &styled_by, &nodes, style_type, values, json)
                    })
                }
                Cmd::Uses { id } => st.rtx(|((nodes, _, _, _, styled_by, bound_to, _), ..)| {
                    cmd_uses(&nodes, &styled_by, &bound_to, id, json)
                }),
                Cmd::Vars { id } => st.rtx(
                    |((nodes, ..), _, _, _, variables, variable_collections, _)| {
                        cmd_vars(&nodes, &variables, &variable_collections, id, json)
                    },
                ),
                Cmd::Pull { .. } | Cmd::Watch { .. } | Cmd::ImportVariables { .. } => {
                    unreachable!("handled above")
                }
            }
        }
    }
}

// ---- config / db resolution ----

/// The store to open plus (when known) the file key it mirrors.
struct Db {
    path: PathBuf,
    key: Option<String>,
}

const CURRENT_FILE: &str = ".figmog/current";

fn resolve_db(cli: &Cli) -> Result<Db, String> {
    if let Some(path) = &cli.db {
        return Ok(Db {
            path: path.clone(),
            key: None,
        });
    }

    // pull/watch with an explicit file ref establish the key for this run.
    // `.figmog/current` is only written after a successful sync (see
    // `do_pull`), so a failed pull never repoints later commands.
    if let Cmd::Pull { file: Some(f), .. } | Cmd::Watch { file: Some(f), .. } = &cli.cmd {
        let key = parse_file_ref(f).ok_or_else(|| format!("not a Figma file key or URL: {f}"))?;
        return Ok(Db {
            path: db_path_for(&key),
            key: Some(key),
        });
    }

    let key = std::fs::read_to_string(CURRENT_FILE)
        .map_err(|_| no_mirror_msg(cli))?
        .trim()
        .to_string();
    if key.is_empty() {
        return Err(no_mirror_msg(cli));
    }
    Ok(Db {
        path: db_path_for(&key),
        key: Some(key),
    })
}

/// `pull --from-file` with neither a file ref nor an established key has
/// nothing to sync into — point the user at `--from-file`'s own
/// requirements rather than the generic "run pull first" message (which
/// would tell a user already running pull to run pull).
fn no_mirror_msg(cli: &Cli) -> String {
    if let Cmd::Pull {
        from_file: Some(_),
        file: None,
        ..
    } = &cli.cmd
    {
        "--from-file needs a target mirror: pass the file key/url too, or --db <path>".into()
    } else {
        "no mirror here — run `figmog pull <file-url>` first".into()
    }
}

fn db_path_for(key: &str) -> PathBuf {
    PathBuf::from(".figmog").join(key).join("db")
}

fn write_current(key: &str) -> Result<(), String> {
    std::fs::create_dir_all(".figmog").map_err(|e| e.to_string())?;
    std::fs::write(CURRENT_FILE, key).map_err(|e| e.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

// ---- engine commands ----

/// Errors from [`do_pull`]: either a typed API failure (so callers can act
/// on rate limits) or any other pull-mechanics failure. `Display` matches
/// the plain-string messages `do_pull` used to produce, so `cmd_pull`'s
/// user-facing errors are unchanged.
#[derive(Debug)]
enum PullError {
    Api(ApiError),
    Other(String),
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PullError::Api(e) => write!(f, "{e}"),
            PullError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl From<String> for PullError {
    fn from(s: String) -> Self {
        PullError::Other(s)
    }
}

impl From<ApiError> for PullError {
    fn from(e: ApiError) -> Self {
        PullError::Api(e)
    }
}

fn cmd_pull(
    db: &Db,
    file: Option<String>,
    from_file: Option<PathBuf>,
    fresh: bool,
    json: bool,
) -> Result<(), String> {
    let (churn, name, version) = do_pull(db, file, from_file, fresh).map_err(|e| e.to_string())?;
    print_churn(&churn, &name, &version, json)
}

/// The pull mechanics without any printing, so `cmd_watch` can format its
/// own per-tick event lines around the same churn. `.figmog/current` is
/// written only once the sync below has actually happened, so a failed
/// pull never repoints later commands at a nonexistent mirror.
fn do_pull(
    db: &Db,
    file: Option<String>,
    from_file: Option<PathBuf>,
    fresh: bool,
) -> Result<(Churn, String, String), PullError> {
    let resp: Value = match from_file {
        Some(path) => {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("parsing {}: {e}", path.display()))?
        }
        None => {
            let key = db
                .key
                .clone()
                .or_else(|| file.and_then(|f| parse_file_ref(&f)))
                .ok_or_else(|| "no file key: pass a file key or figma.com URL".to_string())?;
            let token = std::env::var("FIGMA_TOKEN")
                .map_err(|_| "FIGMA_TOKEN not set — required for network pulls".to_string())?;
            UreqApi::new(token).file(&key)?
        }
    };

    if fresh {
        std::fs::remove_dir_all(&db.path).ok();
    }

    let flattened = flatten_file(&resp).map_err(|e| e.to_string())?;

    let mut st = crate::open_store!(&db.path);
    let prior: BTreeSet<Id> = st.rtx(|((nodes, ..), components, component_sets, styles, ..)| {
        collect_sweepable(&nodes, &components, &component_sets, &styles)
    });
    let churn = sync(&mut st, &prior, &flattened, now_ms());

    if let Some(key) = &db.key {
        write_current(key)?;
    }

    Ok((
        churn,
        flattened.file.name.clone(),
        flattened.file.version.clone(),
    ))
}

fn print_churn(churn: &Churn, name: &str, version: &str, json: bool) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string(churn).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "synced {name} v{version}: +{} ~{} -{} (={} unchanged)",
            churn.added, churn.changed, churn.removed, churn.unchanged
        );
    }
    Ok(())
}

fn cmd_watch(db: &Db, file: Option<String>, interval: u64, json: bool) -> Result<(), String> {
    let key = db
        .key
        .clone()
        .or_else(|| file.and_then(|f| parse_file_ref(&f)))
        .ok_or_else(|| "no file key: pass a file key or figma.com URL".to_string())?;
    let token = std::env::var("FIGMA_TOKEN")
        .map_err(|_| "FIGMA_TOKEN not set — required for watch".to_string())?;
    let api = UreqApi::new(token);

    if read_watermark(db).is_none() {
        cmd_pull(db, Some(key.clone()), None, false, json)?;
    }

    let mut stored = read_watermark(db);
    let mut watcher = Watcher::new(stored.clone());
    let interval = Duration::from_secs(interval);
    // Backoff for Tier-1 pull failures, independent of the Watcher's own
    // Tier-3 meta-poll backoff — reset on any successful pull.
    let mut pull_backoff = BACKOFF_START;

    loop {
        match watcher.tick(&api, &key) {
            Tick::Unchanged => std::thread::sleep(interval),
            Tick::Wait { after } => {
                if json {
                    println!(
                        "{}",
                        json!({"event": "waiting", "seconds": after.as_secs()})
                    );
                } else {
                    println!("waiting {}s", after.as_secs());
                }
                std::thread::sleep(after);
            }
            Tick::Changed { .. } => {
                if json {
                    println!("{}", json!({"event": "changed"}));
                } else {
                    println!("changed → pulling…");
                }
                match do_pull(db, Some(key.clone()), None, false) {
                    Ok((churn, name, version)) => {
                        stored = read_watermark(db);
                        pull_backoff = BACKOFF_START;
                        if json {
                            let mut v = serde_json::to_value(&churn).unwrap_or_default();
                            if let Some(obj) = v.as_object_mut() {
                                obj.insert("event".to_string(), json!("pulled"));
                            }
                            println!("{v}");
                        } else {
                            println!(
                                "synced {name} v{version}: +{} ~{} -{} (={} unchanged)",
                                churn.added, churn.changed, churn.removed, churn.unchanged
                            );
                        }
                        std::thread::sleep(interval);
                    }
                    Err(e) => {
                        eprintln!("figmog: pull failed: {e}");
                        // Watcher already advanced its watermark; reset it to
                        // the last successfully-synced one so the same
                        // change is re-detected on the next tick.
                        watcher = Watcher::new(stored.clone());
                        let wait = pull_failure_wait(&e, &mut pull_backoff, interval);
                        if json {
                            println!("{}", json!({"event": "waiting", "seconds": wait.as_secs()}));
                        } else {
                            println!("waiting {}s", wait.as_secs());
                        }
                        std::thread::sleep(wait);
                    }
                }
            }
        }
    }
}

/// How long `cmd_watch` should sleep after a failed pull, and advance the
/// per-loop backoff state. `RateLimited` honors `Retry-After` (never less
/// than the normal poll interval); anything else gets the same exponential
/// backoff discipline the [`Watcher`] uses for Tier-3 meta failures.
fn pull_failure_wait(err: &PullError, backoff: &mut Duration, interval: Duration) -> Duration {
    if let PullError::Api(ApiError::RateLimited { retry_after }) = err {
        interval.max(*retry_after)
    } else {
        let wait = *backoff;
        *backoff = (*backoff * 2).min(BACKOFF_CAP);
        wait
    }
}

fn cmd_import_variables(db: &Db, path: PathBuf, json: bool) -> Result<(), String> {
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let v: Value =
        serde_json::from_str(&content).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    let recs = crate::vars::parse_variables_export(&v).map_err(|e| e.to_string())?;

    let mut st = crate::open_store!(&db.path);
    st.wtx(|tx| {
        for (id, rec) in &recs {
            tx.upsert(id, rec);
        }
    });

    let imported = recs
        .iter()
        .filter(|(id, _)| matches!(id, Id::Variable(_)))
        .count();
    if json {
        println!(
            "{}",
            serde_json::to_string(&json!({"imported": imported})).map_err(|e| e.to_string())?
        );
    } else {
        println!("imported {imported} variables");
    }
    Ok(())
}

fn read_watermark(db: &Db) -> Option<String> {
    let st = crate::open_store!(&db.path);
    st.rtx(|(_, _, _, _, _, _, meta)| meta.get(&0).map(|m| m.last_modified))
}

// ---- core reads ----

fn cmd_status<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    meta: &TableReader<'_, R, u8, FileMeta>,
    json: bool,
) -> Result<(), String> {
    let m = meta
        .get(&0)
        .ok_or_else(|| "no mirror here — run `figmog pull <file-url>` first".to_string())?;
    let count = nodes.iter().count();
    if json {
        let v = json!({
            "name": m.name,
            "version": m.version,
            "last_modified": m.last_modified,
            "synced_at_unix_ms": m.synced_at_unix_ms,
            "nodes": count,
        });
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        println!(
            "{} v{} — {count} nodes (last modified {})",
            m.name, m.version, m.last_modified
        );
    }
    Ok(())
}

fn cmd_pages<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    by_type: &InvertedIndexReader<'_, R, String, String>,
    json: bool,
) -> Result<(), String> {
    let mut ids = by_type.search(&"CANVAS".to_string());
    ids.sort();

    let mut pages: Vec<(u32, String, String)> = ids
        .into_iter()
        .filter_map(|id| nodes.get(&id).map(|n| (n.child_index, n.id, n.name)))
        .collect();
    pages.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));

    if json {
        let arr: Vec<Value> = pages
            .iter()
            .map(|(_, id, name)| json!({"id": id, "name": name}))
            .collect();
        println!(
            "{}",
            serde_json::to_string(&arr).map_err(|e| e.to_string())?
        );
    } else {
        for (_, id, name) in &pages {
            println!("{name}  {id}");
        }
    }
    Ok(())
}

/// One level of a `tree` outline; JSON shape `{id, name, type, children}`.
struct TreeNode {
    id: String,
    name: String,
    node_type: String,
    children: Vec<TreeNode>,
}

fn build_tree<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    children: &MultimapReader<'_, R, String, (u32, String)>,
    node: &NodeRec,
    depth: Option<usize>,
) -> TreeNode {
    let mut kids = Vec::new();
    if depth != Some(0) {
        let mut edges = children.get(&node.id);
        edges.sort();
        let next_depth = depth.map(|d| d - 1);
        for (_, child_id) in edges {
            if let Some(child) = nodes.get(&child_id) {
                kids.push(build_tree(nodes, children, &child, next_depth));
            }
        }
    }
    TreeNode {
        id: node.id.clone(),
        name: node.name.clone(),
        node_type: node.node_type.clone(),
        children: kids,
    }
}

fn tree_to_json(t: &TreeNode) -> Value {
    json!({
        "id": t.id,
        "name": t.name,
        "type": t.node_type,
        "children": t.children.iter().map(tree_to_json).collect::<Vec<_>>(),
    })
}

fn print_tree_human(t: &TreeNode, indent: usize) {
    println!(
        "{}{}  [{}]  {}",
        "  ".repeat(indent),
        t.name,
        t.node_type,
        t.id
    );
    for c in &t.children {
        print_tree_human(c, indent + 1);
    }
}

fn cmd_tree<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    children: &MultimapReader<'_, R, String, (u32, String)>,
    by_type: &InvertedIndexReader<'_, R, String, String>,
    id: Option<String>,
    depth: Option<usize>,
    json: bool,
) -> Result<(), String> {
    let start = match id {
        Some(raw) => normalize_node_id(&raw),
        None => {
            let mut docs = by_type.search(&"DOCUMENT".to_string());
            docs.sort();
            docs.into_iter()
                .next()
                .ok_or_else(|| "no DOCUMENT node in the mirror".to_string())?
        }
    };
    let root = nodes
        .get(&start)
        .ok_or_else(|| format!("no node {start} in the mirror"))?;
    let tree = build_tree(nodes, children, &root, depth);

    if json {
        println!(
            "{}",
            serde_json::to_string(&tree_to_json(&tree)).map_err(|e| e.to_string())?
        );
    } else {
        print_tree_human(&tree, 0);
    }
    Ok(())
}

fn cmd_get<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    children: &MultimapReader<'_, R, String, (u32, String)>,
    id: String,
    with_children: bool,
    _json: bool,
) -> Result<(), String> {
    let id = normalize_node_id(&id);
    let node = nodes
        .get(&id)
        .ok_or_else(|| format!("no node {id} in the mirror"))?;
    let mut value: Value = serde_json::from_str(&node.raw).map_err(|e| e.to_string())?;

    if with_children {
        let mut edges = children.get(&id);
        edges.sort();
        let kids: Vec<Value> = edges
            .into_iter()
            .filter_map(|(_, child_id)| {
                nodes
                    .get(&child_id)
                    .map(|n| json!({"id": n.id, "name": n.name, "type": n.node_type}))
            })
            .collect();
        if let Some(obj) = value.as_object_mut() {
            obj.insert("children".to_string(), Value::Array(kids));
        }
    }

    // Get's output is always JSON, whether or not --json was passed.
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn cmd_find<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    by_type: &InvertedIndexReader<'_, R, String, String>,
    node_type: String,
    page: Option<String>,
    json: bool,
) -> Result<(), String> {
    // Figma node types are stored uppercase; normalize so `--type frame`
    // matches the same as `--type FRAME`.
    let mut ids = by_type.search(&node_type.to_uppercase());
    ids.sort();
    let page = page.as_deref().map(normalize_node_id);

    let mut rows: Vec<(String, String, String)> = ids
        .into_iter()
        .filter_map(|id| nodes.get(&id))
        .filter(|n| page.as_deref().is_none_or(|p| n.page_id == p))
        .map(|n| (n.id, n.name, n.page_id))
        .collect();
    rows.sort();

    if json {
        let arr: Vec<Value> = rows
            .iter()
            .map(|(id, name, page_id)| json!({"id": id, "name": name, "page_id": page_id}))
            .collect();
        println!(
            "{}",
            serde_json::to_string(&arr).map_err(|e| e.to_string())?
        );
    } else {
        for (id, name, page_id) in &rows {
            println!("{id}  {name}  ({page_id})");
        }
    }
    Ok(())
}

// ---- design-system reads ----

fn cmd_search<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    text: &TextReader<'_, R>,
    query: String,
    limit: usize,
    json: bool,
) -> Result<(), String> {
    // BM25's own ranking order is deterministic; keep it (do not re-sort).
    let hits = text.search(&query, limit);
    let rows: Vec<Value> = hits
        .iter()
        .filter_map(|hit| {
            let node = nodes.get(&hit.val)?;
            let snippet = node
                .text
                .as_ref()
                .map(|t| t.chars().take(80).collect::<String>());
            Some(json!({
                "id": node.id,
                "score": hit.score,
                "type": node.node_type,
                "name": node.name,
                "page_id": node.page_id,
                "snippet": snippet,
            }))
        })
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(|e| e.to_string())?
        );
    } else {
        for row in &rows {
            println!(
                "{}  {:.3}  [{}]  {}",
                row["id"].as_str().unwrap_or_default(),
                row["score"].as_f64().unwrap_or_default(),
                row["type"].as_str().unwrap_or_default(),
                row["name"].as_str().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

/// Resolve a target (node id, component key, or component/set name) to the
/// component node ids it names, in priority order: exact node id, then key,
/// then set name (all variants), then component name (all matches).
fn resolve_component_ids<R: Readable>(
    components: &TableReader<'_, R, String, ComponentRec>,
    component_sets: &TableReader<'_, R, String, ComponentSetRec>,
    target: &str,
) -> Vec<String> {
    if components.contains(&target.to_string()) {
        return vec![target.to_string()];
    }

    let mut ids: Vec<String> = components
        .iter()
        .filter(|(_, c)| c.key == target)
        .map(|(id, _)| id)
        .collect();
    if !ids.is_empty() {
        return ids;
    }

    let set_ids: Vec<String> = component_sets
        .iter()
        .filter(|(_, s)| s.name == target)
        .map(|(id, _)| id)
        .collect();
    if !set_ids.is_empty() {
        ids = components
            .iter()
            .filter(|(_, c)| {
                c.component_set_id
                    .as_deref()
                    .is_some_and(|s| set_ids.iter().any(|sid| sid == s))
            })
            .map(|(id, _)| id)
            .collect();
        return ids;
    }

    components
        .iter()
        .filter(|(_, c)| c.name == target)
        .map(|(id, _)| id)
        .collect()
}

fn cmd_instances<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    instances_of: &InvertedIndexReader<'_, R, String, String>,
    components: &TableReader<'_, R, String, ComponentRec>,
    component_sets: &TableReader<'_, R, String, ComponentSetRec>,
    target: String,
    json: bool,
) -> Result<(), String> {
    let target = normalize_node_id(&target);
    let component_ids = resolve_component_ids(components, component_sets, &target);

    let mut instance_ids: BTreeSet<String> = BTreeSet::new();
    for cid in &component_ids {
        instance_ids.extend(instances_of.search(cid));
    }

    let rows: Vec<Value> = instance_ids
        .iter()
        .filter_map(|id| nodes.get(id))
        .map(|n| json!({"id": n.id, "name": n.name, "page_id": n.page_id, "component_id": n.component_id}))
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(|e| e.to_string())?
        );
    } else {
        for row in &rows {
            println!(
                "{}  {}  ({})",
                row["id"].as_str().unwrap_or_default(),
                row["name"].as_str().unwrap_or_default(),
                row["page_id"].as_str().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

fn cmd_components<R: Readable>(
    component_sets: &TableReader<'_, R, String, ComponentSetRec>,
    components: &TableReader<'_, R, String, ComponentRec>,
    nodes: &TableReader<'_, R, String, NodeRec>,
    json: bool,
) -> Result<(), String> {
    let mut sets: Vec<(String, ComponentSetRec)> = component_sets.iter().collect();
    sets.sort_by(|a, b| a.0.cmp(&b.0));

    let mut all_components: Vec<(String, ComponentRec)> = components.iter().collect();
    all_components.sort_by(|a, b| a.0.cmp(&b.0));

    let sets_json: Vec<Value> = sets
        .iter()
        .map(|(set_id, set)| {
            let variants: Vec<Value> = all_components
                .iter()
                .filter(|(_, c)| c.component_set_id.as_deref() == Some(set_id.as_str()))
                .map(|(cid, c)| json!({"node_id": cid, "name": c.name, "key": c.key}))
                .collect();
            let property_definitions: Value = nodes
                .get(set_id)
                .and_then(|n| n.property_definitions.clone())
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null);
            json!({
                "node_id": set_id,
                "name": set.name,
                "key": set.key,
                "variants": variants,
                "property_definitions": property_definitions,
            })
        })
        .collect();

    let standalone: Vec<Value> = all_components
        .iter()
        .filter(|(_, c)| c.component_set_id.is_none())
        .map(|(cid, c)| json!({"node_id": cid, "name": c.name, "key": c.key}))
        .collect();

    let out = json!({"sets": sets_json, "components": standalone});

    if json {
        println!(
            "{}",
            serde_json::to_string(&out).map_err(|e| e.to_string())?
        );
    } else {
        for s in &sets_json {
            println!(
                "{}  {} variants",
                s["name"].as_str().unwrap_or_default(),
                s["variants"].as_array().map(Vec::len).unwrap_or(0),
            );
        }
        for c in &standalone {
            println!(
                "{}  {}",
                c["node_id"].as_str().unwrap_or_default(),
                c["name"].as_str().unwrap_or_default()
            );
        }
    }
    Ok(())
}

fn cmd_styles<R: Readable>(
    styles: &TableReader<'_, R, String, StyleRec>,
    styled_by: &InvertedIndexReader<'_, R, String, String>,
    nodes: &TableReader<'_, R, String, NodeRec>,
    style_type: Option<String>,
    values: bool,
    json: bool,
) -> Result<(), String> {
    let mut rows: Vec<(String, StyleRec)> = styles.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some(t) = &style_type {
        rows.retain(|(_, s)| s.style_type.eq_ignore_ascii_case(t));
    }

    let out: Vec<Value> = rows
        .iter()
        .map(|(style_id, s)| {
            let mut consumers = styled_by.search(style_id);
            consumers.sort();
            let mut obj = json!({
                "style_id": style_id,
                "name": s.name,
                "key": s.key,
                "type": s.style_type,
                "uses": consumers.len(),
            });
            if values {
                let value = consumers
                    .first()
                    .and_then(|nid| nodes.get(nid))
                    .and_then(|n| crate::vars::style_value_from_consumer(&s.style_type, &n.raw))
                    .unwrap_or(Value::Null);
                obj["value"] = value;
            }
            obj
        })
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string(&out).map_err(|e| e.to_string())?
        );
    } else {
        for row in &out {
            println!(
                "{}  {}  [{}]  uses={}",
                row["style_id"].as_str().unwrap_or_default(),
                row["name"].as_str().unwrap_or_default(),
                row["type"].as_str().unwrap_or_default(),
                row["uses"].as_u64().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

fn cmd_uses<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    styled_by: &InvertedIndexReader<'_, R, String, String>,
    bound_to: &InvertedIndexReader<'_, R, String, String>,
    id: String,
    json: bool,
) -> Result<(), String> {
    let mut ids = styled_by.search(&id);
    if ids.is_empty() {
        ids = bound_to.search(&id);
    }
    ids.sort();

    let rows: Vec<Value> = ids
        .iter()
        .filter_map(|nid| nodes.get(nid))
        .map(|n| json!({"id": n.id, "name": n.name, "page_id": n.page_id}))
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(|e| e.to_string())?
        );
    } else {
        for row in &rows {
            println!(
                "{}  {}  ({})",
                row["id"].as_str().unwrap_or_default(),
                row["name"].as_str().unwrap_or_default(),
                row["page_id"].as_str().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

fn cmd_vars<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    variables: &TableReader<'_, R, String, VariableRec>,
    variable_collections: &TableReader<'_, R, String, VariableCollectionRec>,
    id: Option<String>,
    json: bool,
) -> Result<(), String> {
    let owned_nodes: Vec<NodeRec> = nodes.iter().map(|(_, n)| n).collect();
    let inferred = crate::vars::infer_from_nodes(owned_nodes.iter());
    let mut inferred_by_id: HashMap<String, crate::vars::VarUsage> = inferred
        .into_iter()
        .map(|u| (u.variable_id.clone(), u))
        .collect();

    let mut all_ids: BTreeSet<String> = inferred_by_id.keys().cloned().collect();
    all_ids.extend(variables.iter().map(|(k, _)| k));
    if let Some(target) = &id {
        all_ids.retain(|v| v == target);
    }

    let rows: Vec<Value> = all_ids
        .iter()
        .map(|vid| {
            let usage = inferred_by_id.remove(vid);
            let (sites, observed) = usage.map(|u| (u.sites, u.observed)).unwrap_or_default();

            if let Some(var) = variables.get(vid) {
                let collection = variable_collections.get(&var.collection_id);
                let mut values_by_mode = serde_json::Map::new();
                for (mode_id, val_str) in &var.values_by_mode {
                    let mode_name = collection
                        .as_ref()
                        .and_then(|c| c.modes.iter().find(|(mid, _)| mid == mode_id))
                        .map(|(_, name)| name.clone())
                        .unwrap_or_else(|| mode_id.clone());
                    let val: Value = serde_json::from_str(val_str).unwrap_or(Value::Null);
                    values_by_mode.insert(mode_name, val);
                }
                json!({
                    "variable_id": vid,
                    "source": "imported",
                    "name": var.name,
                    "resolved_type": var.resolved_type,
                    "collection": collection.map(|c| c.name),
                    "values_by_mode": Value::Object(values_by_mode),
                    "sites": sites,
                    "observed": observed,
                })
            } else {
                json!({
                    "variable_id": vid,
                    "source": "inferred",
                    "sites": sites,
                    "observed": observed,
                })
            }
        })
        .collect();

    if json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(|e| e.to_string())?
        );
    } else {
        for row in &rows {
            println!(
                "{}  [{}]  sites={}",
                row["variable_id"].as_str().unwrap_or_default(),
                row["source"].as_str().unwrap_or_default(),
                row["sites"].as_array().map(Vec::len).unwrap_or(0),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_waits_max_of_interval_and_retry_after() {
        let mut backoff = BACKOFF_START;
        let err = PullError::Api(ApiError::RateLimited {
            retry_after: Duration::from_secs(90),
        });
        // retry_after exceeds interval: use retry_after.
        let wait = pull_failure_wait(&err, &mut backoff, Duration::from_secs(10));
        assert_eq!(wait, Duration::from_secs(90));
        // rate-limit waits don't consume the exponential-backoff budget.
        assert_eq!(backoff, BACKOFF_START);

        let err = PullError::Api(ApiError::RateLimited {
            retry_after: Duration::from_secs(3),
        });
        let wait = pull_failure_wait(&err, &mut backoff, Duration::from_secs(10));
        assert_eq!(wait, Duration::from_secs(10));
    }

    #[test]
    fn other_errors_back_off_exponentially_and_cap() {
        let mut backoff = BACKOFF_START;
        let interval = Duration::from_secs(10);
        let net_err = PullError::Api(ApiError::Network("down".into()));

        let w1 = pull_failure_wait(&net_err, &mut backoff, interval);
        assert_eq!(w1, Duration::from_secs(5));
        let w2 = pull_failure_wait(&net_err, &mut backoff, interval);
        assert_eq!(w2, Duration::from_secs(10));
        let w3 = pull_failure_wait(&net_err, &mut backoff, interval);
        assert_eq!(w3, Duration::from_secs(20));

        // non-Api errors (e.g. flatten failures) get the same treatment.
        let other_err = PullError::Other("bad shape".into());
        let mut backoff2 = BACKOFF_CAP / 2 + Duration::from_secs(1);
        let w = pull_failure_wait(&other_err, &mut backoff2, interval);
        assert!(w <= BACKOFF_CAP);
        assert_eq!(backoff2, BACKOFF_CAP);
    }

    #[test]
    fn pull_error_display_matches_prior_stringified_messages() {
        let e = PullError::Other("FIGMA_TOKEN not set — required for network pulls".into());
        assert_eq!(
            e.to_string(),
            "FIGMA_TOKEN not set — required for network pulls"
        );

        let e = PullError::Api(ApiError::RateLimited {
            retry_after: Duration::from_secs(30),
        });
        assert_eq!(
            e.to_string(),
            ApiError::RateLimited {
                retry_after: Duration::from_secs(30)
            }
            .to_string()
        );
    }
}
