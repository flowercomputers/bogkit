//! FoldEngine integration: counts through fold sinks, exact crash healing
//! via the transactional cursor, and the MemEngine differential oracle.
#![cfg(feature = "fold-engine")]

use fold::pipeline::terminal::{Bag, Count};
use sod::engine::{Engine, MemEngine};
use sod::engine_fold::FoldEngine;
use sod::log_file::FileLog;
use sod::store::LogStore;
use sod::time::Watermark;
use sod::{Frame, Replica, ReplicaId};

type Pipeline = (Bag<String>, Count);
type DemoEngine = FoldEngine<String, Pipeline>;

fn pipeline() -> Pipeline {
    (Bag::new("notes"), Count::new("count"))
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sod-fold-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_replica(dir: &std::path::Path, me: ReplicaId) -> Replica<DemoEngine, FileLog> {
    let log = FileLog::open(dir.join("sod.log")).unwrap();
    let engine = FoldEngine::open(dir.join("db"), pipeline(), Watermark::new());
    Replica::open(me, log, engine).unwrap()
}

fn datum(s: &str) -> Vec<u8> {
    postcard::to_stdvec(&s.to_string()).unwrap()
}

fn bag_contents(r: &Replica<DemoEngine, FileLog>) -> Vec<(String, i64)> {
    r.engine().stream().rtx(|(bag, _count)| bag.iter().collect())
}

#[test]
fn fold_replica_counts() {
    let dir = tmp("counts");
    let me = ReplicaId([1; 16]);
    let mut r = open_replica(&dir, me);
    r.commit(vec![(datum("hello"), 1)], 10).unwrap();
    r.commit(vec![(datum("world"), 2), (datum("hello"), -1)], 20).unwrap();

    let notes = bag_contents(&r);
    assert_eq!(notes, vec![("world".to_string(), 2)]);
    let total = r.engine().stream().rtx(|(_bag, count)| count.get());
    assert_eq!(total, 2); // +1 +2 -1
    assert_eq!(r.engine().watermark().get(), 20);

    // reopen: nothing replays (cursor is up to date), state persists
    drop(r);
    let r = open_replica(&dir, me);
    assert_eq!(r.engine().applied().get(&me), 2);
    assert_eq!(bag_contents(&r), vec![("world".to_string(), 2)]);
    assert_eq!(r.engine().watermark().get(), 20, "watermark reseeded from log");
}

#[test]
fn crash_between_log_and_apply_heals() {
    let dir = tmp("crash");
    let me = ReplicaId([2; 16]);
    let first = {
        let mut r = open_replica(&dir, me);
        r.commit(vec![(datum("durable"), 1)], 10).unwrap();
        r.frames_after(&me, 0)[0].clone()
    };

    // Simulate the crash window: frame 2 reaches the log, the engine never
    // sees it (process dies between append and apply).
    {
        let mut log = FileLog::open(dir.join("sod.log")).unwrap();
        let orphan = Frame {
            prev_hash: first.hash(),
            origin: me,
            seq: 2,
            event_time: 20,
            payload: vec![(datum("orphan"), 3)],
        };
        log.append(&orphan).unwrap();
        log.sync().unwrap();
    }

    // Reopen: the cursor says seq 1 is applied, so exactly the orphan replays.
    let r = open_replica(&dir, me);
    assert_eq!(r.vector().get(&me), 2);
    assert_eq!(r.engine().applied().get(&me), 2);
    // Bag iterates in postcard-key order: length prefix first, so the
    // 6-byte "orphan" sorts before the 7-byte "durable".
    let notes = bag_contents(&r);
    assert_eq!(
        notes,
        vec![("orphan".to_string(), 3), ("durable".to_string(), 1)]
    );
}

#[test]
fn differential_vs_mem() {
    let dir = tmp("differential");
    let me = ReplicaId([3; 16]);
    let mut r = open_replica(&dir, me);
    let mut oracle = MemEngine::new();

    // A deterministic pseudo-random mix of inserts and retractions.
    let words = ["ash", "bog", "fen", "moss", "peat", "sedge"];
    let mut x = 42u64;
    for step in 0..120u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let word = words[(x % 6) as usize];
        let mult = [1i64, 2, -1][(x % 3) as usize];
        r.commit(vec![(datum(word), mult)], step).unwrap();
    }
    for f in r.frames_after(&me, 0) {
        oracle.apply(f, f.event_time).unwrap();
    }

    // The Bag reader surfaces only positive multiplicities; the oracle
    // keeps the full Z-set. Compare on the positive projection.
    let fold_side: std::collections::BTreeMap<Vec<u8>, i64> = bag_contents(&r)
        .into_iter()
        .map(|(w, c)| (postcard::to_stdvec(&w).unwrap(), c))
        .collect();
    let mem_side: std::collections::BTreeMap<Vec<u8>, i64> = oracle
        .iter()
        .filter(|(_, c)| **c > 0)
        .map(|(k, c)| (k.clone(), *c))
        .collect();
    assert_eq!(fold_side, mem_side, "fold Bag diverged from MemEngine oracle");
    assert_eq!(r.engine().watermark().get(), oracle.watermark());
}
