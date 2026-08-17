use reach_rollup_lab::{BucketKey, DayWindow, ExactRollup, Record};

#[test]
fn exact_rollup_counts_unique_ids_per_bucket() {
    // Catches replacing set insertion with a raw record counter.
    let window = DayWindow::new(0, 0, 1).unwrap();
    let mut rollup = ExactRollup::new(window);
    let a = Record::new(0, 0, [1; 16]);
    let duplicate_a = Record::new(0, 0, [1; 16]);
    let b = Record::new(0, 0, [2; 16]);

    rollup.add(a).unwrap();
    rollup.add(duplicate_a).unwrap();
    rollup.add(b).unwrap();

    assert_eq!(rollup.count(BucketKey::new(0, 0)), Some(2));
}

#[test]
fn exact_rollup_rejects_records_outside_declared_day_and_tenants() {
    // Catches accepting bad routing keys and silently publishing a partial day.
    let window = DayWindow::new(100, 7, 9).unwrap();
    let mut rollup = ExactRollup::new(window);

    assert!(rollup.add(Record::new(7, 99, [0; 16])).is_err());
    assert!(rollup.add(Record::new(10, 100, [0; 16])).is_err());
    assert!(rollup.add(Record::new(9, 123, [0; 16])).is_ok());
    assert!(rollup.add(Record::new(9, 124, [0; 16])).is_err());
    assert!(rollup.add(Record::new(7, i64::MIN, [0; 16])).is_err());
    assert!(rollup.add(Record::new(7, i64::MAX, [0; 16])).is_err());
}

#[test]
fn day_window_accepts_full_u32_tenant_range() {
    // Catches range validation that overflows or excludes either declared edge.
    let window = DayWindow::new(-12, u32::MIN, u32::MAX).unwrap();
    let mut rollup = ExactRollup::new(window);
    rollup.add(Record::new(u32::MIN, -12, [3; 16])).unwrap();
    rollup.add(Record::new(u32::MAX, 11, [4; 16])).unwrap();
    assert_eq!(rollup.bucket_count(), 2);
}
