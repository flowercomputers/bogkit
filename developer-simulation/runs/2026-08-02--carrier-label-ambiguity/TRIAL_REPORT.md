# Trial report: carrier-label ambiguity

Date: 2026-08-02
Role: fulfillment-platform engineer, intermediate Rust and strong TypeScript, no prior BogKit knowledge
Source revision: `80fd3c9a023e877fff2e5d127accca386d437af0`

## Outcome

After skeptical-review correction, the reliability prototype passes the requested 20,000-shipment workload for all 30 deterministic seeds. Across 600,000 simulated shipments it made exactly one carrier purchase call per shipment, exposed 30,154 unresolved outcomes as `needs_review`, reconstructed all 3,478,477 decisions from the synced journal model, and converged by 30 simulated seconds. Developer, reviewer, and post-fix optimized runs took 2.795–2.857 local seconds and sampled 11.48–18.20 MiB peak resident memory on one host.

BogKit decision: **no fit for this reliability core**. Fold provides a useful durable incremental-view engine, but introducing its embedded store beside the required PostgreSQL authority would create another persistence boundary. It does not make the carrier request and PostgreSQL update atomic, and its retraction/materialization model does not replace the required write-ahead purchase intent, carrier reconciliation, or human review state. ESE and ANNy are unrelated to this problem.

## Ordered discovery and friction trail

1. Read the public root `README.md`. It recommends generating a project with `scripts/new-project.sh` and describes Fold, ESE, ANNy, and four examples.
2. Read public examples in the README order: `starter`, `timeseries`, `chat`, and `search`, including each example's `Cargo.toml` and `src/main.rs`.
3. `starter` established that Fold has transactional writes, consistent reads, persistence, counts, and bags.
4. `timeseries` showed keyed incremental aggregates and retractions into materialized tables.
5. `chat` showed one writer owning a Fold stream and publishing consistent snapshots.
6. `search` showed `KeyedStream` upsert/retraction and confirmed ESE/ANNy target search rather than workflow safety.
7. Froze the checkout baseline at `80fd3c9a023e877fff2e5d127accca386d437af0`; `git status --short` was empty. Toolchain was `rustc 1.95.0` and `cargo 1.95.0`.
8. `cargo fmt --all -- --check` failed on existing formatting differences in `examples/search/src/main.rs`.
9. `cargo test --workspace --all-targets` stopped while ESE's build script attempted to download `model.safetensors`; DNS/network access was unavailable. This occurred before the full baseline could run.
10. `cargo test -p fold --all-targets` passed all 18 Fold tests in 1.66 seconds.
11. `cargo clippy -p fold --all-targets -- -D warnings` was blocked by five existing `needless_range_loop` findings in ANNy.
12. Selected no BogKit dependency and built a dependency-free, archive-safe prototype so the test addressed only the carrier/PostgreSQL ambiguity boundary.
13. The first complete implementation passed functionally but measured 293.66 MiB peak resident memory. Inspection found that audit verification retained every decoded record and repeatedly rescanned the full history.
14. Replaced retained audit records with streaming replay and per-shipment attempt counters. The developer's final realistic run measured 18.16 MiB and 2.799 seconds. The discarded high-memory implementation was not available for independent review, so its historical 293.66 MiB observation is not treated as reproduced evidence.
15. Skeptical review reproduced the main workload but found that reopen ignored an incomplete journal tail without truncating it. A later commit appended behind the bad bytes and the next reopen failed its checksum. Reopen now truncates and syncs the recognized tail before returning, and the regression covers partial write, reopen, later commit, and second reopen.
16. Review also found that a sandboxed measurement could report zero when `ps` returned no samples. The script now fails without a valid sample. A post-fix process-inspected run measured 11.48 MiB and 2.795 seconds.

## Baseline comparison

The unsafe baseline described in the problem retries after a timeout. In this fixture, every seed has exactly 2,000 ambiguous timeouts. Across 30 seeds, about half of the 60,000 ambiguous calls created a paid label at the carrier. Replaying purchase for any of those would create a second paid label when the carrier does not provide a trusted idempotency guarantee.

The prototype changes only the reliability policy:

| Boundary | Unsafe baseline | Prototype |
| --- | --- | --- |
| Before carrier call | May have no durable attempt | Persists one attempt first |
| Missing response | Automatically retries purchase | Never purchases again; reconciles |
| Carrier lookup inconclusive | May remain hidden in a retry queue | Becomes `needs_review` at 30 seconds |
| Callback order/duplicates | Can race or downgrade state | Monotonic reducer; same final carrier transaction |
| Restart after carrier charge | Can repeat the purchase | Recovers intent and finds the carrier label |
| Audit | State and attempts can disagree | Every stored post-state is checked by fresh replay |

This prototype does not claim that the pre-existing service was executed. The baseline comparison is against the explicitly supplied unsafe retry behavior; the repository baseline checks above cover BogKit itself.

## Final validation evidence

All commands below ran from the sanitized checkout root.

### Prototype quality gates

```sh
cargo fmt --all --manifest-path trial-output/Cargo.toml -- --check
cargo clippy --manifest-path trial-output/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path trial-output/Cargo.toml --all-targets
cargo build --release --manifest-path trial-output/Cargo.toml
```

Observed after the review fix: formatting passed; strict lint passed with warnings denied; all three focused tests passed; the release build passed. Tests cover callback order and duplication, partial-tail repair followed by a later commit and second reopen, and a three-seed end-to-end fixture.

### Demonstration

```sh
cargo run --manifest-path trial-output/Cargo.toml -- demo \
  --dir trial-output/.verification-demo
```

Observed: 100 shipments passed; 10 ambiguous timeouts were injected; 94 carrier labels were safely recovered; five unresolved calls became review items; 591 decisions replayed; four simulated restarts recovered; maximum convergence was 30 seconds.

### Actual process exits

```sh
cargo run --manifest-path trial-output/Cargo.toml -- crash-demo \
  --dir trial-output/.verification-crashes
```

Observed: all four child processes exited with the expected crash code. Recovery after a durable intent but no authoritative label ended in `NeedsReview`; exits after carrier creation, local confirmation, and callback persistence all recovered as `Purchased`. Every case retained one attempt, and automatic retries were zero.

### Realistic acceptance and resource measurement

```sh
trial-output/scripts/measure-acceptance.sh \
  trial-output/target/release/carrier-label-ambiguity \
  trial-output/.verification-final \
  trial-output/.verification-final.log
```

Observed post-review final lines:

```text
ACCEPTANCE PASS seeds=30 shipments=600000 paid_labels=542813 needs_review=30154 decisions=3478477 max_final_at=30s journal_mib=186.73 elapsed_seconds=2.795
MEASURED PEAK RSS: 11760 KiB (11.48 MiB)
```

Per seed: exactly 20,000 shipments, exactly 2,000 ambiguous timeouts, four injected restart boundaries, no second purchase calls, and no nonterminal shipment. Duplicate/reordered callbacks were injected for every created label; the reducer test directly compares them with one ordered callback.

## Categorized findings

### F1: A durable attempt is the safety barrier

- Evidence: four child-process crash cases plus all 600,000 shipments retained exactly one attempt and performed no automatic second purchase.
- Severity: critical.
- Confidence: high; directly asserted and replayed.
- Reproduction: run `crash-demo`, then the realistic acceptance command.
- Smallest improvement: in the existing service, commit the attempt row before the HTTP call and prohibit the retry worker from purchasing when any attempt exists.

### F2: Unknown is a durable business state, not a retry condition

- Evidence: 30,154 carrier-inconclusive outcomes became `needs_review` at 30 simulated seconds; none triggered another purchase.
- Severity: critical.
- Confidence: high; deterministic across all 30 seeds.
- Reproduction: run the realistic acceptance command and inspect each seed's `needs_review` count.
- Smallest improvement: add an explicit transition from ambiguous/unfinished attempt to the existing review representation, without changing the external HTTP shape.

### F3: Callback handling must be monotonic by carrier transaction

- Evidence: every carrier label receives duplicates and reordered pending/active callbacks; the focused reducer test proves the same final state and transaction as one ordered sequence.
- Severity: high.
- Confidence: high.
- Reproduction: `cargo test --all-targets reordered_duplicate_callbacks_are_monotonic`.
- Smallest improvement: make callbacks idempotent by carrier transaction and forbid pending callbacks from downgrading `purchased`.

### F4: Incomplete journal tails must be repaired before later writes

- Evidence: skeptical review reproduced a successful first reopen followed by checksum failure after a later append. The corrected regression now completes a second reopen with three valid records.
- Severity: high prototype correctness defect, fixed before archival.
- Confidence: high; reproduced before the fix and covered afterward.
- Reproduction: run `cargo test --all-targets journal_repairs_a_partial_tail_before_later_commits`.
- Smallest improvement: truncate and sync a recognized incomplete final record before permitting another append.

### F5: Decision history can be verified without retaining the fixture

- Evidence: the checksummed journal replays and verifies each recorded post-state; 3,478,477 decisions matched. Post-fix one-host runs sampled 11.48–18.20 MiB. The developer observed 293.66 MiB before replacing the retained-history approach, but that discarded implementation was not independently reproduced.
- Severity: high for audit correctness, medium for the initial memory implementation.
- Confidence: high; corruption/partial-tail behavior and full replay are exercised.
- Reproduction: run tests and the measured acceptance fixture.
- Smallest improvement: replay audit history as a stream and retain only the current shipment projection and compact counters.

### F6: Fold is not the transaction-boundary fix

- Evidence: public examples demonstrate local transactional views and persistence but no carrier-call/PostgreSQL atomicity, external-authority reconciliation, or review transition.
- Severity: high if forced into the write path because it adds another durable authority.
- Confidence: high for this scoped prototype; production integration details remain unknown.
- Reproduction: compare the public `starter`/`chat` ownership model with the supplied PostgreSQL-authority constraint.
- Smallest improvement: document a workflow-safety example or integration boundary if Fold is intended only for derived, rebuildable views.

### F7: Public baseline has avoidable first-run friction

- Evidence: root format check fails in `examples/search`; full tests require an ESE model download; strict Fold clippy reaches existing ANNy warnings.
- Severity: low for this carrier trial, medium for onboarding confidence.
- Confidence: high; exact commands were run on the clean source revision.
- Reproduction: run the three baseline commands in the discovery trail.
- Smallest improvement: format the search example, make ESE test assets explicitly prefetchable or skippable offline, and clear workspace lint warnings.

## Decision audit

### Consequential choices

- Persist intent before network. This is the only local action that makes a later missing response safe to interpret without purchasing again.
- Treat carrier lookup and active callbacks as authoritative evidence of purchase; treat an unknown lookup as review, never proof that purchase did not happen.
- Keep state transitions monotonic and bind all carrier evidence to one carrier transaction ID.
- Store the event and its resulting row state together, then independently reduce the event on reopen. This mirrors a PostgreSQL transaction updating workflow state and inserting history.
- Use a checksummed newline journal and ignore only a final incomplete record, modeling a process exit during a write.
- Use no dependencies. The reliability core needs deterministic state transitions and file durability, not embeddings, nearest-neighbor search, or another database.

### Rejected alternatives

- Automatic retry with the merchant request ID: rejected because the brief does not grant a trustworthy carrier idempotency contract.
- Retry after a carrier lookup returns no result: rejected because absence can be stale or inconclusive.
- Fold as workflow authority: rejected because PostgreSQL must remain authoritative and no distributed transaction exists.
- Fold as audit projection inside this prototype: rejected because the audit must prove the PostgreSQL decision history itself; a second store would only prove its own projection.
- Retaining all decoded audit records: rejected after it exceeded the 256 MiB ceiling.
- Modeling refunds: rejected as an explicit non-goal.

### Remaining uncertainties and limits

- Carrier lookup semantics vary. Production code must distinguish a conclusive carrier rejection from a missing, stale, or unavailable lookup; this prototype intentionally treats missing evidence as review.
- The journal models the required atomic PostgreSQL row-plus-history transaction but is not a PostgreSQL integration test, because existing database and HTTP shapes were deliberately kept out of scope.
- Actual process exits cover four representative one-shipment boundaries. The 30-seed fixture uses durable close/reopen recovery at four boundaries per seed so it can remain fast and deterministic.
- Peak memory was sampled every 20 milliseconds with `ps`; the large margin below 256 MiB makes sampling error immaterial to the result.
- The simulator is sequential. Database-enforced single-attempt uniqueness or conditional transitions, concurrent workers, conflicting callbacks, callback authentication, real carrier lookup semantics, PostgreSQL, and network faults remain untested.
- Sync evidence covers ordinary process exits after completed local filesystem calls, not kernel failure or power loss.

After the partial-tail fix, no requested acceptance criterion remains blocked for the sequential reliability-core prototype. Production PostgreSQL, carrier, and concurrency integration remains outside the prototype boundary.
