# Evidence

Date: 2026-07-30
Checkout: sanitized current-main trial copy, corrected under skeptical review
before archival
Scope: only the public root README, public examples, Fold source/API, prior
reports during skeptical review, and this prototype were inspected.

## Result against the acceptance criteria

| Criterion | Observed result | Disposition |
| --- | --- | --- |
| No premature dependency readiness | Child stayed pending through lease and renewal; became ready in the same transaction as parent completion | Passed in the modeled graphs |
| At most one attempt commits a terminal result | Attempt 2 won after attempt 1 expired; the late attempt named attempt 2 and its object key and made no change | Passed in the modeled state machine |
| Replayed completion/heartbeat makes no further change | Versions were checked before and after duplicate and reordered delivery across all 25 crash pairs | Passed under the message-ID assumption |
| Expired lease reassigns within 5 seconds | Heartbeat at the exact deadline and completion far past it were rejected without mutation; the simulated scan made the job retry-ready 1 ms after the deadline and assigned attempt 2 | Passed in deterministic time; scheduler delay is not measured |
| Recovery accepts work within 10 seconds at 100,000 jobs | Final five release-mode reopen-plus-lease samples: 0.162–0.170 seconds | Passed for one local process |
| 2,000 updates/s, p99 below 50 ms | Final three 2,000-update release passes: 334,115–348,759/s with 0.108–0.126 ms p99 | Passed only as a 32-message batched embedded-store upper bound |
| Every rejected completion names winner or rule | Duplicate/late completion named attempt 2 and `objects/fencing/winner`; nonterminal rejection paths state the violated state/fence rule | Passed in covered rejection paths |
| Persistent state below 1 GiB | 16,309,086 bytes after 100,000-job seed, benchmark leases, and 6,000 renewals | Passed for the synthetic graph |
| Three concurrent coordinator replicas | Second concurrent Fold opener was rejected | Failed; decisive no-fit |

Because multi-replica correctness is required, the overall decision is **no
fit**, even though the single-writer subset met the local checks.

## Ordered discovery and friction trail

1. Read the root `README.md` before any crate source. It presents Fold as a
   durable incremental stream with fast materialized reads, and points new
   users to the examples.
2. Read all four public examples and their manifests. `starter` showed atomic
   writes and persistence; `timeseries` showed incremental derived tables;
   `chat` explicitly used one thread that owns the stream; `search` showed
   `KeyedStream` upsert/retraction behavior.
3. Evaluated the JSON-snapshot baseline before selecting Fold. The minimal
   reproducer acknowledged an in-memory attempt-1 lease, restarted from the
   older JSON snapshot, then gave a different worker the same attempt-1 fence.
   Therefore periodic snapshots fail correctness. The measured in-place
   sync-after-every-update variant is only a naïve full-rewrite lower bound: it
   lacks temporary-file writing, atomic replacement, and directory sync, so it
   is not a crash-safe repair.
4. Inspected Fold's public stream and table APIs. `KeyedStream::wtx` can read,
   replace, and derive multiple records atomically, which fits dependency
   readiness and attempt fencing for one writer.
5. Found the public concrete type awkward for a named coordinator struct
   because pipeline types include predicates. A function-pointer predicate and
   explicit type aliases kept the prototype small.
6. Built persistent job records plus derived ready/leased tables. No ANNy or
   ESE feature is used; they do not fit this workload.
7. Added unit checks for readiness, replay idempotence, exact and far-past
   expiry, fencing, and immutable results. Skeptical review found that the
   first version could revive an expired lease; the coordinator added explicit
   deadline checks to both heartbeat and completion before archival.
8. Added the process-level fault runner. Each of 100 child processes performed
   one mutation, printed `ACK`, flushed it, and called `process::exit(77)`,
   bypassing Rust destructors. The parent required that exact exit, reopened
   Fold, checked state, delivered duplicates/reordered messages, and checked
   that expired heartbeat and completion messages did not mutate the lease.
9. Ran the concurrent-open check. Fold rejected the second opener. This is
   appropriate behavior for an embedded single-writer store, but it exposes
   the scenario mismatch.
10. Ran formatting, tests, strict prototype lint, the release demonstration,
    and repeated release measurements.
11. A warnings-denied Clippy run including dependencies failed on five
    pre-existing `needless_range_loop` warnings in `anny`. The prototype-only
    warnings-denied run passed. No unrelated library source was changed.

## Exact commands and observed results

### Initial public-material inspection

```console
sed -n '1,260p' README.md
for f in examples/*/src/main.rs; do sed -n '1,260p' "$f"; done
for f in examples/*/Cargo.toml; do sed -n '1,180p' "$f"; done
```

Observed: the documented database component relevant to coordinator state was
Fold; the examples consistently constructed an owned local stream and did not
show multi-process writes, replication, or leader election.

### Build and tests

```console
cargo check -p ci-lease-coordinator
cargo test -p ci-lease-coordinator
```

Observed:

```text
cargo check: passed
running 4 tests
test tests::expired_attempt_cannot_overwrite_winner ... ok
test tests::expired_messages_cannot_revive_or_complete_a_lease ... ok
test tests::duplicate_and_reordered_messages_do_not_mutate ... ok
test tests::dependency_only_becomes_ready_after_parent_terminal_commit ... ok
test result: ok. 4 passed; 0 failed
```

### Formatting

```console
cargo fmt -p ci-lease-coordinator -- --check
```

Observed: passed with no output after formatting the prototype.

The broader `cargo fmt --all -- --check` also reported pre-existing formatting
differences in `examples/search/src/main.rs`; this prototype did not alter that
file.

### Strict lint

```console
cargo clippy -p ci-lease-coordinator --all-targets --no-deps -- -D warnings
```

Observed: passed.

The dependency-inclusive form:

```console
cargo clippy -p ci-lease-coordinator --all-targets -- -D warnings
```

stopped in `anny` on five existing `clippy::needless_range_loop` warnings
(`hnsw.rs` at 316, 578, 623, and 640; `metric.rs` at 29). It did not identify a
prototype warning before stopping.

### Release demonstration

```console
cargo run --release -p ci-lease-coordinator -- demo
```

Observed:

```text
baseline failure: acknowledged worker Some(7); restart leased Some(8); both received fencing attempt 1
fault test passed: 100 forced process exits after ACK; duplicate/reordered messages were no-ops; heartbeat and completion observed after expiry were rejected without mutation; expiry became retry-ready in 1 ms; late attempt rejection: rejected completion for job 50: terminal winner is attempt 2, result objects/fencing/winner; rule says terminal results are immutable
multi-writer check: second concurrent Fold opener was rejected; this embedded single-writer component cannot host three active coordinator replicas
```

### Repeated release measurements

```console
cargo run --release -p ci-lease-coordinator -- bench /tmp/bogkit-ci-lease-benchmark-trial-a.db
```

Observed:

```text
JSON full rewrite (100,000 jobs, 20248904 bytes), repeated sync times: 0.030s, 0.028s, 0.029s
batched heartbeat pass 1: 2,000 updates in 0.006s = 338753 updates/s; message p99 commit latency 0.108 ms
batched heartbeat pass 2: 2,000 updates in 0.006s = 334115 updates/s; message p99 commit latency 0.108 ms
batched heartbeat pass 3: 2,000 updates in 0.006s = 348759 updates/s; message p99 commit latency 0.126 ms
Fold upper-bound sample: seed/checkpoint 0.473s; five reopen+lease times 0.169s, 0.162s, 0.165s, 0.168s, 0.170s; overall message p99 0.117 ms; persistent directory 16309086 bytes
```

The JSON file was about 20.2 MB. Its 28–30 ms in-place rewrite times are only a
lower bound for a crash-safe replacement protocol. Periodic rewrites retain the
demonstrated correctness hole.

## Categorized findings

### Correctness defect

- **Baseline only:** acknowledging mutations held only in memory allows a
  restart to forget attempt increments and lease ownership. Two workers can
  receive the same attempt fence.
- No correctness defect was demonstrated in Fold's single-writer transaction
  behavior.

### Performance problem

- **Baseline only:** the naïve in-place rewrite of the 100,000-job state is a
  full 20.2 MB write. Its measured time is a lower bound, not a crash-safe
  snapshot measurement, because the prototype does not use temporary-file
  writing, atomic replacement, and directory sync.
- The Fold result is an optimistic local upper bound; no multi-replica,
  network, object-store, or scheduler overhead was measured.

### API friction

- Fold pipeline types include closure types, which makes storing a composed
  stream in a named coordinator struct awkward. Function-pointer predicates
  and aliases were enough here.
- Ready and leased indexes were easy to express as filtered materialized
  tables, but scheduling order and priority policy remain application logic.

### Documentation gap

- The root README and examples do not state the process/concurrency boundary
  prominently. A new user has to reach the `SingleWriterTxDatabase` API or try
  a second opener to discover it.
- No public example covers crash recovery, idempotent external messages,
  fencing tokens, or the durability distinction between process crash and
  power loss.

### Missing capability

- No consensus, leader lease, replica fencing, replicated log, or documented
  multi-process compare-and-swap is present in the evaluated Fold surface.
  Three active coordinator replicas therefore cannot safely share this state.

### Poor product fit

- The scenario makes three concurrent coordinators and correctness mandatory.
  The missing distributed coordination is not a bounded throughput
  optimization; it changes the system's authority model. BogKit is therefore
  not a fit for the full coordinator.

### Actual BogKit defect

- None demonstrated. Rejecting a second embedded single-writer opener is not a
  defect. The dependency-inclusive strict-lint failure is a repository quality
  issue in ANNy, not evidence of a runtime correctness defect in Fold.

## Decision audit

1. **Rejected periodic JSON snapshots.** They acknowledge state that is not yet
   durable, so the crash requirement fails.
2. **Rejected the naïve sync-after-every-update JSON comparison.** It requires
   a full in-place rewrite per mutation and still lacks a crash-safe
   replacement protocol. Its timing is only a lower bound for a correct
   snapshot design.
3. **Selected only Fold.** ESE and ANNy solve embedding/search problems, not
   durable coordination.
4. **Used keyed full job records as the source of truth.** This allows one
   transaction to compare a fence, commit a result, and unlock dependents.
5. **Materialized ready and leased subsets.** This avoids scanning all 100,000
   jobs during recovery dispatch or expiry checks.
6. **Used coordinator-provided time only.** Worker clocks never determine a
   deadline.
7. **Batched at most 32 heartbeats.** This is a bounded single-writer throughput
   optimization; acknowledgment waits for the batch commit.
8. **Did not invent distributed wrapping.** Adding a consensus service,
   leader-election system, or remote transactional database would be the real
   coordinator authority and is outside BogKit and this prototype.
9. **Concluded no fit.** The multi-writer failure is correctness-critical and
   cannot be excused by the strong single-process measurements.

## Prototype limits and uncertainty

- The synthetic upper-bound graph uses 10,000 builds with 10 jobs each,
  arranged as independent chains. It reaches 100,000 jobs but does not cover
  the full 1-500 job/build distribution or wide fan-in/fan-out.
- The fault run uses 25 two-job dependency graphs plus one fencing job. It
  forces 100 post-ack process exits, but does not kill during the storage
  commit itself because no acknowledgment exists before commit returns.
- `process::exit` bypasses Rust destructors, but it is not a power-loss test.
  Fold documents `checkpoint` separately for OS/power durability.
- The 1 ms expiry result is simulated coordinator time and immediate scheduler
  invocation. It proves state-machine eligibility, not a five-second
  production scheduling service-level objective.
- Heartbeat replay idempotence assumes stable, increasing per-attempt message
  IDs already exist. The worker protocol was unspecified and cannot be
  changed; absence of that field is an unresolved scenario/protocol mismatch.
- Results are local-machine measurements from one run containing three update
  passes, five recovery samples, and three JSON rewrites. They are not
  cross-machine capacity guarantees.
- Batch latency excludes queueing time to collect up to 32 messages. A
  production implementation would need a bounded flush timer below the 50 ms
  objective.
- No network server, dependency discovery, artifact transfer, object-store
  mutation, autoscaling, UI, authentication, or multi-region behavior is
  included, matching the stated non-goals.
- The prototype has no three-replica implementation, because the evaluated
  component provides no safe basis for one. This is the decisive scenario
  mismatch, not a hidden prototype TODO.
