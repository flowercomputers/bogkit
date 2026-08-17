# Reach rollup lab prototype

This archived Rust package compares an exact per-bucket set oracle with a bounded, mergeable approximate distinct counter over the workload in `BRIEF.md`. It uses no external crates and no BogKit component. The outer state-file format is version 2 and verifies a checksum binding its complete header, bucket count, every tenant/hour key, and every nested state before returning a rollup.

## Regenerate correctness evidence

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo run --locked --manifest-path developer-simulation/Cargo.toml --package reach-rollup-lab --release -- demo developer-simulation/runs/2026-08-17--mergeable-unique-installation-reach-rollups/evidence /private/tmp/bogkit-reach-repair-demo-temp
```

The demo procedurally streams the 12-million-record corpus; it does not write or retain the corpus. It removes temporary shard states before returning.

## Verify source and tests

```console
cargo fmt --manifest-path developer-simulation/Cargo.toml --package reach-rollup-lab -- --check
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo test --locked --manifest-path developer-simulation/Cargo.toml --package reach-rollup-lab --release --all-targets --all-features
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo clippy --locked --manifest-path developer-simulation/Cargo.toml --package reach-rollup-lab --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
```

## Reproduce time and memory measurements

Run one unmeasured warm-up for each command, then run each command three times with `/usr/bin/time -l`:

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo build --locked --manifest-path developer-simulation/Cargo.toml --package reach-rollup-lab --release --all-targets
/private/tmp/bogkit-reach-repair-target/release/reach-rollup-lab bench-exact
/private/tmp/bogkit-reach-repair-target/release/reach-rollup-lab bench-candidate
/usr/bin/time -l /private/tmp/bogkit-reach-repair-target/release/reach-rollup-lab bench-exact
/usr/bin/time -l /private/tmp/bogkit-reach-repair-target/release/reach-rollup-lab bench-candidate
```

Use the slower wall time from each three-run series. `evidence/PERFORMANCE.md` records the completed measurements and machine details.

The archive uses the shared nested workspace's release profile rather than the
trial's temporary child profile. A fresh normalized-profile run on the same
host recorded exact wall times of 1.03/1.09/1.06 seconds, candidate wall times
of 0.54/0.53/0.53 seconds, and current-profile maximum resident sizes of
645,136,384 bytes exact versus 49,594,368 bytes candidate. Deterministic output
digests remained `6efde7109d2ddba5` exact and `ecda69b5ba1467d1` candidate.

## Retained evidence

- `evidence/accuracy-tuning.tsv`: 2,880 rows from the specified matrix used to diagnose the estimator transition.
- `evidence/accuracy-heldout.tsv`: 2,880 rows from a different, fixed generator seed not used for further tuning.
- `evidence/load-report.tsv`: 5,760 canonical `(tenant_id, unix_hour)` result rows.
- `evidence/summary.txt`: state-file version, seeds, accuracy/bias statistics, counts, sizes, deterministic digests, and boolean gate results.
- `evidence/PERFORMANCE.md`: warm-up/timing/RSS method and all six measured runs.

Remove `/private/tmp/bogkit-reach-repair-target` after verification; it is deliberately outside this prototype workspace.
