//! Golden log-format test: the on-disk/wire record format is frozen.
//!
//! `golden.sodlog` was generated once by `regenerate_golden_fixture` (an
//! ignored test). Any change to the frame or record encoding makes
//! `golden_log_parses_byte_identically` fail; changing the format is a
//! deliberate act — bump the sync protocol version, then regenerate with
//! `cargo test -p sod --test golden_log -- --ignored`.

use sod::{Frame, ReplicaId, ZERO_HASH, decode_record};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/golden.sodlog");

fn golden_frames() -> Vec<Frame> {
    let f1 = Frame {
        prev_hash: ZERO_HASH,
        origin: ReplicaId([0xAB; 16]),
        seq: 1,
        event_time: 1_755_216_000_000,
        payload: vec![(b"golden datum one".to_vec(), 1), (b"two".to_vec(), -3)],
    };
    let f2 = Frame {
        prev_hash: f1.hash(),
        origin: ReplicaId([0xAB; 16]),
        seq: 2,
        event_time: 1_755_216_000_001,
        payload: vec![(b"three".to_vec(), 2)],
    };
    vec![f1, f2]
}

fn encode_all(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frames {
        f.encode_record(&mut out);
    }
    out
}

#[test]
fn golden_log_parses_byte_identically() {
    let bytes = std::fs::read(FIXTURE).expect("golden fixture missing");
    let expected = golden_frames();

    // The checked-in bytes decode to exactly the expected frames...
    let mut rest = &bytes[..];
    let mut decoded = Vec::new();
    while !rest.is_empty() {
        let (frame, hash, consumed) = decode_record(rest)
            .expect("golden fixture must decode")
            .expect("golden fixture must not end in a partial record");
        assert_eq!(hash, frame.hash());
        decoded.push(frame);
        rest = &rest[consumed..];
    }
    assert_eq!(decoded, expected);

    // ...and today's encoder reproduces the checked-in bytes exactly.
    assert_eq!(encode_all(&expected), bytes, "record encoding drifted");
}

#[test]
#[ignore = "regenerates the golden fixture; run only on a deliberate format change"]
fn regenerate_golden_fixture() {
    std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).unwrap();
    std::fs::write(FIXTURE, encode_all(&golden_frames())).unwrap();
}
