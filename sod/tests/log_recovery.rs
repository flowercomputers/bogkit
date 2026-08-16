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
    let log = FileLog::open(&path).unwrap();
    assert_eq!(log.frames(), &frames[..]);
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
        assert_eq!(log.frames(), &frames[..2], "cut={cut}");
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            two.len() as u64,
            "file restored to last valid boundary, cut={cut}"
        );

        // appends still work after recovery
        log.append(&frames[2]).unwrap();
        log.sync().unwrap();
        drop(log);
        let log = FileLog::open(&path).unwrap();
        assert_eq!(log.frames(), &frames[..]);
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
