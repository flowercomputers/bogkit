use std::collections::BTreeMap;

use reach_rollup_lab::{ApproxRollup, BucketKey, DayWindow, ExactRollup, Record};

#[test]
fn candidate_state_file_round_trips_and_rejects_corruption() {
    // Catches a state-file format that cannot safely carry shard results between processes.
    let window = DayWindow::new(10, 4, 4).unwrap();
    let mut rollup = ApproxRollup::new(window, 17);
    rollup.add(Record::new(4, 10, [1; 16])).unwrap();
    rollup.add(Record::new(4, 11, [2; 16])).unwrap();
    let bytes = rollup.state_file_bytes();
    let decoded = ApproxRollup::from_state_file_bytes(window, 17, &bytes).unwrap();
    assert_eq!(decoded.state_file_bytes(), bytes);

    let mut bad_version = bytes.clone();
    bad_version[4] = 9;
    assert!(ApproxRollup::from_state_file_bytes(window, 17, &bad_version).is_err());
    assert!(ApproxRollup::from_state_file_bytes(window, 18, &bytes).is_err());
    assert!(ApproxRollup::from_state_file_bytes(window, 17, &bytes[..bytes.len() - 1]).is_err());
}

#[test]
fn state_file_checksum_binds_valid_in_window_tenant_and_hour_keys() {
    // Catches corruption that silently reassigns valid sketch bytes to another valid bucket.
    let window = DayWindow::new(0, 0, 1).unwrap();
    let mut rollup = ApproxRollup::new(window, 17);
    rollup.add(Record::new(0, 0, [3; 16])).unwrap();
    let bytes = rollup.state_file_bytes();
    assert_eq!(bytes[4], 2, "the complete-binding format must be v2");

    let mut changed_tenant = bytes.clone();
    changed_tenant[20..24].copy_from_slice(&1_u32.to_le_bytes());
    assert!(ApproxRollup::from_state_file_bytes(window, 17, &changed_tenant).is_err());

    let mut changed_hour = bytes.clone();
    changed_hour[24..32].copy_from_slice(&1_i64.to_le_bytes());
    assert!(ApproxRollup::from_state_file_bytes(window, 17, &changed_hour).is_err());

    let mut changed_file_checksum = bytes.clone();
    changed_file_checksum[12] ^= 1;
    assert!(ApproxRollup::from_state_file_bytes(window, 17, &changed_file_checksum).is_err());
}

#[test]
fn compact_exact_counts_still_drive_complete_report_comparison() {
    // Catches a scale harness that keeps raw IDs alive or loses oracle completeness after compaction.
    let window = DayWindow::new(0, 0, 0).unwrap();
    let mut exact = ExactRollup::new(window);
    let mut approx = ApproxRollup::new(window, 17);
    for value in 0_u8..10 {
        let record = Record::new(0, 0, [value; 16]);
        exact.add(record).unwrap();
        approx.add(record).unwrap();
    }
    let counts = exact.into_counts();
    assert_eq!(counts.get(&BucketKey::new(0, 0)), Some(&10));
    let report = approx.report_from_counts(&counts).unwrap();
    assert!(String::from_utf8(report).unwrap().contains("0\t0\t10\t"));

    let missing = BTreeMap::new();
    assert!(approx.report_from_counts(&missing).is_err());
}
