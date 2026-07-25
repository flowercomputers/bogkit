//! séance — search any repo at any point in its history.
//!
//! A repo's history is ingested as deltas: each commit's diff becomes
//! upserts (added/modified files) and removals (deleted files) on a
//! `KeyedStream<path, Doc>`, where a `Doc` carries the (capped) text plus
//! its ese embedding, batch-computed across all cores before the
//! transaction. Fold maintains every view incrementally, so the
//! materialized indexes exist *at whatever commit the cursor points to* —
//! and moving the cursor (forward or backward!) costs one transaction
//! proportional to the diff, never a reindex. Retraction is what makes
//! scrubbing backward identical to scrubbing forward, and short jumps are
//! walked commit-by-commit so the scrub streams a real, searchable view of
//! every intermediate state.
//!
//! Each `Keyed { key: path, val: Doc }` delta fans out to:
//!   - a BM25 full-text index (`terminal::search::Bm25`)
//!   - an HNSW semantic index over ese embeddings (`terminal::search::Hnsw`)
//!   - a path -> content table for snippets (`terminal::Table`)
//!   - a live file count (`terminal::Count`)
//!   - per-extension file counts (`Aggregate` -> `Table`) — the repo's
//!     language shape, morphing as you scrub
//!   - total line count (`terminal::Stats`)
//!
//! Serving follows the chat example: one plain thread owns the stream and
//! does all writes; commands arrive on an mpsc, snapshots leave on a tokio
//! watch, and every websocket client just mirrors the latest snapshot.
//!
//! Run:
//!   cargo run -p seance -- [repo-path] [max-commits]
//! then open http://localhost:3333 and scrub. Or headless:
//!   cargo run -p seance -- [repo-path] --probe "query"
//! which materializes HEAD, rewinds to the first commit, returns to HEAD,
//! and prints one JSON view per stop on stdout.

mod chronicle;
mod git;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anny::metric::Cosine;
use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Html,
    routing::get,
};
use fold::pipeline::{Aggregate, KeyBy, Keyed, Map, Push, Scored, terminal};
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

const DIM: usize = ese::DIMENSIONS;

/// Reciprocal-rank-fusion constant (value from the original RRF paper).
const RRF_K: f64 = 60.0;
/// Embeddings see at most this many bytes of a file. Deterministic
/// truncation keeps the map a pure function, which retraction requires.
const EMBED_CAP_BYTES: usize = 8 * 1024;
/// Indexed content is capped at this many bytes: BM25 tokenization cost is
/// proportional to bytes ingested, and repos like sqlite carry multi-MB
/// text files whose heads carry nearly all the searchable signal. Applied
/// before the record enters the stream, so every view (postings, lines,
/// snippets) sees the same capped text and retraction cancels exactly.
const INDEX_CAP_BYTES: usize = 64 * 1024;
/// Blobs larger than this are not indexed at all — their size is checked
/// via `cat-file --batch-check` before contents are ever transported, so a
/// 17MB fuzz corpus costs one header line instead of 17MB down a pipe.
const MAX_BLOB_BYTES: usize = 1024 * 1024;
const RESULT_LIMIT: usize = 12;
const SNIPPET_BYTES: usize = 160;

#[derive(Debug, Clone)]
enum Cmd {
    Goto(usize),
    Query(String),
}

/// One indexed file version. The embedding is computed *outside* the
/// pipeline (batched across all cores via ese's rayon feature) and stored
/// in the record: `KeyedStream` reproduces the stored record on removal,
/// so retraction never re-embeds anything and determinism is structural
/// rather than relying on the embedder being called twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Doc {
    text: String,
    vec: Vec<f32>,
}

impl AsRef<str> for Doc {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

fn vec_to_dim(v: &[f32]) -> [f32; DIM] {
    std::array::from_fn(|i| v[i])
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Hit {
    path: String,
    score: f64,
    snippet: String,
}

/// One consistent snapshot of every view at the cursor, as sent to clients.
#[derive(Debug, Clone, Default, Serialize)]
struct View {
    cursor: usize,
    oid: String,
    subject: String,
    author: String,
    at: i64,
    files: i64,
    lines: i64,
    by_ext: Vec<(String, i64)>,
    query: String,
    keyword: Vec<Hit>,
    semantic: Vec<Hit>,
    hybrid: Vec<Hit>,
    /// how long the last jump took and how many paths it touched — the
    /// "only the diff" story, on screen
    apply_ms: u64,
    changed: usize,
    /// true for chronicle-sourced previews published while the store is
    /// still converging: header fields reflect the target, search results
    /// don't exist yet
    transit: bool,
}

#[derive(Serialize)]
struct Hello {
    repo: String,
    total: usize,
}

#[derive(Deserialize)]
struct ClientMsg {
    goto: Option<usize>,
    query: Option<String>,
}

/// Deterministically truncate to a char boundary at most `max` bytes in.
fn cap(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Embed at most the head of a file. Pure function of the content — fold's
/// determinism contract — so a retraction re-embeds to exactly the vector
/// that was inserted.
fn embed(text: &str) -> [f32; DIM] {
    ese::encode_single(cap(text, EMBED_CAP_BYTES))
}

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_else(|| "(none)".to_string())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// The first line matching a query term (else the first nonempty line),
/// trimmed for display.
fn snippet(content: &str, query: &str) -> String {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();
    let hit = content
        .lines()
        .find(|l| {
            let l = l.to_lowercase();
            !terms.is_empty() && terms.iter().any(|t| l.contains(t.as_str()))
        })
        .or_else(|| content.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("");
    truncate(hit.trim(), SNIPPET_BYTES)
}

/// Fuse the BM25 and HNSW hit lists by reciprocal rank; the two scores live
/// on incomparable scales (relevance vs cosine distance).
fn rrf(keyword: &[Scored<f64, String>], semantic: &[Scored<f32, String>]) -> Vec<(String, f64)> {
    let mut fused: HashMap<String, f64> = HashMap::new();
    for (rank, hit) in keyword.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    for (rank, hit) in semantic.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    let mut fused: Vec<(String, f64)> = fused.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    fused
}

/// The pipeline. A macro because its type contains closures and can't be
/// written down (same trick as the other examples).
macro_rules! pipeline {
    ($dir:expr) => {
        (
            terminal::search::Bm25::<String, Doc>::new("text"),
            Map::new(
                |d: &Keyed<String, Doc>| Keyed::new(d.key.clone(), vec_to_dim(&d.val.vec)),
                terminal::search::Hnsw::<String, f32, Cosine, DIM, 16, 10, 40, 32, 16>::new("vecs", Cosine, 42)
                    .with_graph_snapshot($dir.join("hnsw.graph")),
            ),
            terminal::Table::<String, Doc>::new("files"),
            terminal::Count::new("file_count"),
            Map::new(
                |d: &Keyed<String, Doc>| ext_of(&d.key),
                KeyBy::new(
                    |e: &String| e.clone(),
                    Aggregate::new(
                        "by_ext_agg",
                        |acc: &mut i64, _e: &String, delta| *acc += delta as i64,
                        terminal::Table::<String, i64>::new("by_ext"),
                    ),
                ),
            ),
            Map::new(
                |d: &Keyed<String, Doc>| d.val.text.lines().count() as i64,
                terminal::Stats::new("lines", |n: &i64| *n as f64),
            ),
        )
    };
}

/// Read one consistent snapshot into a `View` (macro: the reader tuple type
/// contains closures too).
macro_rules! snapshot {
    ($st:expr, $commits:expr, $cursor:expr, $query:expr, $apply:expr) => {
        $st.rtx(|(bm25, vecs, files, count, by_ext, lines)| {
            let query: &str = $query.as_ref();
            let (mut keyword, semantic) = if query.trim().is_empty() {
                (Vec::new(), Vec::new())
            } else {
                (bm25.search(query, 20), vecs.search(&embed(query)))
            };
            // bm25 leaves equal-scored ties in hash order; pin them down
            keyword.sort_by(|a: &Scored<f64, String>, b| {
                b.score.total_cmp(&a.score).then(a.val.cmp(&b.val))
            });
            let hybrid = rrf(&keyword, &semantic);
            let snip = |path: &String| {
                files
                    .get(path)
                    .map(|d: Doc| snippet(&d.text, query))
                    .unwrap_or_default()
            };

            let mut exts: Vec<(String, i64)> = by_ext.iter().collect();
            exts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            exts.truncate(12);

            let c = &$commits[$cursor];
            View {
                cursor: $cursor,
                oid: c.oid.get(..10).unwrap_or(&c.oid).to_string(),
                subject: c.subject.clone(),
                author: c.author.clone(),
                at: c.at,
                files: count.get(),
                lines: lines.sum() as i64,
                by_ext: exts,
                query: query.to_string(),
                keyword: keyword
                    .iter()
                    .take(RESULT_LIMIT)
                    .map(|h| Hit {
                        path: h.val.clone(),
                        score: h.score,
                        snippet: snip(&h.val),
                    })
                    .collect(),
                semantic: semantic
                    .iter()
                    .take(RESULT_LIMIT)
                    .map(|h| Hit {
                        path: h.val.clone(),
                        score: h.score as f64,
                        snippet: snip(&h.val),
                    })
                    .collect(),
                hybrid: hybrid
                    .into_iter()
                    .take(RESULT_LIMIT)
                    .map(|(path, score)| Hit {
                        snippet: snip(&path),
                        path,
                        score,
                    })
                    .collect(),
                apply_ms: $apply.0,
                changed: $apply.1,
                transit: false,
            }
        })
    };
}

/// Move the cursor to `target` by applying `git diff-tree` between the two
/// trees as one atomic transaction: upserts re-index, deletes retract.
/// Forward and backward are the same operation.
///
/// The transaction runs in two passes with a mid-tx flush between them:
/// first every changed path is retracted, then new contents are inserted.
/// This matters for the posting sinks (`Bm25`): their set-semantic flush
/// writes a posting's *accumulated* delta, so retracting one version of a
/// document and inserting another in the same flush would fold shared
/// terms to `tf_new - tf_old` instead of `tf_new`. Flushing between the
/// passes (via an empty [`fold::stream::Tx::rtx`]) makes each pass
/// all-retraction or all-insertion — each folds correctly — while the jump
/// still commits as one atomic transaction.
fn goto<P: Push<Keyed<String, Doc>>>(
    st: &mut KeyedStream<String, Doc, P>,
    blobs: &mut git::Blobs,
    repo: &Path,
    empty_tree: &str,
    commits: &[git::Commit],
    cursor: &mut Option<usize>,
    target: usize,
    // prefetched changed-path set for this jump (walk steps); statuses are
    // unknown, but `blob_size` decides existence at the target anyway, so
    // every cached path is simply tried as an upsert
    cached: Option<&Vec<String>>,
    // polled between phases on large jumps; returning true abandons the
    // jump before anything is written (cursor and views untouched)
    interrupt: &mut dyn FnMut() -> bool,
) -> Option<(u64, usize)> {
    use std::time::Instant;
    let started = Instant::now();
    let from = match *cursor {
        Some(i) => commits[i].oid.as_str(),
        None => empty_tree,
    };
    let to = commits[target].oid.as_str();
    let changes: Vec<(git::Status, String)> = match cached {
        Some(paths) => paths
            .iter()
            .map(|p| (git::Status::Upsert, p.clone()))
            .collect(),
        None => git::diff(repo, from, to),
    };
    let t_diff = started.elapsed();
    let interruptible = changes.len() > INTERRUPT_MIN_PATHS;
    let abandon = |phase: &str| {
        eprintln!(
            "seance: jump to {} abandoned at {phase} (target moved)",
            &to[..to.len().min(10)]
        );
    };
    if interruptible && interrupt() {
        abandon("diff");
        return None;
    }

    // phase: sizes — skip oversized blobs before their bytes ever move
    let t = Instant::now();
    let mut to_read: Vec<&String> = Vec::new();
    for (status, path) in &changes {
        if let git::Status::Upsert = status {
            if let Some(size) = blobs.blob_size(&format!("{to}:{path}")) {
                if size <= MAX_BLOB_BYTES {
                    to_read.push(path);
                }
            }
        }
    }
    let t_stat = t.elapsed();

    // phase: read — transport contents, drop binaries, cap what we index
    let t = Instant::now();
    let mut docs: Vec<(String, String)> = Vec::with_capacity(to_read.len());
    for path in to_read {
        if let Some(bytes) = blobs.read(&format!("{to}:{path}")) {
            if !bytes.contains(&0) {
                let content = String::from_utf8_lossy(&bytes);
                docs.push((path.clone(), cap(&content, INDEX_CAP_BYTES).to_string()));
            }
        }
    }
    let t_read = t.elapsed();
    if interruptible && interrupt() {
        abandon("read");
        return None;
    }

    // phase: embed — one rayon-parallel batch across every core
    let t = Instant::now();
    let vecs = ese::encode(docs.iter().map(|(_, text)| cap(text, EMBED_CAP_BYTES)));
    let t_embed = t.elapsed();
    if interruptible && interrupt() {
        abandon("embed");
        return None;
    }

    // phase: tx — one retraction transaction, then inserts in bounded
    // chunks. Separate transactions keep Bm25's set-semantic folds pure
    // (retract-only, then insert-only — the same property the old mid-tx
    // flush provided), and bounded chunks keep journal bytes per commit
    // small so rotation and flushing keep pace: reopening a store (or a
    // checkpoint clone of it) never replays a mega-transaction.
    const TX_CHUNK_DOCS: usize = 400;
    let t = Instant::now();
    st.wtx(|tx| {
        for (_, path) in &changes {
            tx.remove(path);
        }
    });
    let mut items: Vec<(String, Doc)> = docs
        .into_iter()
        .zip(vecs)
        .map(|((path, text), vec)| {
            (
                path,
                Doc {
                    text,
                    vec: vec.to_vec(),
                },
            )
        })
        .collect();
    while !items.is_empty() {
        let take = items.len().min(TX_CHUNK_DOCS);
        let batch: Vec<(String, Doc)> = items.drain(..take).collect();
        st.wtx(|tx| {
            for (path, doc) in &batch {
                tx.upsert(path, doc);
            }
        });
    }
    let t_tx = t.elapsed();
    *cursor = Some(target);

    let ms = started.elapsed().as_millis() as u64;
    eprintln!(
        "seance: {} -> {} ({} paths, {} ms: diff {} · stat {} · read {} · embed {} · tx {})",
        &from[..from.len().min(10)],
        &to[..to.len().min(10)],
        changes.len(),
        ms,
        t_diff.as_millis(),
        t_stat.as_millis(),
        t_read.as_millis(),
        t_embed.as_millis(),
        t_tx.as_millis(),
    );
    Some((ms, changes.len()))
}

fn repo_hash(repo: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    repo.hash(&mut h);
    h.finish()
}

fn db_path_for(repo: &Path) -> PathBuf {
    std::env::temp_dir().join(format!("bog-seance-{:016x}.db", repo_hash(repo)))
}

fn chron_path_for(repo: &Path) -> PathBuf {
    std::env::temp_dir().join(format!("bog-seance-{:016x}.chron.db", repo_hash(repo)))
}

/// Immutable checkpoint masters live here, one dir per commit oid. They
/// are never opened in place — every use clones first (opening a fjall dir
/// mutates it), so masters stay pristine.
fn ckpt_root_for(repo: &Path) -> PathBuf {
    std::env::temp_dir().join(format!("bog-seance-{:016x}.ckpts", repo_hash(repo)))
}

fn live_root_for(repo: &Path) -> PathBuf {
    std::env::temp_dir().join(format!("bog-seance-{:016x}.live", repo_hash(repo)))
}

/// Copy-on-write directory clone: APFS clonefile via `cp -c` (O(1) space
/// and near-O(1) time regardless of store size), plain copy as fallback.
fn clone_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    let cow = std::process::Command::new("cp")
        .arg("-cR")
        .arg(src)
        .arg(dst)
        .status()?
        .success();
    if cow {
        return Ok(());
    }
    let _ = std::fs::remove_dir_all(dst);
    let plain = std::process::Command::new("cp")
        .arg("-R")
        .arg(src)
        .arg(dst)
        .status()?
        .success();
    if plain {
        Ok(())
    } else {
        Err(std::io::Error::other("cp failed"))
    }
}

/// Write the HNSW graph snapshot beside the store so the next open of a
/// clone restores it at memory bandwidth instead of re-inserting every
/// vector. Must run between transactions (the in-memory graph then matches
/// committed rows exactly) and before [`create_checkpoint`]'s clone.
macro_rules! persist_graph {
    ($st:expr) => {
        if let Err(e) = $st.rtx(|(_, vecs, _, _, _, _)| vecs.save_graph()) {
            eprintln!("seance: graph snapshot failed: {e}");
        }
    };
}

/// Seal memtables and drain queued flush work so the on-disk state is
/// self-contained. Bounded wait so a long compaction can't stall callers.
fn quiesce(db: &fjall::SingleWriterTxDatabase) {
    for name in db.list_keyspace_names() {
        if let Ok(ks) = db.keyspace(&name, fjall::KeyspaceCreateOptions::default) {
            if let Err(e) = ks.inner().rotate_memtable_and_wait() {
                eprintln!("seance: memtable rotate failed for {name}: {e}");
            }
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while (db.inner().outstanding_flushes() > 0 || db.inner().active_compactions() > 0)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// One unit of work for the single wash worker: either a fresh clone to
/// normalize and promote to master, or a stale (pre-wash-era) master to
/// heal in place.
enum WashJob {
    Fresh { tmp: PathBuf, dst: PathBuf, idx: usize },
    Heal { dir: PathBuf, idx: usize },
}

/// Normalize a copied store so later opens skip journal replay.
///
/// Pass 1 replays the journals and flushes everything to tables. fjall's
/// own journal eviction then never fires here: it requires each watermark
/// keyspace's persisted seqno to reach the sealed journal's LSN, and in a
/// short-lived recovered store those donor-session seqnos never reconcile
/// (journal/manager.rs `maintenance`). So the sealed journals — whose
/// content is provably in the tables after the quiesce — are retired by
/// hand, keeping only the newest (active) journal, which carries the
/// post-recovery meta writes. Pass 2 reopens to prove the result recovers.
/// Marker written into masters whose wash completed. Versioned: bumping
/// it re-heals every master produced by an older (possibly buggy) wash.
const WASH_MARKER: &str = "washed6";

fn open_raw(dir: &Path) -> fjall::Result<fjall::SingleWriterTxDatabase> {
    fjall::SingleWriterTxDatabase::builder(dir)
        .max_journaling_size(64 * 1024 * 1024)
        .open()
}

/// Normalize a store copy so later opens skip journal replay — by
/// REBUILDING it: bulk-copy every keyspace's rows into a born-fresh
/// database, then swap it into place. A fresh store has a tiny journal
/// and self-consistent seqnos by construction. (Surgically deleting
/// journal files is off the table: fjall's active journal is a bare
/// numbered file preallocated to 64MiB, sealed ones are `N.jnl`, and the
/// rename dance between the two during recovery is undocumented — a
/// previous wash corrupted a master by guessing it.)
fn wash_dir(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use fjall::Readable;
    let fresh = dir.with_extension("fresh");
    let _ = std::fs::remove_dir_all(&fresh);
    {
        let src = open_raw(dir)?;
        let dst = open_raw(&fresh)?;
        let snap = src.read_tx();
        for name in src.list_keyspace_names() {
            let sks = src.keyspace(&name, fjall::KeyspaceCreateOptions::default)?;
            let dks = dst.keyspace(&name, fjall::KeyspaceCreateOptions::default)?;
            for kv in snap.iter(&sks) {
                let (k, v) = kv.into_inner()?;
                dks.insert(k, v)?;
            }
        }
        // fjall rotates (and can then evict) journals only when the active
        // journal crosses a hardcoded 64MB at flush time — a store whose
        // whole corpus is smaller never rotates, and its entire rebuild is
        // replayed on every future open (measured: 705ms for a 1.1k-file
        // era vs 11ms for one that happened to cross the threshold). Pad
        // past the threshold with throwaway ballast; in a born-fresh store
        // the seqno watermarks are consistent, so the sealed journal is
        // genuinely evicted by the normal rotate/flush cycle.
        // a fixed volume: journal_disk_space() counts the active journal's
        // 64MiB preallocation, so probing it under-writes
        let ballast = dst.keyspace("wash_ballast", fjall::KeyspaceCreateOptions::default)?;
        let chunk = vec![0u8; 1024 * 1024];
        for i in 0..68u64 {
            ballast.insert(i.to_be_bytes(), chunk.as_slice())?;
        }
        quiesce(&dst); // flush crosses the threshold -> the fat journal seals
        dst.inner().delete_keyspace(ballast.inner().clone())?;
        quiesce(&dst);
        dst.persist(fjall::PersistMode::SyncAll)?;
    }
    // NO journal-file deletion of any kind: the ballast-triggered mid-life
    // rotation lets fjall's own eviction collect the fat journals (all
    // watermarks persisted, consistent fresh-store seqnos), and what drop
    // seals at close is only the small post-ballast tail. Hand-deleting
    // files — even "provably flushed" ones — broke journal-id continuity
    // and corrupted the masters' DESCENDANTS: clones of a live store
    // descended from a journal-stripped master failed to open.
    let graph = dir.join("hnsw.graph");
    if graph.exists() {
        std::fs::copy(&graph, fresh.join("hnsw.graph"))?;
    }
    let old = dir.with_extension("old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(dir, &old)?;
    std::fs::rename(&fresh, dir)?;
    let _ = std::fs::remove_dir_all(&old);
    // validation = an exact warp rehearsal: clone the master and open the
    // clone. A master that fails this never reaches the registry.
    let probe = dir.with_extension("probe");
    let _ = std::fs::remove_dir_all(&probe);
    clone_dir(dir, &probe)?;
    {
        let _ = open_raw(&probe)?;
    }
    let _ = std::fs::remove_dir_all(&probe);
    Ok(())
}


/// Fsync the live store, clone it, and queue the clone for washing.
/// Deliberately does NOT quiesce inline — the wash re-quiesces the copy
/// anyway, so draining flushes here would only stall the interactive loop
/// (measured up to 1.3s right after a teleport). Inline cost is one fsync
/// plus one CoW clone. The master joins the registry only once washed.
fn begin_checkpoint<P: Push<Keyed<String, Doc>>>(
    st: &mut KeyedStream<String, Doc, P>,
    live_dir: &Path,
    ckpt_root: &Path,
    idx: usize,
    oid: &str,
    wash: &mpsc::Sender<WashJob>,
) {
    let dst = ckpt_root.join(oid);
    let tmp = ckpt_root.join(format!(".tmp-{oid}"));
    if dst.exists() || tmp.exists() {
        return; // already a master, or a wash is in flight
    }
    let t = std::time::Instant::now();
    st.checkpoint(); // fsync: the clone must capture fully-committed state
    if clone_dir(live_dir, &tmp).is_err() {
        return;
    }
    eprintln!(
        "seance: checkpoint {} cloned in {} ms, queued for wash",
        &oid[..oid.len().min(10)],
        t.elapsed().as_millis()
    );
    let _ = wash.send(WashJob::Fresh { tmp, dst, idx });
}

/// Insert a finished master, evicting the most redundant one (smallest
/// gap to its nearest neighbor; never the head master) past the cap —
/// masters pin LSM file sets, so an unbounded registry eats the disk.
fn insert_master(
    registry: &mut HashMap<usize, PathBuf>,
    head: usize,
    idx: usize,
    path: PathBuf,
) {
    const MAX_MASTERS: usize = 16;
    registry.insert(idx, path);
    if registry.len() <= MAX_MASTERS {
        return;
    }
    let mut evict: Option<(usize, usize)> = None; // (gap, idx)
    for &i in registry.keys() {
        if i == head {
            continue;
        }
        let gap = registry
            .keys()
            .filter(|&&j| j != i)
            .map(|&j| j.abs_diff(i))
            .min()
            .unwrap_or(usize::MAX);
        if evict.is_none_or(|(g, _)| gap < g) {
            evict = Some((gap, i));
        }
    }
    if let Some((_, i)) = evict {
        if let Some(p) = registry.remove(&i) {
            let _ = std::fs::remove_dir_all(&p);
            eprintln!("seance: evicted redundant checkpoint (idx {i})");
        }
    }
}

/// Layer-1 benchmark: build (or reopen) the chronicle, then measure
/// zero-write as-of-T lookups across the whole timeline. JSON on stdout.
fn bench_chronicle(repo: &Path, commits: &[git::Commit], ch: &chronicle::Chronicle) {
    let empty = git::empty_tree(repo);
    let head_oid = &commits[commits.len() - 1].oid;
    let paths: Vec<String> = git::diff(repo, &empty, head_oid)
        .into_iter()
        .map(|(_, p)| p)
        .take(400)
        .collect();

    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let n = commits.len() as u64;

    let percentile = |sorted: &[u64], p: f64| sorted[(sorted.len() as f64 * p) as usize];

    let mut stats_ns: Vec<u64> = Vec::with_capacity(2000);
    let mut stats_hits = 0usize;
    for _ in 0..2000 {
        let t = (rng() % n) as u32;
        let started = std::time::Instant::now();
        let r = ch.stats_at(t);
        stats_ns.push(started.elapsed().as_nanos() as u64);
        stats_hits += r.is_some() as usize;
    }
    stats_ns.sort_unstable();

    let mut oid_ns: Vec<u64> = Vec::with_capacity(2000);
    let mut oid_hits = 0usize;
    for _ in 0..2000 {
        let t = (rng() % n) as u32;
        let path = &paths[(rng() as usize) % paths.len()];
        let started = std::time::Instant::now();
        let r = ch.oid_at(path, t);
        oid_ns.push(started.elapsed().as_nanos() as u64);
        oid_hits += r.is_some() as usize;
    }
    oid_ns.sort_unstable();

    println!(
        "{{\"commits\":{},\"stats_at_us\":{{\"p50\":{},\"p99\":{},\"hits\":{}}},\"oid_at_us\":{{\"p50\":{},\"p99\":{},\"hits\":{}}}}}",
        commits.len(),
        percentile(&stats_ns, 0.5) / 1000,
        percentile(&stats_ns, 0.99) / 1000,
        stats_hits,
        percentile(&oid_ns, 0.5) / 1000,
        percentile(&oid_ns, 0.99) / 1000,
        oid_hits,
    );
}

/// Jumps of at most this many commits are walked one commit at a time,
/// publishing a real, searchable view at every step — the scrub becomes a
/// live playback of history, with each step a small (<50ms) transaction.
/// Longer teleports apply one direct diff instead: past this distance the
/// summed per-commit churn exceeds the bounded full-tree diff.
const STEP_WALK_MAX: usize = 300;
/// Jumps touching more than this many paths poll for newer commands at
/// each phase boundary and abort if the target moved. Everything before
/// the transaction is read-only, so an aborted jump leaves no trace — a
/// drag cancels stale multi-second teleports within one phase (~20-350ms)
/// instead of queueing them, while an uninterrupted teleport still pays
/// only the one direct diff.
const INTERRUPT_MIN_PATHS: usize = 100;

/// Prefetch per-commit changed paths for walk steps in `(lo, hi]`: one
/// `git log` spawn replaces one `git diff-tree` spawn per step, which
/// otherwise costs ~10ms of process overhead on a ~20ms step.
fn prefetch_walk(
    repo: &Path,
    commits: &[git::Commit],
    oid_idx: &HashMap<String, usize>,
    lo: usize,
    hi: usize,
    cache: &mut HashMap<usize, Vec<String>>,
) {
    if hi <= lo {
        return;
    }
    let t = std::time::Instant::now();
    let ranges = git::first_parent_changes(repo, &commits[lo].oid, &commits[hi].oid);
    let n = ranges.len();
    for (oid, paths) in ranges {
        if let Some(&i) = oid_idx.get(&oid) {
            cache.insert(i, paths);
        }
    }
    eprintln!(
        "seance: prefetched {} walk diffs ({}..{}] in {} ms",
        n,
        lo,
        hi,
        t.elapsed().as_millis()
    );
}

/// Owns the fold stream: converges the cursor toward the latest requested
/// target, republishing snapshots as it goes. Commands are drained between
/// steps, so dragging the slider retargets the walk instead of queueing
/// stale jumps.
fn ingest(
    repo: PathBuf,
    commits: Vec<git::Commit>,
    rx: mpsc::Receiver<Cmd>,
    view_tx: watch::Sender<View>,
) {
    let oid_idx: HashMap<String, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.oid.clone(), i))
        .collect();

    // the live store is always a scratch dir; checkpoint masters are the
    // durable artifacts, so restarts warp instead of rematerializing
    let ckpt_root = ckpt_root_for(&repo);
    let _ = std::fs::create_dir_all(&ckpt_root);
    let live_root = live_root_for(&repo);
    let _ = std::fs::remove_dir_all(&live_root);
    let _ = std::fs::create_dir_all(&live_root);
    let mut live_counter = 0usize;

    // one worker serializes every wash and heal: concurrent washes were
    // contending with the interactive store for disk and CPU
    let (ckpt_tx, ckpt_rx) = mpsc::channel::<(usize, PathBuf)>();
    let (wash_tx, wash_rx) = mpsc::channel::<WashJob>();
    {
        let done = ckpt_tx.clone();
        std::thread::spawn(move || {
            for job in wash_rx {
                let t = std::time::Instant::now();
                match job {
                    WashJob::Fresh { tmp, dst, idx } => match wash_dir(&tmp) {
                        Ok(()) => {
                            let _ = std::fs::write(tmp.join(WASH_MARKER), b"");
                            if std::fs::rename(&tmp, &dst).is_ok() {
                                eprintln!(
                                    "seance: checkpoint ready at idx {idx} ({} ms wash)",
                                    t.elapsed().as_millis()
                                );
                                let _ = done.send((idx, dst));
                            }
                        }
                        Err(e) => {
                            eprintln!("seance: checkpoint wash failed, discarding: {e}");
                            let _ = std::fs::remove_dir_all(&tmp);
                        }
                    },
                    WashJob::Heal { dir, idx } => match wash_dir(&dir) {
                        Ok(()) => {
                            let _ = std::fs::write(dir.join(WASH_MARKER), b"");
                            eprintln!(
                                "seance: healed stale checkpoint at idx {idx} ({} ms)",
                                t.elapsed().as_millis()
                            );
                            let _ = done.send((idx, dir));
                        }
                        // a master that cannot even be read is beyond
                        // healing: discard it, it's only a cache
                        Err(e) => {
                            eprintln!("seance: heal failed for idx {idx}, discarding: {e}");
                            let _ = std::fs::remove_dir_all(&dir);
                        }
                    },
                }
            }
        });
    }

    let mut registry: HashMap<usize, PathBuf> = HashMap::new();
    let mut healing = 0usize;
    if let Ok(rd) = std::fs::read_dir(&ckpt_root) {
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(oid) = name.to_str() else { continue };
            if oid.starts_with(".tmp-")
                || oid.ends_with(".fresh")
                || oid.ends_with(".old")
                || oid.ends_with(".probe")
            {
                // in-flight wash/rebuild from a previous run: discard
                let _ = std::fs::remove_dir_all(e.path());
                continue;
            }
            // checkpoints for commits off the current first-parent line
            // (e.g. after a fetch) are simply unreachable
            let Some(&idx) = oid_idx.get(oid) else { continue };
            if e.path().join(WASH_MARKER).exists() {
                registry.insert(idx, e.path());
            } else {
                healing += 1;
                let _ = wash_tx.send(WashJob::Heal {
                    dir: e.path(),
                    idx,
                });
            }
        }
    }
    if !registry.is_empty() || healing > 0 {
        eprintln!(
            "seance: {} checkpoint(s) on disk, {healing} healing in background",
            registry.len() + healing
        );
    }

    // expanded exactly once: every open shares one concrete store type
    let open_store = |path: &Path| KeyedStream::new(path, pipeline!(path));

    let head = commits.len() - 1;
    let mut cursor: Option<usize> = None;
    let mut live_dir = live_root.join(live_counter.to_string());
    live_counter += 1;
    let mut st = match registry
        .iter()
        .min_by_key(|(i, _)| i.abs_diff(head))
        .map(|(&i, p)| (i, p.clone()))
    {
        Some((idx, ck)) => {
            let t = std::time::Instant::now();
            if clone_dir(&ck, &live_dir).is_ok() {
                let s = open_store(&live_dir);
                cursor = Some(idx);
                eprintln!(
                    "seance: resumed from checkpoint {} (idx {}) in {} ms",
                    &commits[idx].oid[..10],
                    idx,
                    t.elapsed().as_millis()
                );
                s
            } else {
                open_store(&live_dir)
            }
        }
        None => open_store(&live_dir),
    };
    let mut blobs = git::Blobs::new(&repo);
    let empty_tree = git::empty_tree(&repo);

    // the chronicle builds once in the background; when it signals done we
    // open our own (read-only-in-practice) handle for zero-write as-of-T
    let chron_db = chron_path_for(&repo);
    let (chron_tx, chron_rx) = mpsc::channel::<()>();
    {
        let (repo, commits, chron_db, empty_tree) = (
            repo.clone(),
            commits.clone(),
            chron_db.clone(),
            empty_tree.clone(),
        );
        std::thread::spawn(move || {
            // a panic here is the chron db locked by another seance
            // instance. Retry forever: the chronicle simply comes online
            // whenever the lock frees, however long that takes — a demo
            // should degrade to "no instant stats yet", never lose the
            // feature for the whole session
            let mut attempt = 0u32;
            loop {
                let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let ch = chronicle::build(&repo, &commits, &chron_db, &empty_tree);
                    drop(ch); // release the store's single-writer lock for ingest
                }));
                if built.is_ok() {
                    let _ = chron_tx.send(());
                    return;
                }
                attempt += 1;
                if attempt <= 3 {
                    eprintln!(
                        "seance: chronicle locked (another seance running against this repo?) — retrying every 10s"
                    );
                }
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
        });
    }
    let mut chron: Option<chronicle::Chronicle> = None;

    let mut query = String::new();
    let mut walk_cache: HashMap<usize, Vec<String>> = HashMap::new();
    let mut ckpt_attempted: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // materialize HEAD so the page loads populated (fresh start only —
    // a checkpoint resume already has state and converges via the loop)
    let mut target = head;
    let mut apply = (0u64, 0usize);
    if cursor.is_none() {
        apply = goto(
            &mut st, &mut blobs, &repo, &empty_tree, &commits, &mut cursor, head, None,
            &mut || false,
        )
        .expect("initial materialization cannot be interrupted");
        persist_graph!(st);
        begin_checkpoint(&mut st, &live_dir, &ckpt_root, head, &commits[head].oid, &wash_tx);
    }
    let _ = view_tx.send(snapshot!(st, commits, cursor.unwrap(), query, apply));
    let mut transit_sent = false;

    loop {
        // converge toward the target: single-commit steps in walk range,
        // one direct (but cancellable) teleport beyond it
        while cursor != Some(target) {
            if chron.is_none() && chron_rx.try_recv().is_ok() {
                chron = Some(chronicle::Chronicle::open(&chron_db));
                eprintln!("seance: chronicle online — instant as-of-T stats");
            }
            while let Ok((i, p)) = ckpt_rx.try_recv() {
                insert_master(&mut registry, head, i, p);
            }
            let cur = cursor.unwrap();
            let dist = target.abs_diff(cur);
            // teleport in flight: snap the header to the target instantly
            // from the chronicle (zero writes) while search catches up
            if dist > STEP_WALK_MAX {
                if let Some(row) = chron.as_ref().and_then(|c| c.stats_at(target as u32)) {
                    let c = &commits[target];
                    transit_sent = true;
                    let _ = view_tx.send(View {
                        cursor: target,
                        oid: c.oid.get(..10).unwrap_or(&c.oid).to_string(),
                        subject: c.subject.clone(),
                        author: c.author.clone(),
                        at: c.at,
                        files: row.files,
                        lines: row.lines,
                        by_ext: row.by_ext.into_iter().take(12).collect(),
                        query: query.clone(),
                        transit: true,
                        ..View::default()
                    });
                }
            }
            let (next, cached) = if dist <= STEP_WALK_MAX {
                let next = if target > cur { cur + 1 } else { cur - 1 };
                let younger = cur.max(next);
                if !walk_cache.contains_key(&younger) {
                    let (lo, hi) = (cur.min(target), cur.max(target));
                    prefetch_walk(&repo, &commits, &oid_idx, lo, hi, &mut walk_cache);
                }
                (next, walk_cache.get(&younger))
            } else {
                // warp: if a checkpoint sits materially closer to the
                // target than we do, become a clone of it instead of
                // diffing our way there
                let best = registry
                    .iter()
                    .min_by_key(|(i, _)| i.abs_diff(target))
                    .map(|(&i, p)| (i, p.clone()));
                if let Some((ck_idx, ck_path)) = best {
                    if ck_idx != cur && ck_idx.abs_diff(target) * 2 < dist {
                        let t = std::time::Instant::now();
                        let new_live = live_root.join(live_counter.to_string());
                        live_counter += 1;
                        if clone_dir(&ck_path, &new_live).is_ok() {
                            let clone_ms = t.elapsed().as_millis() as u64;
                            let t = std::time::Instant::now();
                            // a master is only a cache: if its clone fails
                            // to open, discard it and diff our way there
                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                open_store(&new_live)
                            })) {
                                Ok(store) => {
                                    st = store;
                                    let open_ms = t.elapsed().as_millis() as u64;
                                    let old = std::mem::replace(&mut live_dir, new_live);
                                    let _ = std::fs::remove_dir_all(&old);
                                    cursor = Some(ck_idx);
                                    apply = (clone_ms + open_ms, 0);
                                    eprintln!(
                                        "seance: warp to checkpoint {} (idx {ck_idx}): clone {clone_ms} ms, open {open_ms} ms",
                                        &commits[ck_idx].oid[..10]
                                    );
                                    let _ = view_tx
                                        .send(snapshot!(st, commits, ck_idx, query, apply));
                                    continue;
                                }
                                Err(_) => {
                                    eprintln!(
                                        "seance: warp target idx {ck_idx} corrupt, discarding master"
                                    );
                                    let _ = std::fs::remove_dir_all(&new_live);
                                    registry.remove(&ck_idx);
                                    let _ = std::fs::remove_dir_all(&ck_path);
                                }
                            }
                        }
                    }
                }
                (target, None)
            };

            // large jumps poll this between phases: drain arrivals into a
            // stash and abort if any of them moved the target
            let mut stash: Vec<Cmd> = Vec::new();
            let result = goto(
                &mut st, &mut blobs, &repo, &empty_tree, &commits, &mut cursor, next, cached,
                &mut || {
                    while let Ok(cmd) = rx.try_recv() {
                        stash.push(cmd);
                    }
                    stash
                        .iter()
                        .any(|c| matches!(c, Cmd::Goto(i) if (*i).min(head) != target))
                },
            );
            if let Some(a) = result {
                apply = a;
                let _ = view_tx.send(snapshot!(st, commits, cursor.unwrap(), query, apply));
                // note: no checkpoint here — regions merely passed through
                // during a drag don't deserve masters, and creation right
                // after a teleport stalls the loop; the idle dwell below
                // memoizes wherever the user actually settles
            }
            for cmd in stash {
                match cmd {
                    Cmd::Goto(i) => target = i.min(head),
                    Cmd::Query(q) => query = q,
                }
            }
            // newer commands also interrupt the walk between steps
            while let Ok(cmd) = rx.try_recv() {
                match cmd {
                    Cmd::Goto(i) => target = i.min(head),
                    Cmd::Query(q) => query = q,
                }
            }
        }

        // converged: if a transit preview went out, correct the record
        // with a real snapshot before going idle
        if transit_sent {
            transit_sent = false;
            let _ = view_tx.send(snapshot!(st, commits, cursor.unwrap(), query, apply));
        }

        // block for the next command; after a short dwell with no input,
        // memoize the region the user settled in as a checkpoint — never
        // during active scrubbing, and only where no master is near
        let first = match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(cmd) => cmd,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(cur) = cursor {
                    let near = registry.keys().any(|&i| i.abs_diff(cur) <= STEP_WALK_MAX / 2);
                    let oid = &commits[cur].oid;
                    // one attempt per position per session: a failed wash
                    // must not re-clone on every idle tick
                    if !near
                        && !ckpt_attempted.contains(&cur)
                        && !ckpt_root.join(oid).exists()
                        && !ckpt_root.join(format!(".tmp-{oid}")).exists()
                    {
                        ckpt_attempted.insert(cur);
                        persist_graph!(st);
                        begin_checkpoint(&mut st, &live_dir, &ckpt_root, cur, oid, &wash_tx);
                    }
                }
                match rx.recv() {
                    Ok(cmd) => cmd,
                    Err(_) => return,
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        while let Ok((i, p)) = ckpt_rx.try_recv() {
            insert_master(&mut registry, head, i, p);
        }
        let mut cmds = vec![first];
        while let Ok(more) = rx.try_recv() {
            cmds.push(more);
        }
        for cmd in cmds {
            match cmd {
                Cmd::Goto(i) => target = i.min(commits.len() - 1),
                Cmd::Query(q) => query = q,
            }
        }
        // a query change with no movement still needs a fresh view
        if cursor == Some(target) {
            let _ = view_tx.send(snapshot!(st, commits, target, query, apply));
        }
    }
}

/// Headless check: materialize HEAD, rewind to the first commit, return to
/// HEAD. Prints one JSON view per stop on stdout, then verifies the first
/// and last stops agree on every deterministic view (file/line counts,
/// extension table, bm25 hits) — if they don't, retraction is broken and
/// the exit code says so. The hnsw views are compared by distance per key
/// only: an approximate graph's k-nearest *membership* may legitimately
/// vary with insertion order.
fn probe(repo: &Path, commits: &[git::Commit], query: &str) {
    let db_path = db_path_for(repo);
    let _ = std::fs::remove_dir_all(&db_path);
    let mut st = KeyedStream::new(&db_path, pipeline!(&db_path));
    let mut blobs = git::Blobs::new(repo);
    let empty_tree = git::empty_tree(repo);
    let mut cursor: Option<usize> = None;

    let head = commits.len() - 1;
    let mut views = Vec::new();
    for target in [head, 0, head] {
        let apply = goto(
            &mut st, &mut blobs, repo, &empty_tree, commits, &mut cursor, target, None,
            &mut || false,
        )
        .expect("probe jumps are never interrupted");
        let view = snapshot!(st, commits, target, query, apply);
        println!("{}", serde_json::to_string(&view).unwrap());
        views.push(view);
    }

    let (a, b) = (&views[0], &views[2]);
    let deterministic_ok = a.files == b.files
        && a.lines == b.lines
        && a.by_ext == b.by_ext
        && a.keyword == b.keyword
        && a.hybrid.len() == b.hybrid.len();
    let semantic_ok = {
        let dist: HashMap<&str, f64> = b.semantic.iter().map(|h| (h.path.as_str(), h.score)).collect();
        a.semantic
            .iter()
            .all(|h| dist.get(h.path.as_str()).is_none_or(|d| (d - h.score).abs() < 1e-6))
    };
    if deterministic_ok && semantic_ok {
        eprintln!("seance: probe ok — HEAD -> first -> HEAD round trip is consistent");
    } else {
        eprintln!("seance: probe FAILED — views diverged across the round trip");
        std::process::exit(1);
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: seance [repo-path] [max-commits] [--probe <query>] [--chronicle-bench] [--bench-walk <n> [--queries \"a,b,c\"]]"
    );
    std::process::exit(2);
}

/// Head-to-head benchmark workload: materialize at HEAD-n, walk forward one
/// commit at a time to HEAD, running the same fixed queries after every
/// step. One JSON line per commit on stdout — the same schema the sqlite /
/// grep baseline driver emits, so `bench.py summary` compares them
/// directly. Note for fairness: apply_ms here INCLUDES ese embedding and
/// HNSW maintenance per changed file, which no baseline performs.
fn run_bench_walk(
    repo: &Path,
    commits: &[git::Commit],
    n: usize,
    queries: &[String],
    dump_vecs: Option<&Path>,
) {
    let db_path = db_path_for(repo);
    let _ = std::fs::remove_dir_all(&db_path);
    let mut st = KeyedStream::new(&db_path, pipeline!(&db_path));
    let mut blobs = git::Blobs::new(repo);
    let empty_tree = git::empty_tree(repo);
    let oid_idx: HashMap<String, usize> = commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.oid.clone(), i))
        .collect();

    let head = commits.len() - 1;
    let start = head.saturating_sub(n);
    let mut cursor: Option<usize> = None;
    let t = std::time::Instant::now();
    goto(
        &mut st, &mut blobs, repo, &empty_tree, commits, &mut cursor, start, None,
        &mut || false,
    )
    .expect("bench materialization is never interrupted");
    eprintln!(
        "seance: bench baseline materialized at idx {start} in {} ms",
        t.elapsed().as_millis()
    );
    let mut cache: HashMap<usize, Vec<String>> = HashMap::new();
    prefetch_walk(repo, commits, &oid_idx, start, head, &mut cache);

    for i in (start + 1)..=head {
        let cached = cache.get(&i);
        let (mut apply_ms, changed) = goto(
            &mut st, &mut blobs, repo, &empty_tree, commits, &mut cursor, i, cached,
            &mut || false,
        )
        .expect("bench steps are never interrupted");
        // periodic maintenance, timed into this step's apply cost for
        // fairness (FTS5 pays its merge policy inside its applies too):
        // without it, posting scans degrade ~50x over 1000 commits of
        // churn as LSM read amplification accumulates (and re-degrades within ~500 commits, hence the 200-commit cadence)
        if i % 200 == 0 {
            let t = std::time::Instant::now();
            if let Ok(ks) = st
                .db()
                .keyspace("sink_text", fjall::KeyspaceCreateOptions::default)
            {
                let _ = ks.inner().major_compact();
            }
            let compact_ms = t.elapsed().as_millis() as u64;
            eprintln!("seance: bench compacted bm25 postings in {compact_ms} ms");
            apply_ms += compact_ms;
        }

        let (query_us, hits, stats_us) = st.rtx(|(bm25, _, _, count, _, lines)| {
            let mut query_us = Vec::with_capacity(queries.len());
            let mut hits = Vec::with_capacity(queries.len());
            for q in queries {
                let t = std::time::Instant::now();
                let found = bm25.search(q, 10);
                query_us.push(t.elapsed().as_micros() as u64);
                hits.push(found.len());
            }
            let t = std::time::Instant::now();
            let _ = (count.get(), lines.sum());
            (query_us, hits, t.elapsed().as_micros() as u64)
        });
        println!(
            "{{\"idx\":{i},\"apply_ms\":{apply_ms},\"changed\":{changed},\"query_us\":{query_us:?},\"hits\":{hits:?},\"stats_us\":{stats_us}}}"
        );
    }

    // ---- capability benches; the store now sits at HEAD ----

    // semantic (top-10 by cosine over ese embeddings) and hybrid (keyword
    // + semantic + reciprocal-rank fusion). Bog's numbers INCLUDE query
    // embedding; the baselines are handed pre-computed query vectors.
    let (sem_us, hyb_us) = st.rtx(|(bm25, vecs, _, _, _, _)| {
        let (mut sem, mut hyb) = (Vec::new(), Vec::new());
        for _ in 0..50 {
            for q in queries {
                let t = std::time::Instant::now();
                let _ = vecs.search(&embed(q));
                sem.push(t.elapsed().as_micros() as u64);
                let t = std::time::Instant::now();
                let kw = bm25.search(q, 10);
                let sm = vecs.search(&embed(q));
                let _ = rrf(&kw, &sm);
                hyb.push(t.elapsed().as_micros() as u64);
            }
        }
        (sem, hyb)
    });
    println!("{{\"cap\":\"semantic\",\"query_us\":{sem_us:?}}}");
    println!("{{\"cap\":\"hybrid\",\"query_us\":{hyb_us:?}}}");

    // the baselines search the SAME vectors: dump docs + query embeddings
    if let Some(path) = dump_vecs {
        #[derive(Serialize)]
        struct VecDump {
            queries: Vec<(String, Vec<f32>)>,
            docs: Vec<(String, Vec<f32>)>,
        }
        let docs: Vec<(String, Vec<f32>)> =
            st.rtx(|(_, _, files, _, _, _)| files.iter().map(|(p, d): (String, Doc)| (p, d.vec)).collect());
        let dump = VecDump {
            queries: queries.iter().map(|q| (q.clone(), embed(q).to_vec())).collect(),
            docs,
        };
        let f = std::fs::File::create(path).expect("dump-vecs path unwritable");
        serde_json::to_writer(std::io::BufWriter::new(f), &dump).unwrap();
        eprintln!("seance: dumped {} doc vectors to {}", dump.docs.len(), path.display());
    }

    // era snapshot: the production checkpoint flow (fsync + CoW clone
    // inline; wash off-path, reported separately)
    let bench_ckpt = std::env::temp_dir().join("seance-bench-ckpt");
    let _ = std::fs::remove_dir_all(&bench_ckpt);
    persist_graph!(st);
    let t = std::time::Instant::now();
    st.checkpoint();
    clone_dir(&db_path, &bench_ckpt).expect("bench checkpoint clone");
    println!("{{\"cap\":\"checkpoint_create\",\"ms\":{}}}", t.elapsed().as_millis());
    let t = std::time::Instant::now();
    wash_dir(&bench_ckpt).expect("bench checkpoint wash");
    println!("{{\"cap\":\"checkpoint_wash_background\",\"ms\":{}}}", t.elapsed().as_millis());

    // retraction-exact time travel, no checkpoint: one direct diff jump
    let far = head.saturating_sub(10_000);
    let (jump_ms, jump_changed) = goto(
        &mut st, &mut blobs, repo, &empty_tree, commits, &mut cursor, far, None,
        &mut || false,
    )
    .expect("bench jump is never interrupted");
    println!(
        "{{\"cap\":\"cold_jump\",\"commits\":{},\"changed\":{jump_changed},\"ms\":{jump_ms}}}",
        head - far
    );

    // warp back to HEAD via the washed snapshot: clone + open + query
    let warp_live = std::env::temp_dir().join("seance-bench-warp.db");
    let _ = std::fs::remove_dir_all(&warp_live);
    let t = std::time::Instant::now();
    clone_dir(&bench_ckpt, &warp_live).expect("bench warp clone");
    let st2 = KeyedStream::new(&warp_live, pipeline!(&warp_live));
    let open_ms = t.elapsed().as_millis();
    let hits = st2.rtx(|(bm25, _, _, _, _, _)| bm25.search(&queries[0], 10).len());
    println!(
        "{{\"cap\":\"warp\",\"ms\":{},\"open_ms\":{open_ms},\"hits\":{hits}}}",
        t.elapsed().as_millis()
    );
    drop(st2);
    let _ = std::fs::remove_dir_all(&warp_live);
    let _ = std::fs::remove_dir_all(&bench_ckpt);
}

fn main() {
    let mut repo = PathBuf::from(".");
    let mut limit: Option<usize> = None;
    let mut probe_query: Option<String> = None;
    let mut chron_bench = false;
    let mut bench_walk: Option<usize> = None;
    let mut dump_vecs: Option<String> = None;
    let mut queries = "btree balance,wal checkpoint,vdbe cursor".to_string();

    let mut positional = 0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--probe" {
            probe_query = Some(args.next().unwrap_or_else(|| usage()));
        } else if arg == "--chronicle-bench" {
            chron_bench = true;
        } else if arg == "--bench-walk" {
            bench_walk = Some(
                args.next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage()),
            );
        } else if arg == "--queries" {
            queries = args.next().unwrap_or_else(|| usage());
        } else if arg == "--dump-vecs" {
            dump_vecs = Some(args.next().unwrap_or_else(|| usage()));
        } else if arg == "--help" || arg == "-h" {
            usage();
        } else {
            match positional {
                0 => repo = PathBuf::from(&arg),
                1 => limit = Some(arg.parse().unwrap_or_else(|_| usage())),
                _ => usage(),
            }
            positional += 1;
        }
    }

    let repo = repo.canonicalize().unwrap_or_else(|e| {
        eprintln!("seance: cannot open repo: {e}");
        std::process::exit(2);
    });
    let commits = git::list_commits(&repo, limit);
    if commits.is_empty() {
        eprintln!("seance: no commits found in {}", repo.display());
        std::process::exit(2);
    }
    eprintln!(
        "seance: {} — {} commits on the first-parent line",
        repo.display(),
        commits.len()
    );

    if let Some(n) = bench_walk {
        let qs: Vec<String> = queries.split(',').map(|q| q.trim().to_string()).collect();
        run_bench_walk(&repo, &commits, n, &qs, dump_vecs.as_deref().map(Path::new));
        return;
    }
    if chron_bench {
        let ch = chronicle::build(&repo, &commits, &chron_path_for(&repo), &git::empty_tree(&repo));
        bench_chronicle(&repo, &commits, &ch);
        return;
    }
    if let Some(q) = probe_query {
        probe(&repo, &commits, &q);
        return;
    }

    let hello = serde_json::to_string(&Hello {
        repo: repo.display().to_string(),
        total: commits.len(),
    })
    .unwrap();

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let initial = View {
        subject: "materializing repository…".to_string(),
        ..View::default()
    };
    let (view_tx, view_rx) = watch::channel(initial);
    std::thread::spawn(move || ingest(repo, commits, cmd_rx, view_tx));

    serve(hello, cmd_tx, view_rx);
}

type AppState = (String, mpsc::Sender<Cmd>, watch::Receiver<View>);

#[tokio::main]
async fn serve(hello: String, cmd_tx: mpsc::Sender<Cmd>, view_rx: watch::Receiver<View>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state((hello, cmd_tx, view_rx));

    let port: u16 = std::env::var("SEANCE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3333);
    let addr = format!("0.0.0.0:{port}");
    eprintln!("seance: the veil thins at http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn ws_upgrade(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Per-client task: greet with repo metadata and the current view, then
/// mirror every new snapshot down and feed every command up.
async fn handle_socket(mut socket: WebSocket, (hello, cmd_tx, mut view_rx): AppState) {
    if socket.send(Message::text(hello)).await.is_err() {
        return;
    }
    let current = serde_json::to_string(&*view_rx.borrow_and_update()).unwrap();
    if socket.send(Message::text(current)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = view_rx.changed() => {
                if changed.is_err() {
                    return; // ingest thread gone
                }
                let update = serde_json::to_string(&*view_rx.borrow_and_update()).unwrap();
                if socket.send(Message::text(update)).await.is_err() {
                    return;
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(Message::Text(text))) = incoming else {
                    return; // client closed or errored
                };
                let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
                    continue;
                };
                if let Some(i) = msg.goto {
                    let _ = cmd_tx.send(Cmd::Goto(i));
                }
                if let Some(q) = msg.query {
                    let _ = cmd_tx.send(Cmd::Query(q));
                }
            }
        }
    }
}
