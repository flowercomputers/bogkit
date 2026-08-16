//! Command-line surface. Read commands never touch the network: they open
//! the local store and read one snapshot.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use fold::pipeline::terminal::{InvertedIndexReader, MultimapReader, TableReader};
use fold::stream::Readable;

use crate::api::{ApiError, FigmaApi, UreqApi};
use crate::flatten::flatten_file;
use crate::ident::parse_file_ref;
use crate::model::{
    ComponentRec, ComponentSetRec, FileMeta, Id, NodeRec, StyleRec, VariableCollectionRec,
    VariableRec,
};
use crate::query::{self, TextReader};
use crate::store::{Churn, collect_sweepable, sync};
use crate::watch::{BACKOFF_CAP, BACKOFF_START, Tick, Watcher};

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
    /// MCP stdio server: `figmog_*` tools over the local mirror, with the
    /// sync loop built in (one process owns the store).
    Serve {
        /// File key or figma.com URL. Optional after the first pull, or
        /// with `--no-watch` and `--db` for a read-only, offline server.
        file: Option<String>,
        /// Poll interval in seconds.
        #[arg(long, default_value = "10")]
        interval: u64,
        /// Disable the poll loop (offline/fixture use).
        #[arg(long)]
        no_watch: bool,
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
    /// Node counts by type and page, table totals, text-node count, max tree depth.
    Stats,
    /// Ancestor chain root→node for one id.
    Path { id: String },
    /// Every TEXT node's (id, characters, page_id), optionally scoped to one page.
    Text {
        #[arg(long)]
        page: Option<String>,
    },
    /// Nodes whose raw JSON matches an RFC 6901 pointer, optionally by value.
    Where {
        /// RFC 6901 pointer into the node's raw JSON, e.g. /layoutMode.
        #[arg(long)]
        pointer: String,
        /// JSON value to match; parsed as JSON, falling back to a bare string.
        #[arg(long)]
        equals: Option<String>,
        #[arg(long)]
        page: Option<String>,
    },
    /// Nodes whose absolute bounds contain a point, sorted by area ascending.
    At {
        #[arg(long)]
        x: f64,
        #[arg(long)]
        y: f64,
    },
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
        Cmd::Serve {
            file,
            interval,
            no_watch,
        } => crate::serve::run_serve(&db, file, interval, no_watch),
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
                Cmd::Stats => st.rtx(
                    |(
                        (nodes, _, _, _, _, _, by_type),
                        components,
                        component_sets,
                        styles,
                        variables,
                        ..,
                    )| {
                        cmd_stats(
                            &nodes,
                            &components,
                            &component_sets,
                            &styles,
                            &variables,
                            &by_type,
                            json,
                        )
                    },
                ),
                Cmd::Path { id } => st.rtx(|((nodes, ..), ..)| cmd_path(&nodes, id, json)),
                Cmd::Text { page } => st.rtx(|((nodes, _, _, _, _, _, by_type), ..)| {
                    cmd_text(&nodes, &by_type, page, json)
                }),
                Cmd::Where {
                    pointer,
                    equals,
                    page,
                } => st.rtx(|((nodes, ..), ..)| cmd_where(&nodes, pointer, equals, page, json)),
                Cmd::At { x, y } => st.rtx(|((nodes, ..), ..)| cmd_at(&nodes, x, y, json)),
                Cmd::Pull { .. }
                | Cmd::Watch { .. }
                | Cmd::ImportVariables { .. }
                | Cmd::Serve { .. } => {
                    unreachable!("handled above")
                }
            }
        }
    }
}

// ---- config / db resolution ----

/// The store to open plus (when known) the file key it mirrors.
pub(crate) struct Db {
    pub(crate) path: PathBuf,
    pub(crate) key: Option<String>,
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
    if let Cmd::Pull { file: Some(f), .. }
    | Cmd::Watch { file: Some(f), .. }
    | Cmd::Serve { file: Some(f), .. } = &cli.cmd
    {
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

pub(crate) fn write_current(key: &str) -> Result<(), String> {
    std::fs::create_dir_all(".figmog").map_err(|e| e.to_string())?;
    std::fs::write(CURRENT_FILE, key).map_err(|e| e.to_string())
}

pub(crate) fn now_ms() -> u64 {
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
pub(crate) enum PullError {
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
pub(crate) fn do_pull(
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
pub(crate) fn pull_failure_wait(
    err: &PullError,
    backoff: &mut Duration,
    interval: Duration,
) -> Duration {
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

pub(crate) fn read_watermark(db: &Db) -> Option<String> {
    let st = crate::open_store!(&db.path);
    st.rtx(|(_, _, _, _, _, _, meta)| meta.get(&0).map(|m| m.last_modified))
}

// ---- core reads ----

fn cmd_status<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    meta: &TableReader<'_, R, u8, FileMeta>,
    json: bool,
) -> Result<(), String> {
    let v = query::status(nodes, meta)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        println!(
            "{} v{} — {} nodes (last modified {})",
            v["name"].as_str().unwrap_or_default(),
            v["version"].as_str().unwrap_or_default(),
            v["nodes"].as_u64().unwrap_or_default(),
            v["last_modified"].as_str().unwrap_or_default(),
        );
    }
    Ok(())
}

fn cmd_pages<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    by_type: &InvertedIndexReader<'_, R, String, String>,
    json: bool,
) -> Result<(), String> {
    let v = query::pages(nodes, by_type)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
            println!(
                "{}  {}",
                row["name"].as_str().unwrap_or_default(),
                row["id"].as_str().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

fn print_tree_human(t: &query::TreeNode, indent: usize) {
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
    if json {
        let v = query::tree(nodes, children, by_type, id, depth)?;
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        let t = query::tree_nodes(nodes, children, by_type, id, depth)?;
        print_tree_human(&t, 0);
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
    let value = query::node(nodes, children, id, with_children)?;
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
    let v = query::find(nodes, by_type, node_type, page)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
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

// ---- design-system reads ----

fn cmd_search<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    text: &TextReader<'_, R>,
    query: String,
    limit: usize,
    json: bool,
) -> Result<(), String> {
    let v = query::search(text, nodes, &query, limit)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
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

fn cmd_instances<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    instances_of: &InvertedIndexReader<'_, R, String, String>,
    components: &TableReader<'_, R, String, ComponentRec>,
    component_sets: &TableReader<'_, R, String, ComponentSetRec>,
    target: String,
    json: bool,
) -> Result<(), String> {
    let v = query::instances(nodes, components, component_sets, instances_of, &target)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
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
    let v = query::components(nodes, components, component_sets)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for s in v["sets"].as_array().into_iter().flatten() {
            println!(
                "{}  {} variants",
                s["name"].as_str().unwrap_or_default(),
                s["variants"].as_array().map(Vec::len).unwrap_or(0),
            );
        }
        for c in v["components"].as_array().into_iter().flatten() {
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
    let v = query::styles(nodes, styles, styled_by, style_type, values)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
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
    let v = query::uses(nodes, styled_by, bound_to, &id)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
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
    let v = query::vars(nodes, variables, variable_collections, id)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
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

// ---- whole-file structural queries ----

/// `--equals <json>`: parse as JSON, falling back to treating the bare word
/// as a JSON string (so `--equals VERTICAL` works without quoting).
fn parse_equals(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn cmd_stats<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    components: &TableReader<'_, R, String, ComponentRec>,
    component_sets: &TableReader<'_, R, String, ComponentSetRec>,
    styles: &TableReader<'_, R, String, StyleRec>,
    variables: &TableReader<'_, R, String, VariableRec>,
    by_type: &InvertedIndexReader<'_, R, String, String>,
    json: bool,
) -> Result<(), String> {
    let v = query::stats(
        nodes,
        components,
        component_sets,
        styles,
        variables,
        by_type,
    )?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        println!(
            "{} nodes, max depth {}, {} text nodes",
            v["by_type"]
                .as_object()
                .map(|m| m.values().filter_map(Value::as_u64).sum::<u64>())
                .unwrap_or_default(),
            v["max_depth"].as_u64().unwrap_or_default(),
            v["text_nodes"].as_u64().unwrap_or_default(),
        );
        println!(
            "totals: components={} component_sets={} styles={} variables={}",
            v["totals"]["components"].as_u64().unwrap_or_default(),
            v["totals"]["component_sets"].as_u64().unwrap_or_default(),
            v["totals"]["styles"].as_u64().unwrap_or_default(),
            v["totals"]["variables"].as_u64().unwrap_or_default(),
        );
        println!("by type:");
        for (t, n) in v["by_type"].as_object().into_iter().flatten() {
            println!("  {t}  {n}");
        }
        println!("by page:");
        for (p, n) in v["by_page"].as_object().into_iter().flatten() {
            println!("  {p}  {n}");
        }
    }
    Ok(())
}

fn cmd_path<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    id: String,
    json: bool,
) -> Result<(), String> {
    let v = query::path(nodes, id)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
            println!(
                "{}  [{}]  {}",
                row["id"].as_str().unwrap_or_default(),
                row["type"].as_str().unwrap_or_default(),
                row["name"].as_str().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

fn cmd_text<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    by_type: &InvertedIndexReader<'_, R, String, String>,
    page: Option<String>,
    json: bool,
) -> Result<(), String> {
    let v = query::text(nodes, by_type, page)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
            println!(
                "{}  ({})  {}",
                row["id"].as_str().unwrap_or_default(),
                row["page_id"].as_str().unwrap_or_default(),
                row["characters"].as_str().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

fn cmd_where<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    pointer: String,
    equals: Option<String>,
    page: Option<String>,
    json: bool,
) -> Result<(), String> {
    let equals = equals.as_deref().map(parse_equals);
    let v = query::where_(nodes, &pointer, equals, page)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
            println!(
                "{}  {}  ({})  {}",
                row["id"].as_str().unwrap_or_default(),
                row["name"].as_str().unwrap_or_default(),
                row["page_id"].as_str().unwrap_or_default(),
                row["value"],
            );
        }
    }
    Ok(())
}

fn cmd_at<R: Readable>(
    nodes: &TableReader<'_, R, String, NodeRec>,
    x: f64,
    y: f64,
    json: bool,
) -> Result<(), String> {
    let v = query::at(nodes, x, y)?;
    if json {
        println!("{}", serde_json::to_string(&v).map_err(|e| e.to_string())?);
    } else {
        for row in v.as_array().into_iter().flatten() {
            println!(
                "{}  {}  [{}]  area={}",
                row["id"].as_str().unwrap_or_default(),
                row["name"].as_str().unwrap_or_default(),
                row["type"].as_str().unwrap_or_default(),
                row["area"].as_f64().unwrap_or_default(),
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
