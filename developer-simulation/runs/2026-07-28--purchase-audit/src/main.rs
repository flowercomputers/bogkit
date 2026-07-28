use std::fs;
use std::time::Instant;

use purchase_audit_comparison::{
    AuditError, BaselineModel, FailPoint, RequestStatus, fold_retraction_count,
    reproduce_split_commit,
};

fn main() {
    let mut baseline = BaselineModel::new();
    baseline
        .create(1, 125_000, "priya", "policy-2026.1", FailPoint::Never)
        .expect("create should commit");
    baseline
        .approve(1, "sam", "policy-2026.2", FailPoint::Never)
        .expect("approve should commit");

    let before_failed_write = baseline.audit_len();
    let forced_failure =
        baseline.create(2, 80_000, "priya", "policy-2026.1", FailPoint::BeforeCommit);
    assert_eq!(forced_failure, Err(AuditError::InjectedFailure));
    assert!(baseline.request(2).is_none());
    assert_eq!(baseline.audit_len(), before_failed_write);

    let split_path = data_path("run-split");
    let split = reproduce_split_commit(&split_path);
    assert_eq!(split.current_state, RequestStatus::Approved);
    assert_eq!(split.fold_events.len(), 1);

    let retraction_path = data_path("run-retraction");
    let after_retraction = fold_retraction_count(&retraction_path);
    assert_eq!(after_retraction, 0);

    let mut query_model = BaselineModel::new();
    for id in 1..=5_000 {
        query_model
            .create(id, id * 100, "seed", "p1", FailPoint::Never)
            .expect("seed create should commit");
        match id % 3 {
            0 => query_model
                .approve(id, "seed", "p2", FailPoint::Never)
                .expect("seed approval should commit"),
            1 => query_model
                .reject(id, "seed", "p2", FailPoint::Never)
                .expect("seed rejection should commit"),
            _ => query_model
                .cancel(id, "seed", "p2", FailPoint::Never)
                .expect("seed cancellation should commit"),
        }
    }
    let started = Instant::now();
    let result_count = query_model.all_timeline(10_000).len();
    let elapsed = started.elapsed();

    println!("Decision: use the PostgreSQL baseline; BogKit is not a fit.");
    println!(
        "Baseline forced failure: request absent, audit count unchanged at {}.",
        baseline.audit_len()
    );
    println!(
        "Baseline timeline: {:?}.",
        baseline
            .timeline(1)
            .iter()
            .map(|event| event.action)
            .collect::<Vec<_>>()
    );
    println!(
        "Split-store failure: current state {:?}, Fold has {} older event.",
        split.current_state,
        split.fold_events.len()
    );
    println!("Fold writer retraction: {after_retraction} events remain.");
    println!("Local-model query: {result_count} events in {elapsed:?}.");
}

fn data_path(name: &str) -> std::path::PathBuf {
    let path = std::env::current_dir()
        .expect("current directory should be available")
        .join("target")
        .join("run-data")
        .join(name);
    if path.exists() {
        fs::remove_dir_all(&path).expect("old run data should be removable");
    }
    path
}
