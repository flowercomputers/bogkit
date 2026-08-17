use std::fs;

use reach_rollup_lab::{
    ApproxRollup, DayWindow, ExactRollup, PublishMode, Record, digest_bytes, publish_report,
};

const HASH_SEED: u64 = 0xbb67_ae85_84ca_a73b;

fn records() -> Vec<Record> {
    let mut records = Vec::new();
    for tenant in 0_u32..2 {
        for hour in 0_u32..2 {
            for id in 0_u64..30 {
                let mut installation = [0_u8; 16];
                installation[..8].copy_from_slice(&id.to_le_bytes());
                installation[8..12].copy_from_slice(&tenant.to_le_bytes());
                installation[12..].copy_from_slice(&hour.to_le_bytes());
                records.push(Record::new(tenant, i64::from(hour), installation));
                if id % 3 == 0 {
                    records.push(Record::new(tenant, i64::from(hour), installation));
                }
            }
        }
    }
    records
}

fn shuffled<T>(mut values: Vec<T>, seed: u64) -> Vec<T> {
    let mut state = seed;
    for index in (1..values.len()).rev() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let modulus = u64::try_from(index + 1).unwrap();
        let swap = usize::try_from(state.wrapping_mul(0x2545_f491_4f6c_dd1d) % modulus).unwrap();
        values.swap(index, swap);
    }
    values
}

#[test]
fn fifty_input_and_shard_merge_orders_have_identical_state_bytes() {
    // Catches hidden dependence on input order, shard order, or map iteration order.
    let window = DayWindow::new(0, 0, 1).unwrap();
    let source = records();
    let mut reference = ApproxRollup::new(window, HASH_SEED);
    for record in &source {
        reference.add(*record).unwrap();
    }
    let expected = reference.state_file_bytes();

    for seed in 0..50 {
        let mut direct = ApproxRollup::new(window, HASH_SEED);
        for record in shuffled(source.clone(), seed) {
            direct.add(record).unwrap();
        }
        assert_eq!(direct.state_file_bytes(), expected, "input seed {seed}");
    }

    let mut shards: Vec<_> = (0..8)
        .map(|_| ApproxRollup::new(window, HASH_SEED))
        .collect();
    for (index, record) in source.into_iter().enumerate() {
        shards[index % 8].add(record).unwrap();
    }
    for seed in 0..50 {
        let mut merged = ApproxRollup::new(window, HASH_SEED);
        for shard in shuffled(shards.clone(), seed) {
            merged.merge(&shard).unwrap();
        }
        assert_eq!(merged.state_file_bytes(), expected, "merge seed {seed}");
    }
}

#[test]
fn report_is_canonical_complete_and_repeatable() {
    // Catches nondeterministic rows, missing buckets, or digesting a different byte stream.
    let window = DayWindow::new(0, 0, 1).unwrap();
    let mut exact = ExactRollup::new(window);
    let mut approx = ApproxRollup::new(window, HASH_SEED);
    for record in records() {
        exact.add(record).unwrap();
        approx.add(record).unwrap();
    }
    let first = approx.report_bytes(&exact).unwrap();
    let second = approx.report_bytes(&exact).unwrap();
    assert_eq!(first, second);
    assert_eq!(digest_bytes(&first), digest_bytes(&second));
    let text = String::from_utf8(first).unwrap();
    let rows: Vec<_> = text.lines().collect();
    assert_eq!(rows.len(), 5);
    assert!(rows[1].starts_with("0\t0\t30\t"));
    assert!(rows[2].starts_with("0\t1\t30\t"));
    assert!(rows[3].starts_with("1\t0\t30\t"));
    assert!(rows[4].starts_with("1\t1\t30\t"));
}

#[test]
fn invalid_or_interrupted_publication_preserves_previous_complete_report() {
    // Catches writing the final path before validation or before an atomic rename.
    let root = std::env::temp_dir().join(format!("reach-publish-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("report.tsv");
    fs::write(&path, b"previous complete report\n").unwrap();

    let window = DayWindow::new(0, 0, 0).unwrap();
    let mut approx = ApproxRollup::new(window, HASH_SEED);
    approx.add(Record::new(0, 0, [1; 16])).unwrap();
    let empty_exact = ExactRollup::new(window);
    assert!(publish_report(&path, &approx, &empty_exact, PublishMode::Commit).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous complete report\n");

    let mut exact = ExactRollup::new(window);
    exact.add(Record::new(0, 0, [1; 16])).unwrap();
    assert!(publish_report(&path, &approx, &exact, PublishMode::InterruptAfterTemp).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous complete report\n");

    publish_report(&path, &approx, &exact, PublishMode::Commit).unwrap();
    assert!(
        fs::read(&path)
            .unwrap()
            .starts_with(b"tenant_id\tunix_hour")
    );
    fs::remove_dir_all(&root).unwrap();
}
