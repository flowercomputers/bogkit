use reach_rollup_lab::{run_candidate_benchmark, run_exact_benchmark};

#[test]
#[ignore = "representative 12-million-record benchmark; run explicitly in release mode"]
fn benchmark_modes_process_the_identical_complete_corpus() {
    // Catches benchmark modes that compare different inputs or optimize work away.
    let exact = run_exact_benchmark().unwrap();
    let candidate = run_candidate_benchmark().unwrap();
    assert_eq!(exact.record_count, 12_000_000);
    assert_eq!(candidate.record_count, exact.record_count);
    assert_eq!(candidate.bucket_count, exact.bucket_count);
    assert_eq!(exact.unique_bucket_occurrences, Some(10_800_000));
    assert_eq!(candidate.max_serialized_bucket_size, Some(4_124));
    assert_ne!(exact.digest, 0);
    assert_ne!(candidate.digest, 0);
}
