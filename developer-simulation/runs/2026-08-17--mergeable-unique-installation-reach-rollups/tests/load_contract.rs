use std::fs;

use reach_rollup_lab::run_load_verification;

#[test]
fn complete_load_corpus_meets_non_performance_contract() {
    // Catches a scale-only drift in cardinalities, duplicates, mergeability, or determinism.
    let temporary =
        std::env::temp_dir().join(format!("reach-load-contract-{}", std::process::id()));
    let _ = fs::remove_dir_all(&temporary);
    let result = run_load_verification(&temporary).unwrap();
    assert_eq!(result.record_count, 12_000_000);
    assert_eq!(result.unique_bucket_occurrences, 10_800_000);
    assert_eq!(result.bucket_count, 5_760);
    assert_eq!(result.cardinality_1k_buckets, 4_800);
    assert_eq!(result.cardinality_5k_buckets, 720);
    assert_eq!(result.cardinality_10k_buckets, 240);
    assert!(result.duplicates_byte_identical);
    assert!(result.direct_and_sharded_byte_identical);
    assert_eq!(result.merge_permutation_digests.len(), 50);
    assert!(
        result
            .merge_permutation_digests
            .iter()
            .all(|digest| *digest == result.state_digest)
    );
    assert!(result.repeated_state_byte_identical);
    assert!(result.repeated_report_byte_identical);
    assert_eq!(result.max_serialized_bucket_size, 4_124);
    assert!(result.max_serialized_bucket_size <= 4_160);
    assert_eq!(result.total_serialized_bucket_state, 23_754_240);
    assert_eq!(
        String::from_utf8_lossy(&result.report_bytes)
            .lines()
            .count(),
        5_761
    );
    assert!(!temporary.exists(), "temporary shard states were retained");
}
