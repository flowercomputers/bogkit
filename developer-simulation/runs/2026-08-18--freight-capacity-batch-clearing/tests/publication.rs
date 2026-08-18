use std::fs;

use freight_clearing::{FailurePoint, RunPaths, run_files};
use sha2::{Digest, Sha256};

fn write_valid_snapshot(directory: &std::path::Path) -> RunPaths {
    let orders = directory.join("orders.ndjson");
    let manifest = directory.join("manifest.json");
    let proposal = directory.join("proposal.ndjson");
    let order_bytes = concat!(
        "{\"order_id\":\"s1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"sell\",\"submitted_sequence\":2,\"quantity\":3,\"limit_price_cents\":90}\n",
        "{\"order_id\":\"b1\",\"lane_id\":\"lane\",\"service_date\":\"2026-09-01\",\"side\":\"buy\",\"submitted_sequence\":1,\"quantity\":3,\"limit_price_cents\":100}\n",
    )
    .as_bytes();
    fs::write(&orders, order_bytes).unwrap();
    let order_digest = format!("{:x}", Sha256::digest(order_bytes));
    fs::write(
        &manifest,
        format!(
            "{{\"order_count\":2,\"buy_count\":1,\"sell_count\":1,\"market_count\":1,\"max_fill_count\":1,\"expected_gross_value_cents\":270,\"orders_sha256\":\"{order_digest}\"}}\n"
        ),
    )
    .unwrap();
    RunPaths {
        orders,
        manifest,
        proposal,
    }
}

#[test]
fn exit_after_temp_sync_preserves_previous_proposal_and_removes_temp_file() {
    // Catches writing to the final path early or leaking an ambiguous sibling
    // file after a crash-window simulation.
    let directory = tempfile::tempdir().unwrap();
    let paths = write_valid_snapshot(directory.path());
    fs::write(&paths.proposal, b"previous-complete-proposal\n").unwrap();

    let error = run_files(&paths, FailurePoint::AfterTempSync)
        .expect_err("injected pre-rename exit must fail");

    assert!(error.contains("injected"), "{error}");
    assert_eq!(
        fs::read(&paths.proposal).unwrap(),
        b"previous-complete-proposal\n"
    );
    let names: Vec<_> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 3, "temporary output leaked: {names:?}");
}

#[test]
fn every_injected_prepublication_exit_preserves_previous_bytes() {
    // Catches exposing partially cleared output at the final path.
    for failure in [
        FailurePoint::AfterValidation,
        FailurePoint::AfterOnePercent,
        FailurePoint::AfterHalf,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let paths = write_valid_snapshot(directory.path());
        fs::write(&paths.proposal, b"stable\n").unwrap();

        run_files(&paths, failure).expect_err("injected exit must fail");

        assert_eq!(fs::read(&paths.proposal).unwrap(), b"stable\n");
    }
}

#[test]
fn shuffled_input_and_repeat_runs_publish_identical_bytes() {
    // Catches arrival-order or process-state leakage into canonical output.
    let directory = tempfile::tempdir().unwrap();
    let paths = write_valid_snapshot(directory.path());

    let first = run_files(&paths, FailurePoint::None).expect("first run");
    let first_bytes = fs::read(&paths.proposal).unwrap();
    let lines: Vec<_> = fs::read_to_string(&paths.orders)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let shuffled = format!("{}\n{}\n", lines[1], lines[0]);
    fs::write(&paths.orders, &shuffled).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.manifest).unwrap()).unwrap();
    manifest["orders_sha256"] =
        serde_json::Value::String(format!("{:x}", Sha256::digest(shuffled.as_bytes())));
    fs::write(
        &paths.manifest,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let second = run_files(&paths, FailurePoint::None).expect("shuffled run");
    let second_bytes = fs::read(&paths.proposal).unwrap();
    let third = run_files(&paths, FailurePoint::None).expect("repeat run");

    assert_eq!(first_bytes, second_bytes);
    assert_eq!(first.proposal_sha256, second.proposal_sha256);
    assert_eq!(second.proposal_sha256, third.proposal_sha256);
}

#[test]
fn forced_read_and_write_errors_never_replace_a_prior_proposal() {
    // Catches error paths that touch the final output before all I/O succeeds.
    let directory = tempfile::tempdir().unwrap();
    let mut paths = write_valid_snapshot(directory.path());
    fs::write(&paths.proposal, b"prior\n").unwrap();
    paths.orders = directory.path().to_path_buf();
    run_files(&paths, FailurePoint::None).expect_err("directory read must fail");
    assert_eq!(fs::read(&paths.proposal).unwrap(), b"prior\n");

    let paths = write_valid_snapshot(directory.path());
    let unwritable = RunPaths {
        proposal: directory.path().join("missing-parent/proposal.ndjson"),
        ..paths
    };
    run_files(&unwritable, FailurePoint::None).expect_err("missing output parent must fail");
    assert!(!unwritable.proposal.exists());
}
