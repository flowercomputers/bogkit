# Caregiver scheduler cancellation trial

This is a small, standalone Rust prototype for testing whether BogKit helps a
single-process caregiver scheduler respond to cancellation bursts without
reshuffling unaffected work.

## Outcome

**No fit for the SQLite-authoritative handoff as implemented.** Fold
demonstrates useful atomic state and counts inside an isolated projection, but
the required SQLite-to-Fold synchronization and atomic publication boundary
were not implemented. Fold is not a scheduling solver. The candidate index,
scheduling policy, explanations, and independent validator remain plain Rust.
ESE and ANNy do not fit this problem.

The existing SQLite importer is represented by deterministic normalized input.
An actual SQLite adapter is deliberately outside this compact prototype. The
time parser accepts RFC3339 instants with explicit UTC offsets and rejects
offset-free strings. It does not apply IANA timezone rules or reject a
nonexistent local wall time when the caller supplies an offset.

## What the demo does

- Generates a seeded 14-day workload with 2,000 caregivers and 12,000 visits,
  exactly 10% of the requested scale.
- Models interval availability, certifications, regions, travel time, required
  rest, hour limits, urgency, and continuity preferences.
- Measures a full-rescan greedy baseline before exercising the incremental path.
- Processes a burst of 200 committed and checkpointed cancellations. Only the
  canceled visit and, when possible, one newly fillable visit are changed.
- Independently checks all constraints and reason codes, including a regression
  that rejects a spurious travel-conflict explanation when a caregiver is
  actually eligible.
- Replays the same seed and changes and compares a deterministic digest.
- Reopens the durable state, then runs a child process that commits one change
  and aborts; the parent verifies that the committed state recovers correctly.

The last measured run produced:

```text
BASELINE sampled_p95_ms=116.540 sampled_mean_preservation_pct=98.2940
INCREMENTAL burst_changes=200 burst_ms=900.334 p95_ms=5.363 throughput_changes_per_s=222.1 preservation_pct=100.0000
BASELINE filled=10000 urgent_filled=2761/2913
INCREMENTAL filled=10000 urgent_filled=2761/2913
RESTART recovery_ms=47.200
CRASH_RESTART recovery_ms=50.553
REPLAY digest=3a9719e0c8654377
```

Across the author run and two reviewer reruns, baseline sampled p95 was
102.118–116.540 ms and incremental p95 was 5.244–5.517 ms. These are
representative-scale measurements on this machine, not evidence that the full
20,000/120,000 workload or the 1 GiB limit passes.

## Reproduce

From this directory:

```console
cargo test --all-targets
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo run --release
```

After dependencies have been downloaded once, append `--offline` before the
final `--` (if any) to reproduce without network access:

```console
cargo test --all-targets --offline
cargo clippy --all-targets --offline -- -D warnings
cargo run --release --offline
```

The demo recreates `target/caregiver-scheduler-demo-state` on each run. The
child abort is intentional and is treated as success only if the parent can
recover and validate its committed change.

## Archive layout

This crate is a member of the nested `developer-simulation` workspace and uses:

```toml
fold = { path = "../../../fold" }
```

No ESE or ANNy dependency is included because neither was selected.

## Files

- `src/model.rs`: normalized records, seeded data, explicit-offset time import.
- `src/scheduler.rs`: baseline and incremental scheduling paths.
- `src/store.rs`: Fold-backed atomic records and incremental count views.
- `src/validator.rs`: independent constraint and explanation checker.
- `src/main.rs`: measurements, replay, recovery, and crash harness.
- `EVIDENCE.md`: discovery log, exact command results, findings, and decision.
