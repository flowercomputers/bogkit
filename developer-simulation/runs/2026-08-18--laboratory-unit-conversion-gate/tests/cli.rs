use std::process::Command;

use tempfile::tempdir;

#[test]
fn benchmark_command_emits_one_machine_readable_summary() {
    let dir = tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_lab-unit-gate");
    let generated = Command::new(binary)
        .args([
            "generate",
            dir.path().to_str().unwrap(),
            "95",
            "3",
            "2",
            "42",
        ])
        .output()
        .unwrap();
    assert!(generated.status.success());

    let benchmark = Command::new(binary)
        .args([
            "bench",
            dir.path().to_str().unwrap(),
            dir.path().join("report.ndjson").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(benchmark.status.success());
    assert!(benchmark.stderr.is_empty());
    let stdout = String::from_utf8(benchmark.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["converted"], 98);
    assert_eq!(value["rejected"], 2);
    assert_eq!(value["total"], 100);
    assert_eq!(value["run_status"], "complete");
    assert_eq!(value["acceptance"], "unverified");
    assert!(value.get("status").is_none());
    assert_eq!(value["output_digest"].as_str().unwrap().len(), 64);
    assert!(value.get("elapsed_ms").is_some());
    assert!(value.get("peak_rss_bytes").is_some());
}

#[test]
fn relative_output_path_publishes_successfully() {
    let dir = tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_lab-unit-gate");
    let generated = Command::new(binary)
        .args([
            "generate",
            dir.path().to_str().unwrap(),
            "95",
            "3",
            "2",
            "42",
        ])
        .output()
        .unwrap();
    assert!(generated.status.success());

    let run = Command::new(binary)
        .current_dir(dir.path())
        .args(["run", ".", "report.ndjson"])
        .output()
        .unwrap();

    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(run.stderr.is_empty());
    assert!(dir.path().join("report.ndjson").is_file());
    let summary: serde_json::Value = serde_json::from_slice(&run.stdout).unwrap();
    assert_eq!(summary["total"], 100);
}
