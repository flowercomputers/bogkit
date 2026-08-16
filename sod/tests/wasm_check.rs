//! Portability gate: sod's core must keep compiling with no default
//! features — including for wasm32-unknown-unknown when that target is
//! installed. This is what keeps the browser/React-Native story a fact
//! rather than an aspiration: every platform-specific dependency must stay
//! behind its feature flag.

use std::process::Command;

fn cargo() -> Command {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
}

#[test]
fn core_builds_with_no_default_features() {
    let out = cargo()
        .args(["check", "-p", "sod", "--no-default-features"])
        .output()
        .expect("failed to run cargo");
    assert!(
        out.status.success(),
        "no-default-features check failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn core_builds_for_wasm32() {
    // Probe whether the target's std is installed; skip (pass with a
    // notice) when it isn't, so the suite runs on machines without it.
    let probe = cargo()
        .args([
            "check",
            "-p",
            "sod",
            "--no-default-features",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .output()
        .expect("failed to run cargo");
    if probe.status.success() {
        return;
    }
    let stderr = String::from_utf8_lossy(&probe.stderr);
    if stderr.contains("may not be installed") || stderr.contains("rustup target add") {
        eprintln!("SKIP: wasm32-unknown-unknown target not installed");
        return;
    }
    panic!("wasm32 check failed for a reason other than a missing target:\n{stderr}");
}
