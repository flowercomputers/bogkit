//! sod-demo: a replicated notes bag — the smallest complete sod app, and
//! the template for building your own (see the README).
//!
//! Each data directory is one replica ("a sod"): an append-only frame log
//! (`sod.log`), a fold database (`db/`, rebuildable cache), and the
//! replica's identity (`replica_id`, freshly generated whenever the log is
//! created — SOD-3). Any two directories converge by syncing, in either
//! direction, through any chain of peers.
//!
//! ```console
//! $ cargo run -p sod-demo -- ./a add hello from a
//! $ cargo run -p sod-demo -- ./b serve 127.0.0.1:7171   # terminal 1
//! $ cargo run -p sod-demo -- ./a sync ws://127.0.0.1:7171  # terminal 2
//! $ cargo run -p sod-demo -- ./b list
//! ```

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fold::pipeline::terminal::{Bag, Count};
use sod::engine_fold::FoldEngine;
use sod::log_file::FileLog;
use sod::time::Watermark;
use sod::transport::ws::{serve, sync_with};
use sod::{Replica, ReplicaId};

type Pipeline = (Bag<String>, Count);
type DemoReplica = Replica<FoldEngine<String, Pipeline>, FileLog>;

/// Bump when the pipeline or datum type changes shape: replicas with
/// different schemas refuse to sync instead of corrupting (SOD-9).
const SCHEMA: u32 = 1;

fn open(dir: &Path) -> DemoReplica {
    std::fs::create_dir_all(dir).expect("cannot create data dir");
    let log_path = dir.join("sod.log");
    let id_path = dir.join("replica_id");

    // SOD-3: the replica id never outlives the log. A missing log with a
    // leftover id (or db) means the replica was reset — start identity and
    // cache from scratch.
    if !log_path.exists() {
        let _ = std::fs::remove_file(&id_path);
        let _ = std::fs::remove_dir_all(dir.join("db"));
    }
    let id = match std::fs::read(&id_path) {
        Ok(bytes) => ReplicaId(bytes.as_slice().try_into().expect("replica_id is 16 bytes")),
        Err(_) => {
            let id = ReplicaId::generate();
            std::fs::write(&id_path, id.0).expect("cannot persist replica_id");
            id
        }
    };

    let log = FileLog::open(&log_path).unwrap_or_else(|e| panic!("log open failed: {e}"));
    let engine = FoldEngine::open(
        dir.join("db"),
        (Bag::new("notes"), Count::new("count")),
        Watermark::new(),
    );
    Replica::open(id, log, engine).unwrap_or_else(|e| panic!("replica open failed: {e}"))
}

/// Applications stamp event time; sod itself never reads a clock (SOD-7).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// The datum wire format is the pipeline type's postcard encoding.
fn encode(note: &str) -> Vec<u8> {
    postcard::to_stdvec(&note.to_string()).unwrap()
}

fn usage() -> ! {
    eprintln!(
        "usage: sod-demo <dir> <command>\n\
         commands:\n  \
         add <text...>     insert a note\n  \
         remove <text...>  retract a note\n  \
         list              print notes and total count\n  \
         serve <addr>      accept sync sessions (e.g. 127.0.0.1:7171)\n  \
         sync <url>        sync once with a peer (e.g. ws://127.0.0.1:7171)"
    );
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, cmd, rest) = match args.split_first() {
        Some((dir, rest)) => match rest.split_first() {
            Some((cmd, rest)) => (PathBuf::from(dir), cmd.as_str(), rest),
            None => usage(),
        },
        None => usage(),
    };
    let mut replica = open(&dir);

    match cmd {
        "add" | "remove" if !rest.is_empty() => {
            let note = rest.join(" ");
            let mult = if cmd == "add" { 1 } else { -1 };
            replica
                .commit(vec![(encode(&note), mult)], now_ms())
                .unwrap_or_else(|e| panic!("commit failed: {e}"));
            println!("{cmd}ed: {note}");
        }
        "list" => {
            let (notes, total) = replica
                .engine()
                .stream()
                .rtx(|(bag, count)| (bag.iter().collect::<Vec<_>>(), count.get()));
            for (note, n) in notes {
                println!("{n}\u{d7} {note}");
            }
            println!("-- {total} note(s), replica {}", replica.id());
        }
        "serve" => {
            let addr = rest.first().map(String::as_str).unwrap_or_else(|| usage());
            println!("replica {} serving on {addr} (ctrl-c to stop)", replica.id());
            serve(addr, &mut replica, SCHEMA, None)
                .unwrap_or_else(|e| panic!("serve failed: {e}"));
        }
        "sync" => {
            let url = rest.first().map(String::as_str).unwrap_or_else(|| usage());
            sync_with(url, &mut replica, SCHEMA)
                .unwrap_or_else(|e| panic!("sync failed: {e}"));
            println!("synced with {url}");
        }
        _ => usage(),
    }
}
