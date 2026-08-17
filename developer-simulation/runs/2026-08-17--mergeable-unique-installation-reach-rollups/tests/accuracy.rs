use reach_rollup_lab::run_accuracy_matrix;

#[test]
fn full_accuracy_matrix_meets_the_declared_contract() {
    // Catches a candidate that succeeds only on handpicked cardinalities or seeds.
    let run = run_accuracy_matrix(0xbb67_ae85_84ca_a73b, 0x1319_8a2e_0370_7344);
    assert_eq!(run.observations.len(), 2_880);
    assert!(
        run.summary.median_absolute_error <= 0.015,
        "{:?}",
        run.summary
    );
    assert!(run.summary.p95_absolute_error <= 0.035, "{:?}", run.summary);
    assert!(
        run.summary.worst_absolute_error <= 0.08,
        "{:?}",
        run.summary
    );
    assert!(run.summary.positive_count > 0);
    assert!(run.summary.negative_count > 0);

    let evidence = run.to_tsv();
    assert_eq!(evidence.lines().count(), 2_881);
    assert!(evidence.starts_with(
        "seed\tcardinality\tbucket\testimate\tsigned_relative_error\tabsolute_relative_error\n"
    ));
    assert_eq!(evidence, run.to_tsv());
}
