//! The sod-web app's replica, packaged as a Node.js native addon.
//!
//! Per-app compiled sod: datum `String` (emoji slug), pipeline
//! `(sod::sinks::Bag<String>, fold Count)`, `SCHEMA = 1`. One replica per
//! process behind a mutex. Sync I/O never blocks the JS event loop:
//! `syncWithPeer` runs on a libuv worker thread ([`AsyncTask`]), and the
//! serve loop owns a Rust thread that borrows the replica only for the
//! milliseconds of each session (via sod's `SyncListener`).
//!
//! Peer identity comes from sync itself: every completed session (either
//! direction) records the peer's replica id, and `status().connectedIds`
//! lists the distinct peers seen within the last 10 s — the top-nav
//! "N bogs connected" badge.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fold::pipeline::terminal::Count;
use napi::bindgen_prelude::*;
use napi::{Env, Task};
use napi_derive::napi;
use sod::engine_fold::FoldEngine;
use sod::log_file::FileLog;
use sod::sinks::Bag;
use sod::time::Watermark;
use sod::transport::ws::{SyncListener, connect};
use sod::{Replica, ReplicaId};

type Pipeline = (Bag<String>, Count);
type AppReplica = Replica<FoldEngine<String, Pipeline>, FileLog>;

/// Bump when the datum type or pipeline changes shape (SOD-9).
const SCHEMA: u32 = 1;
/// A peer counts as "connected" if a session completed within this window.
const LIVENESS: Duration = Duration::from_secs(10);

static REPLICA: Mutex<Option<AppReplica>> = Mutex::new(None);
static PEERS_SEEN: Mutex<BTreeMap<ReplicaId, Instant>> = Mutex::new(BTreeMap::new());
static SERVE_STARTED: OnceLock<String> = OnceLock::new();

fn err(e: impl std::fmt::Display) -> Error {
    Error::from_reason(e.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn seen(peer: ReplicaId) {
    PEERS_SEEN.lock().unwrap().insert(peer, Instant::now());
}

fn open_replica(dir: &Path) -> std::result::Result<AppReplica, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log_path = dir.join("sod.log");
    let id_path = dir.join("replica_id");
    // SOD-3: the id never outlives the log
    if !log_path.exists() {
        let _ = std::fs::remove_file(&id_path);
        let _ = std::fs::remove_dir_all(dir.join("db"));
    }
    let id = match std::fs::read(&id_path) {
        Ok(bytes) => ReplicaId(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| "replica_id must be 16 bytes".to_string())?,
        ),
        Err(_) => {
            let id = ReplicaId::generate();
            std::fs::write(&id_path, id.0).map_err(|e| e.to_string())?;
            id
        }
    };
    let log = FileLog::open(&log_path).map_err(|e| e.to_string())?;
    let engine = FoldEngine::open(
        dir.join("db"),
        (Bag::new("reactions"), Count::new("total")),
        Watermark::new(),
    );
    Replica::open(id, log, engine).map_err(|e| e.to_string())
}

fn with_replica<T>(f: impl FnOnce(&mut AppReplica) -> Result<T>) -> Result<T> {
    let mut guard = REPLICA.lock().map_err(|_| err("replica mutex poisoned"))?;
    let replica = guard.as_mut().ok_or_else(|| err("call open(dir) first"))?;
    f(replica)
}

/// Open (or create) the replica at `dir`. One replica per process.
#[napi]
pub fn open(dir: String) -> Result<String> {
    let replica = open_replica(Path::new(&dir)).map_err(err)?;
    let id = replica.id().to_string();
    *REPLICA.lock().map_err(|_| err("replica mutex poisoned"))? = Some(replica);
    Ok(id)
}

/// Close the replica (drops the store handles).
#[napi]
pub fn close() -> Result<()> {
    *REPLICA.lock().map_err(|_| err("replica mutex poisoned"))? = None;
    Ok(())
}

fn commit(emoji: &str, mult: i64) -> Result<()> {
    let datum = postcard::to_stdvec(&emoji.to_string()).map_err(err)?;
    with_replica(|r| {
        r.commit(vec![(datum, mult)], now_ms()).map_err(err)?;
        Ok(())
    })
}

/// Add one reaction.
#[napi]
pub fn react(emoji: String) -> Result<()> {
    commit(&emoji, 1)
}

/// Remove one reaction. Errors at zero — an unmatched retraction would
/// store hidden negative debt that swallows a future reaction. The check
/// and the commit run under ONE replica borrow, so a concurrent remote
/// retraction (sync worker thread) cannot slip between them.
#[napi]
pub fn unreact(emoji: String) -> Result<()> {
    let datum = postcard::to_stdvec(&emoji).map_err(err)?;
    with_replica(|r| {
        let present = r.engine().stream().rtx(|(bag, _)| bag.contains(&emoji));
        if !present {
            return Err(err(format!("nothing to unreact: {emoji}")));
        }
        r.commit(vec![(datum, -1)], now_ms()).map_err(err)?;
        Ok(())
    })
}

#[napi(object)]
pub struct Reaction {
    pub emoji: String,
    pub count: i64,
}

#[napi(object)]
pub struct Board {
    pub reactions: Vec<Reaction>,
    pub total: i64,
}

/// The whole board, in canonical (postcard-key) order.
#[napi]
pub fn board() -> Result<Board> {
    with_replica(|r| {
        let (reactions, total) = r.engine().stream().rtx(|(bag, count)| {
            (
                bag.iter()
                    .map(|(emoji, count)| Reaction { emoji, count })
                    .collect::<Vec<_>>(),
                count.get(),
            )
        });
        Ok(Board { reactions, total })
    })
}

#[napi(object)]
pub struct Status {
    pub id: String,
    /// origin id (hex) -> seq held through
    pub vector: HashMap<String, i64>,
    pub watermark: i64,
    /// distinct peer replica ids (hex) with a completed session ≤ 10 s ago
    pub connected_ids: Vec<String>,
    /// distinct origins whose writes we hold (excluding ourselves)
    pub heard_from: i64,
}

/// Replica identity + convergence state for the UI.
#[napi]
pub fn status() -> Result<Status> {
    with_replica(|r| {
        let id = r.id();
        let vector: HashMap<String, i64> = r
            .vector()
            .iter()
            .map(|(origin, seq)| (origin.to_string(), *seq as i64))
            .collect();
        let heard_from = r.vector().iter().filter(|(o, _)| **o != id).count() as i64;
        let connected_ids = PEERS_SEEN
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, at)| at.elapsed() <= LIVENESS)
            .map(|(peer, _)| peer.to_string())
            .collect();
        Ok(Status {
            id: id.to_string(),
            vector,
            watermark: r.watermark() as i64,
            connected_ids,
            heard_from,
        })
    })
}

pub struct SyncTask {
    url: String,
}

impl Task for SyncTask {
    type Output = Vec<String>;
    type JsValue = Vec<String>;

    fn compute(&mut self) -> Result<Self::Output> {
        // Worker thread: blocking here never blocks the JS event loop.
        // CONNECT BEFORE LOCKING: dialing an unreachable peer (the whole
        // point of the wifi-kill demo) must never stall reads/writes —
        // the replica is borrowed only once the socket is live.
        let outgoing = connect(&self.url).map_err(err)?;
        with_replica(|r| {
            let report = outgoing.run(r, SCHEMA).map_err(err)?;
            seen(report.peer);
            Ok(report.skipped.iter().map(|s| s.to_string()).collect())
        })
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

/// Run one full sync session with a peer. Resolves to the session's
/// per-origin refusals (surface these — empty on a clean sync).
#[napi(ts_return_type = "Promise<Array<string>>")]
pub fn sync_with_peer(url: String) -> AsyncTask<SyncTask> {
    AsyncTask::new(SyncTask { url })
}

/// Start the accept loop on `addr` (once per process); returns the bound
/// address (port 0 resolves). The loop owns a Rust thread and borrows the
/// replica only per session, so writes proceed while it idles.
#[napi]
pub fn start_serve_loop(addr: String) -> Result<String> {
    if let Some(bound) = SERVE_STARTED.get() {
        return Ok(bound.clone());
    }
    let listener = SyncListener::bind(&addr).map_err(err)?;
    let bound = listener.local_addr().map_err(err)?.to_string();
    let bound_ret = bound.clone();
    SERVE_STARTED.set(bound).ok();
    std::thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok(incoming) => {
                    let mut guard = match REPLICA.lock() {
                        Ok(g) => g,
                        Err(_) => break, // poisoned: nothing sane left to do
                    };
                    match guard.as_mut() {
                        Some(replica) => match incoming.run(replica, SCHEMA) {
                            Ok(report) => {
                                drop(guard);
                                for s in &report.skipped {
                                    eprintln!("sod-web: refused during sync: {s}");
                                }
                                seen(report.peer);
                            }
                            Err(e) => eprintln!("sod-web: sync session failed: {e}"),
                        },
                        // replica closed: drop the connection; the peer retries
                        None => drop(incoming),
                    }
                }
                Err(e) => eprintln!("sod-web: accept failed: {e}"),
            }
        }
    });
    Ok(bound_ret)
}
