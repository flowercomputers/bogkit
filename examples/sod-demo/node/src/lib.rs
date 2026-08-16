//! The sod-demo app packaged as a Node.js native addon (napi-rs).
//!
//! This is the "per-app compiled addon" pattern: the app's datum type and
//! fold pipeline are Rust, compiled together with sod into one `.node`
//! module; the JS surface is thin and app-specific. Compiling against
//! in-tree fold/sod means fold API changes break this crate at build time,
//! not at runtime.
//!
//! Access is single-replica-per-process behind a mutex; calls are
//! synchronous (fold is single-writer, and a notes app doesn't need an
//! async bridge). See `demo.mjs` for the workflow.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use fold::pipeline::terminal::Count;
use napi::bindgen_prelude::*;
use sod::sinks::Bag;
use napi_derive::napi;
use sod::engine_fold::FoldEngine;
use sod::log_file::FileLog;
use sod::time::Watermark;
use sod::transport::ws::{serve, sync_with};
use sod::{Replica, ReplicaId};

type Pipeline = (Bag<String>, Count);
type DemoReplica = Replica<FoldEngine<String, Pipeline>, FileLog>;

const SCHEMA: u32 = 1;

static REPLICA: Mutex<Option<DemoReplica>> = Mutex::new(None);

fn err(e: impl std::fmt::Display) -> Error {
    Error::from_reason(e.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn open_replica(dir: &Path) -> std::result::Result<DemoReplica, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let log_path = dir.join("sod.log");
    let id_path = dir.join("replica_id");
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
        (Bag::new("notes"), Count::new("count")),
        Watermark::new(),
    );
    Replica::open(id, log, engine).map_err(|e| e.to_string())
}

fn with_replica<T>(f: impl FnOnce(&mut DemoReplica) -> Result<T>) -> Result<T> {
    let mut guard = REPLICA.lock().map_err(|_| err("replica mutex poisoned"))?;
    let replica = guard.as_mut().ok_or_else(|| err("call open(dir) first"))?;
    f(replica)
}

fn commit(note: String, mult: i64) -> Result<()> {
    let datum = postcard::to_stdvec(&note).map_err(err)?;
    with_replica(|r| {
        r.commit(vec![(datum, mult)], now_ms()).map_err(err)?;
        Ok(())
    })
}

/// Open (or create) the replica at `dir`. One replica per process.
#[napi]
pub fn open(dir: String) -> Result<String> {
    let replica = open_replica(Path::new(&dir)).map_err(err)?;
    let id = replica.id().to_string();
    *REPLICA.lock().map_err(|_| err("replica mutex poisoned"))? = Some(replica);
    Ok(id)
}

/// Insert one copy of `note`.
#[napi]
pub fn add(note: String) -> Result<()> {
    commit(note, 1)
}

/// Retract one copy of `note`. Errors if the note is not present — an
/// unmatched retraction would store a hidden negative multiplicity that
/// swallows a future add.
#[napi]
pub fn remove(note: String) -> Result<()> {
    let present =
        with_replica(|r| Ok(r.engine().stream().rtx(|(bag, _count)| bag.contains(&note))))?;
    if !present {
        return Err(err(format!("no such note: {note}")));
    }
    commit(note, -1)
}

/// All notes as `"<count>x <text>"`, in canonical (postcard-key) order.
#[napi]
pub fn list() -> Result<Vec<String>> {
    with_replica(|r| {
        Ok(r.engine().stream().rtx(|(bag, _count)| {
            bag.iter().map(|(note, n)| format!("{n}x {note}")).collect()
        }))
    })
}

/// Total note count.
#[napi]
pub fn count() -> Result<i64> {
    with_replica(|r| Ok(r.engine().stream().rtx(|(_bag, count)| count.get())))
}

/// Run one full sync session with a peer (e.g. `ws://127.0.0.1:7171`).
/// Returns any per-origin refusals recorded while the session continued
/// (equivocating or poisoned feeds) — surface these to the user.
#[napi]
pub fn sync_with_peer(url: String) -> Result<Vec<String>> {
    with_replica(|r| {
        let skipped = sync_with(&url, r, SCHEMA).map_err(err)?;
        Ok(skipped.iter().map(|s| s.to_string()).collect())
    })
}

/// Accept exactly one sync session on `addr`, then return.
#[napi]
pub fn serve_once(addr: String) -> Result<()> {
    with_replica(|r| serve(&addr, r, SCHEMA, Some(1)).map_err(err))
}

/// Close the replica (drops the store handles).
#[napi]
pub fn close() -> Result<()> {
    *REPLICA.lock().map_err(|_| err("replica mutex poisoned"))? = None;
    Ok(())
}
