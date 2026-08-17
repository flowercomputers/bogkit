use reach_rollup_lab::run_adversarial_verification;

#[test]
fn declared_adversarial_id_and_shard_cases_hold() {
    // Catches hashing or merge behavior that only works for pseudorandom, balanced examples.
    let result = run_adversarial_verification().unwrap();
    assert_eq!(result.id_pattern_cases, 4);
    assert!(result.worst_absolute_error <= 0.08, "{result:?}");
    assert!(result.reverse_order_byte_identical);
    assert!(result.thousand_duplicates_byte_identical);
    assert!(result.hot_shard_merge_byte_identical);
    assert!(result.empty_input_is_empty);
    assert!(result.one_bucket_is_one_bucket);
}
