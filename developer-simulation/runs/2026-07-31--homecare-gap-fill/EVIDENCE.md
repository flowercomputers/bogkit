# Evidence

## Ordered discovery and friction

1. Read the repository `README.md` first. It described Fold as an incremental,
   durable stream framework and named starter, time-series, chat, and search
   examples.
2. Read the four public examples in that order: starter, time-series, chat,
   search. Starter established durable transactions and retractions;
   time-series showed keyed aggregates; chat showed one writer and consistent
   snapshots; search showed keyed upsert/remove semantics.
3. Inspected only the public Fold keyed-stream, table, and pipeline APIs needed
   to determine whether the component could represent mutable input and durable
   count views.
4. Evaluated the baseline before selecting a component. A deterministic
   full-rescan greedy scheduler met fill and urgent coverage, but the sampled
   cancellation changes preserved only 98.2940% of unaffected assignments on
   average. That misses the 99.5% requirement and motivates a local change path.
5. Selected Fold narrowly for durable keyed state and transactional materialized
   counts. Scheduling itself stayed in ordinary Rust. ESE and ANNy were rejected
   as no-fit.
6. First dependency resolution attempt failed because the sandbox could not
   resolve `index.crates.io`. After network access was approved, dependencies
   resolved. Subsequent checks ran offline.
7. The first compile exposed a generic associated-reader type that could not be
   destructured. Replacing anonymous closures with named functions gave the
   Fold pipeline a concrete type; no BogKit source was changed.
8. The first successful workload over-correlated region and certification and
   made most unfilled visits structurally impossible. Randomizing certification
   independently and lowering the total hour cap created meaningful capacity
   pressure: cancellations now free capacity for the incremental path.
9. Skeptical review found that the validator returned `TRAVEL_CONFLICT` whenever
   all earlier filters left a candidate, without proving travel was the blocker.
   A one-caregiver reproducer was incorrectly accepted. The validator now checks
   travel independently, errors when any caregiver is eligible, and has a
   regression test.
10. Review also rejected the initial “partial fit” label. The prototype has no
    SQLite adapter, change feed, rebuild policy, idempotent handoff, or
    cross-store transaction, so the scenario remains no-fit for the required
    SQLite-authoritative boundary.

## Exact commands and observed results

All commands were run from this crate directory.

### Dependency-resolution failure

```console
cargo test --all-targets
```

Observed: failed before compilation because `index.crates.io` could not be
resolved. Retried with approved network access; Cargo downloaded dependencies
and then reported the concrete Fold reader type error described above.

### Tests

```console
cargo test --all-targets --offline
```

Observed after review fixes: exit 0; 5 passed, 0 failed. Coverage includes
explicit-offset instant arithmetic around two DST transitions, rejection of
offset-free local times, the spurious-travel-reason regression, deterministic
scheduling, cancellation stability, independent validation, and Fold
round-trip recovery with materialized count checks.

### Formatting

```console
cargo fmt --check
```

Observed: exit 0; no output.

### Linting

```console
cargo clippy --all-targets --offline -- -D warnings
```

Observed: exit 0; no warnings in the prototype or its targets.

### Release demo

```console
cargo run --release --offline
```

Observed: exit 0. One measured run:

```text
DATASET label=10%-representative caregivers=2000 visits=12000 horizon_days=14 generated_ms=1.959
BASELINE initial_ms=133.788 sampled_changes=12 sampled_p95_ms=116.540 sampled_mean_preservation_pct=98.2940 final_rescan_ms=103.811 filled=10000 urgent_filled=2761/2913
INCREMENTAL initial_ms=485.727 burst_changes=200 burst_ms=900.334 p95_ms=5.363 throughput_changes_per_s=222.1 preservation_pct=100.0000 filled=10000 urgent_filled=2761/2913
VALIDATOR status=ok constraint_violations=0 active_visits=11800 outcomes=11800 unfilled_reason_codes={"HOUR_LIMIT": 1235, "NO_CERTIFICATION": 57, "NO_REGION_COVERAGE": 0, "OUTSIDE_AVAILABILITY": 0, "REQUIRED_REST": 508, "TRAVEL_CONFLICT": 0}
REPLAY status=deterministic digest=3a9719e0c8654377 replay_ms=505.919
RESTART status=ok recovery_ms=47.200 caregivers=2000 active_visits=11800 assignments=10000 unfilled=1800
CRASH_RESTART status=ok child_status=signal: 6 (SIGABRT) recovery_ms=50.553 committed_visit=28 canceled_visits=201
```

The latency above includes a Fold transaction and explicit checkpoint for every
change. Two reviewer reruns measured baseline sampled p95 at 102.118 and
113.509 ms and incremental p95 at 5.517 and 5.244 ms. All three runs reproduced
digest `3a9719e0c8654377`, zero validator violations, the same fill/urgent counts,
and 100% preservation.
`/usr/bin/time -l` could not report resident memory because its
`sysctl kern.clockrate` call was denied in this environment; the demo itself
still completed successfully.

## Categorized findings

| Category | Severity | Confidence | Finding | Reproduction | Smallest improvement |
|---|---:|---:|---|---|---|
| Poor product fit | High | High | The isolated Fold projection demonstrates atomic durable input/outcome state and counts, but the required SQLite-to-Fold synchronization and atomic publication boundary are absent. | Inspect `src/store.rs`; no SQLite adapter or handoff exists. | Document source-of-truth and cross-store transaction boundaries; keep this scenario no-fit until a real handoff is proven. |
| Prototype correctness defect, fixed | High | High | The original validator accepted a spurious travel-conflict reason without testing travel feasibility. | Run `validator_rejects_spurious_travel_conflict`; the pre-fix reviewer reproducer returned success. | Independently check travel and error when any caregiver is fully eligible. |
| Stability | High | High | Full rescans preserved 98.2940% in the sample; the incremental path preserved 100%. | `cargo run --release --offline` | Keep published outcomes first-class and expose affected-key transactions. |
| Performance evidence | Medium | High at representative scale | Across three local runs, incremental p95 was 5.244–5.517 ms and baseline sampled p95 was 102.118–116.540 ms. | Run the release demo repeatedly. | Add full-scale, memory-bounded, multi-seed benchmarks before production claims. |
| Correctness | High | High at representative scale after the fix | Independent validation found no certification, region, availability, rest, hour, or travel violations; the regression rejects a falsely unfilled eligible visit. | Run the demo and all tests. | Add mutation/property tests across many seeds. |
| Recovery | High | High for committed state | Normal reopen was 47.200 ms and post-abort reopen was 50.553 ms with the committed change present. | Same demo; child SIGABRT is intentional. | Add a second crash point inside an uncommitted transaction. |
| Time handling | Medium | High for the narrow parser contract | Explicit-offset RFC3339 strings are parsed as instants and offset-free strings are rejected; IANA-zone ambiguity and nonexistent-wall-time policy are not tested. | Run the time unit test. | Add timezone-aware conversion at the unimplemented SQLite import boundary. |
| Developer experience | Low | High | Anonymous Fold pipeline closure types made a reusable load helper awkward. | Replace named functions in `src/store.rs` with closures and compile. | Provide a documented named-pipeline/type-alias example for reusable stores. |
| Environment | Low | High | First build requires registry access if dependencies are not cached. | Clear cache and run `cargo test --all-targets` without network. | Offer a vendored/offline lab setup. |

## Decision audit

- **Baseline first:** retained as a deterministic full-rescan comparator. It
  met final fill and urgent coverage but failed the assignment-preservation
  target and did unnecessary work on each cancellation.
- **Fold: evaluated in an isolated projection.** `KeyedStream` gives upsert/remove semantics,
  atomic caregiver/visit/outcome changes, durable restart state, and
  incrementally maintained counts. This is directly relevant to cancellation
  events and recovery.
- **Custom scheduler: selected.** The smallest safe change is to retain valid
  published assignments, remove only the canceled visit, and test the freed
  caregiver against the urgency-ordered open set. It guarantees no reassignment
  of unaffected visits, including those inside six hours.
- **Independent validator: selected.** It deliberately duplicates constraint
  checks and reason classification instead of calling scheduler eligibility.
- **Overall decision: no fit for the stated SQLite-authoritative system.** The
  isolated Fold projection is useful evidence, but it is not a safe integration
  until synchronization, rebuilding, and atomic publication are demonstrated.
  Do not move constraint logic into BogKit core based on this trial.

## Alternatives rejected

- **Fold `TopK` or aggregate as the scheduler:** no-fit because eligibility
  depends on interacting travel, rest, hour, and assignment constraints; a
  simple rank is not enough.
- **ESE:** no-fit; there is no semantic-text matching problem.
- **ANNy:** no-fit; exact deterministic eligibility is required, not approximate
  vector similarity.
- **Global optimization:** explicitly out of scope and would make the prototype
  materially larger.
- **SQLite replacement:** rejected. The existing service owns that boundary;
  this trial tests a durable scheduling projection, not a database migration.
- **Full 20,000/120,000 benchmark in this pass:** rejected to keep the trial
  compact. The 10% run is labeled and no linear extrapolation is claimed.

## Unresolved uncertainty

- Full requested scale has not been run, so p95, startup time, and the 1 GiB
  limit are unproven at 20,000 caregivers and 120,000 visits.
- Peak resident memory was not measured because `/usr/bin/time -l` could not
  access the required system control in this sandbox.
- The SQLite import/change-feed adapter is not implemented.
- The crash harness proves recovery after an acknowledged commit and process
  abort; it does not yet inject a crash inside an uncommitted transaction.
- Explicit-offset parsing is correct only as instant arithmetic. IANA-zone
  conversion and policy for ambiguous/nonexistent local wall times must be
  owned by the importer.
- Only one seed and a synthetic distribution were used for the recorded
  performance run. More seeds and real distribution shapes could expose
  different contention and reason mixes.
