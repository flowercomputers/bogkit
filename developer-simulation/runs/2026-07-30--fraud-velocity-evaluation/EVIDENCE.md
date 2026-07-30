# Evidence: fraud velocity blind evaluation

Run date: 2026-07-30
Environment: arm64 macOS 26.5.2, Rust 1.95.0, release profile inherited from the
workspace (`opt-level=3`, one codegen unit, fat LTO).

## Outcome

**Decision: BogKit is not a fit for this scenario on the evidence available.**

The Redis baseline is incorrect under duplicate and late delivery. Fold offers
useful embedded transactional materialization and ranked event-time range
indexing, but the public contracts inspected do not cover the full correctness
boundary: maintained multi-rule aggregates with linked retroactive corrections,
two-ID deduplication, deletion policy, four-replica partition ownership and
transfer, or atomic coordination with durable-stream offsets. Using Fold only
as a local time index would leave the consequential system semantics custom and
unverified. ESE and ANNy are unrelated to this problem.

The prototype independently demonstrates the intended deterministic semantics,
but it does not satisfy the full acceptance run. No BogKit correctness defect was
found because no BogKit component was forced into a job outside its documented
contract.

## Ordered discovery and friction trail

1. Read the root `README.md` and then all four public example manifests and
   sources (`starter`, `timeseries`, `chat`, and `search`). The public path showed
   durable inserts/retractions, keyed upsert/remove, aggregates, and consistent
   snapshots.
2. While enumerating examples, a local zsh command used the special lowercase
   variable `path`, which temporarily replaced zsh's command search path. The
   command stopped after read-only output; no file changed. Rerunning with
   `file_name` completed the read.
3. Built and ran the Redis-style independent-TTL reproducer before inspecting
   Fold internals or selecting a component. One duplicate changed both counters
   from `(1, 1)` to `(2, 2)`. A later arrival whose event time was still inside
   the same minute returned `(1, 1)` because arrival-time TTLs had expired. The
   design retained no contributing event list.
4. Inspected Fold's public crate, stream, keyed stream, scoring, retention,
   ranked-index, tests, and manifest contracts. Positive findings: single-writer
   crash-safe transactions, atomic keyed upsert/remove, deterministic
   retractions, time-ordered range scans, and persistent materializations.
5. Rejected `Retain`: its documentation explicitly defines a processing-time
   window stamped by transaction wall clock and calls persistence across clock
   jumps best-effort. That is incompatible with event-time replay and 15-minute
   late correction.
6. Rejected `TopK`: timestamp scoring produces the most recent *N records*, not
   1-minute, 10-minute, and 24-hour duration windows.
7. Considered `Ranked`/keyed ranked state as a narrow event-time index. Rejected
   adoption because it would not supply deduplication across both IDs, linked
   decision revisions, explanation retention, deletion policy, broker-offset
   atomicity, or replica partition transfer. The critical behavior would still
   be application-owned.
8. Built the dependency-free reference prototype. The first test pass caught a
   real prototype deletion defect: empty account-index containers survived after
   their final rows were removed. The deletion scan rejected the result. Removing
   empty containers fixed it; all five behavioral tests then passed.
9. Ran formatting, strict lint, the release demonstration, and three release
   benchmark rounds. The release digest was identical in every round.

## Categorized findings

### Baseline defects (not BogKit defects)

- **Correctness defect:** retries double-count because neither event ID nor
  merchant event ID is checked before increment.
- **Correctness defect:** independent arrival-time TTLs disagree with event-time
  windows and cannot deterministically correct prior decisions.
- **Missing baseline capability:** counters alone cannot reconstruct an alert's
  exact contributors.
- **Performance problem:** the baseline requires separate remote operations for
  account, card, device, and IP state on the checkout path. This trial did not
  connect to Redis, so no network-latency number is claimed.

### BogKit evaluation

- **Poor product fit:** Fold is an embedded single-writer dataflow store; the
  scenario's hard boundary includes four stream consumers with partition
  reassignment.
- **Missing capability:** no inspected public contract coordinates a Fold
  transaction with a durable-stream offset or transfers state when a partition
  moves.
- **Missing capability:** `Ranked` and `KeyedRanked` supply event-time range
  scans, but no inspected operator maintains the complete multi-rule
  aggregation, linked correction, deduplication, and deletion semantics.
- **API friction:** pipeline types include closure types, and the examples use
  local macros where ordinary helper signatures cannot name the reader type.
  That is manageable for an experienced Rust developer, but material friction
  for the stated Rust-beginner persona.
- **Documentation gap:** the root guide points to generated Fold documentation
  but does not surface the crucial processing-time versus event-time distinction.
  The detailed `Retain` documentation itself is clear once found.
- **Actual BogKit defect:** none demonstrated.

### Prototype evidence

- **Correctness:** the indexed engine matched a separately implemented naive
  scan for every latest decision in the demonstration fixture.
- **Deduplication:** 10 deliveries became 9 events; the duplicate changed no
  state or decision record.
- **Late data:** late event 8 generated 6 linked corrections.
- **Replay:** uninterrupted and normal close/reopen paths produced 3,311
  byte-identical decision bytes. No crash or torn-write behavior was tested.
- **Explanations:** all 15 emitted alert outcomes were internally
  reconstructable from retained event records. The independent naïve reference
  validates latest decisions, not every historical correction revision.
- **Deletion:** 4 events owned by account 100 had their account and card keys
  scrubbed in memory; the audit captured both identifiers and checked the
  retained event rows plus account/card indexes. A retained event's shared
  device/IP counts were unchanged, and retained alerts reconstructed.
- **Determinism:** the demonstration digest was `2a763be27500e742`.

### Prototype limits

- Persistence is an append-only fixture ledger with one sync per accepted event,
  not an optimized production log.
- Customer deletion is verified for in-memory retained/indexed state; durable
  ledger compaction and a post-compaction disk scan are not implemented.
- Normal close/reopen replay uses one canonical delivery order. It does not
  simulate a crash, torn write, four replicas, cross-partition scheduling,
  reassignment, or stream-offset commits.
- `PersistentEngine` mutates memory before appending and syncing its ledger. A
  write or sync error can leave memory ahead of disk, and a torn final TSV row
  prevents reopening.
- The rule set is fixed and partitions amount totals by currency. It counts all
  authorization attempts; production policy would need to confirm that choice.
- The benchmark uses mostly sparse keys, stores full canonical decision output
  in memory, and reports a payload lower bound, not allocator overhead or
  resident-set size. It is an in-memory upper bound, not a production load
  result.
- The trial did not run the required 20-million-event state test or sustain load
  for 30 minutes. It therefore makes no full-acceptance performance or storage
  claim.

## Decision audit

1. **Chose canonical ordering:** arrivals by `(arrival_time, event_id)` and
   event-time contributions by `(event_time, event_id)`. Rejected wall clock and
   hash-map iteration because replay bytes must not depend on timing or map order.
2. **Chose two-ID deduplication:** either event ID or merchant event ID blocks a
   retry; a conflicting reuse is rejected. Rejected “last write wins” because it
   would silently revise the payment fact.
3. **Chose explicit revisions:** late events recompute only later canonical
   events sharing an affected identifier and append a correction linked to the
   previous revision. Rejected silent aggregate mutation because analysts need
   an audit trail.
4. **Chose contributor IDs only on alerts:** all decisions retain counts and
   totals, while alerts additionally retain exact event IDs. Rejected contributor
   arrays on every non-alert because they grow storage without serving the stated
   explanation requirement.
5. **Chose account/card scrubbing with shared device/IP retention:** this directly
   tests the deletion boundary. Rejected wholesale event removal because that
   corrupts shared aggregates.
6. **Rejected partial Fold adoption:** `Ranked` could replace the prototype's
   ordered maps, but that substitution would not reduce the highest-risk custom
   logic and would create an unsupported impression of end-to-end fit.
7. **Uncertainty:** a production architecture could wrap Fold with partition-local
   ownership, an outbox/offset protocol, Fold's range indexes, custom
   correction semantics, and deletion compaction. This trial did not build or benchmark
   that larger system, so it cannot rule out such an architecture; it does show
   that BogKit does not currently supply the required boundary as a small,
   justified adoption.

## Exact validation commands and observed results

```console
$ cargo run -p fraud-velocity-evaluation -- baseline
redis baseline
  duplicate event 10 changed both counts: (1, 1) -> (2, 2)
  late event 11 belongs with event 10 in event time, but arrival-time TTL returned (1, 1)
  no retained contribution list can explain either result
```

```console
$ cargo test -p fraud-velocity-evaluation
running 5 tests
test result: ok. 5 passed; 0 failed
```

```console
$ cargo fmt -p fraud-velocity-evaluation -- --check
# exit 0, no output
```

```console
$ cargo clippy -p fraud-velocity-evaluation --all-targets -- -D warnings
Finished `dev` profile [optimized + debuginfo]
# exit 0
```

```console
$ cargo run --release -p fraud-velocity-evaluation -- demo
10 deliveries -> 9 unique events; 1 duplicate ignored
6 linked corrections from late event 8; 6 total corrections
15 canonical records; 15 alert explanations; digest 2a763be27500e742
normal close/reopen replay
uninterrupted and reopened ledgers produced 3311 identical decision bytes
scrubbed 4 account-owned events and audited account/card removal; shared device/IP counts unchanged
demo: PASS
```

```console
$ cargo run --release -p fraud-velocity-evaluation -- benchmark 100000 3
sparse-key latest-state naive-reference comparison: PASS (2000 unique events)
round 1: 971105 deliveries/s, p99 0.002 ms, 134 corrections, 50056 alert outcomes, digest 0fb37b9c0b65c76c
round 2: 1080871 deliveries/s, p99 0.002 ms, 134 corrections, 50056 alert outcomes, digest 0fb37b9c0b65c76c
round 3: 951640 deliveries/s, p99 0.002 ms, 134 corrections, 50056 alert outcomes, digest 0fb37b9c0b65c76c
median: 971105 deliveries/s; median p99 0.002 ms
measured payload lower bound: 39.41 MiB
```

The mostly sparse-key benchmark fixture includes 5% late events (up to 15
minutes) and 1% duplicate deliveries. These short-run in-memory upper-bound
results are useful prototype evidence, not substitutes for the specified
30-minute and 20-million-event gates.
