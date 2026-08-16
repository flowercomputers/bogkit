//! FileLog torn-tail recovery and crash-healing tests (SOD-5).

use sod::log_file::FileLog;
use sod::store::LogStore;
use sod::{Frame, FrameHash, ReplicaId, SodError, ZERO_HASH};

fn frame(origin: u8, seq: u64, prev: FrameHash) -> Frame {
    Frame {
        prev_hash: prev,
        origin: ReplicaId([origin; 16]),
        seq,
        event_time: 1_000 + seq,
        payload: vec![(format!("datum-{origin}-{seq}").into_bytes(), 1)],
    }
}

fn chain(origin: u8, n: u64) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut prev = ZERO_HASH;
    for seq in 1..=n {
        let f = frame(origin, seq, prev);
        prev = f.hash();
        frames.push(f);
    }
    frames
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sod-log-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("sod.log")
}

#[test]
fn roundtrip_reopen() {
    let path = tmp("roundtrip");
    let frames = chain(1, 3);
    {
        let mut log = FileLog::open(&path).unwrap();
        for f in &frames {
            log.append(f).unwrap();
        }
        log.sync().unwrap();
    }
    let mut log = FileLog::open(&path).unwrap();
    assert_eq!(log.take_frames(), frames);
}

#[test]
fn torn_tail_truncated() {
    let frames = chain(1, 3);
    let mut full = Vec::new();
    for f in &frames {
        f.encode_record(&mut full);
    }
    let mut two = Vec::new();
    for f in &frames[..2] {
        f.encode_record(&mut two);
    }
    let last_len = full.len() - two.len();

    for cut in 1..=last_len {
        let path = tmp(&format!("torn-{cut}"));
        std::fs::write(&path, &full[..full.len() - cut]).unwrap();

        let mut log = FileLog::open(&path).unwrap();
        assert_eq!(log.take_frames(), frames[..2], "cut={cut}");
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            two.len() as u64,
            "file restored to last valid boundary, cut={cut}"
        );

        // appends still work after recovery
        log.append(&frames[2]).unwrap();
        log.sync().unwrap();
        drop(log);
        let mut log = FileLog::open(&path).unwrap();
        assert_eq!(log.take_frames(), frames);
    }
}

#[test]
fn corrupted_length_prefix_refuses_instead_of_truncating() {
    // A bit flip in a record's length prefix must be detected as interior
    // corruption — the old format misread it as a clean torn tail and
    // silently truncated every valid frame after it (regressing the
    // version vector, which peers then treat as equivocation).
    let frames = chain(1, 3);
    let mut bytes = Vec::new();
    for f in &frames {
        f.encode_record(&mut bytes);
    }
    for i in 0..4 {
        let path = tmp(&format!("lenflip-{i}"));
        let mut bad = bytes.clone();
        bad[i] ^= 0xff; // frame 1's length prefix
        std::fs::write(&path, &bad).unwrap();
        match FileLog::open(&path).err() {
            Some(SodError::Corrupt(_)) => {}
            other => panic!("expected Corrupt, got {other:?} (flip at {i})"),
        }
        // and the file was not touched
        assert_eq!(std::fs::read(&path).unwrap(), bad);
    }
}

#[test]
fn replica_crash_heals_by_replay_and_truncation() {
    use sod::Replica;
    use sod::engine::MemEngine;

    let path = tmp("replica-crash");
    let me = ReplicaId([9; 16]);

    // Two committed writes, then "crash": the engine (in-memory) is lost.
    {
        let log = FileLog::open(&path).unwrap();
        let mut r = Replica::open(me, log, MemEngine::new()).unwrap();
        r.commit(vec![(b"a".to_vec(), 1)], 10).unwrap();
        r.commit(vec![(b"b".to_vec(), 2)], 20).unwrap();
    }
    // Reopen with a fresh engine: replay restores everything (SOD-1/SOD-5).
    {
        let log = FileLog::open(&path).unwrap();
        let r = Replica::open(me, log, MemEngine::new()).unwrap();
        assert_eq!(r.engine().count(b"a"), 1);
        assert_eq!(r.engine().count(b"b"), 2);
        assert_eq!(r.vector().get(&me), 2);
        assert_eq!(r.watermark(), 20);
    }
    // A third commit torn mid-append: chop bytes off the file tail.
    {
        let log = FileLog::open(&path).unwrap();
        let mut r = Replica::open(me, log, MemEngine::new()).unwrap();
        r.commit(vec![(b"c".to_vec(), 1)], 30).unwrap();
    }
    let full = std::fs::read(&path).unwrap();
    std::fs::write(&path, &full[..full.len() - 7]).unwrap();
    // Reopen: the torn third frame is truncated, state equals the durable
    // two-frame prefix, and committing resumes at seq 3.
    {
        let log = FileLog::open(&path).unwrap();
        let mut r = Replica::open(me, log, MemEngine::new()).unwrap();
        assert_eq!(r.engine().count(b"c"), 0);
        assert_eq!(r.vector().get(&me), 2);
        r.commit(vec![(b"c".to_vec(), 5)], 40).unwrap();
        assert_eq!(r.vector().get(&me), 3);
        assert_eq!(r.engine().count(b"c"), 5);
    }
}

#[test]
fn mid_file_corruption_refuses() {
    let path = tmp("midcorrupt");
    let frames = chain(1, 3);
    let mut bytes = Vec::new();
    for f in &frames {
        f.encode_record(&mut bytes);
    }
    // flip a byte inside frame 1's body (past its 4-byte length prefix)
    bytes[10] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();

    match FileLog::open(&path).err() {
        Some(SodError::Corrupt(_)) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }
    // and the file was not touched
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
