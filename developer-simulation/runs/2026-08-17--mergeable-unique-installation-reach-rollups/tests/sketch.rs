use reach_rollup_lab::{HllState, LabError};

const SEED: u64 = 0x6a09_e667_f3bc_c909;

fn sequential_id(value: u64) -> [u8; 16] {
    let mut id = [0_u8; 16];
    id[8..].copy_from_slice(&value.to_be_bytes());
    id
}

#[test]
fn estimate_is_close_for_sequential_ids() {
    // Catches a biased hash/rank calculation or missing small-range correction.
    let mut state = HllState::new(SEED);
    for value in 0..10_000 {
        state.add(sequential_id(value));
    }
    let estimate = state.estimate();
    assert!((9_200.0..=10_800.0).contains(&estimate), "{estimate}");
}

#[test]
fn duplicates_and_self_merge_are_byte_idempotent() {
    // Catches counters or non-idempotent merge operations masquerading as a set sketch.
    let mut state = HllState::new(SEED);
    for value in 0..500 {
        state.add(sequential_id(value));
    }
    let original = state.to_bytes();
    for _ in 0..1_000 {
        for value in 0..500 {
            state.add(sequential_id(value));
        }
    }
    assert_eq!(state.to_bytes(), original);
    let copy = HllState::from_bytes(&original).unwrap();
    state.merge(&copy).unwrap();
    assert_eq!(state.to_bytes(), original);
}

#[test]
fn direct_and_merged_states_are_identical() {
    // Catches order-sensitive state updates or a merge that re-estimates counts.
    let mut direct = HllState::new(SEED);
    let mut left = HllState::new(SEED);
    let mut right = HllState::new(SEED);
    for value in 0..5_000 {
        let id = sequential_id(value);
        direct.add(id);
        if value % 2 == 0 {
            left.add(id);
        } else {
            right.add(id);
        }
    }
    left.merge(&right).unwrap();
    assert_eq!(left.to_bytes(), direct.to_bytes());
}

#[test]
fn serialization_is_bounded_and_rejects_incompatible_or_corrupt_state() {
    // Catches loss of format/hash identity and accepting malformed state bytes.
    let mut state = HllState::new(SEED);
    state.add(sequential_id(7));
    let bytes = state.to_bytes();
    assert!(bytes.len() <= 4_160, "state was {} bytes", bytes.len());
    assert_eq!(HllState::from_bytes(&bytes).unwrap().to_bytes(), bytes);

    let mut other_seed = HllState::new(SEED + 1);
    assert!(matches!(
        other_seed.merge(&state),
        Err(LabError::IncompatibleState)
    ));

    let mut unknown_version = bytes.clone();
    unknown_version[4] = 2;
    assert!(HllState::from_bytes(&unknown_version).is_err());

    let mut impossible_register = bytes.clone();
    *impossible_register.last_mut().unwrap() = 255;
    assert!(HllState::from_bytes(&impossible_register).is_err());

    let mut checksum_mismatch = bytes.clone();
    checksum_mismatch[28] ^= 1;
    assert!(HllState::from_bytes(&checksum_mismatch).is_err());
    assert!(HllState::from_bytes(&bytes[..bytes.len() - 1]).is_err());
}
