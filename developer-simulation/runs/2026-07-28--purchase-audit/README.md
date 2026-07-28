# Purchase-request audit evaluation

## Outcome

Use the conventional PostgreSQL audit table. BogKit is not a fit for the
required audit write path.

Fold gives atomic transactions inside its own embedded `fjall` store. It
cannot put a PostgreSQL state mutation and a Fold audit event in the same
transaction. The included reproduction commits an approval to the
PostgreSQL-like state model, injects a failure before the Fold write, and
ends with approved current state but only the older create event. That
violates the central acceptance criterion.

The baseline prototype passes its deterministic semantic tests. Those tests
do not prove PostgreSQL durability, database grants, crash recovery, 150-user
concurrency, seven-year storage behavior, or the production latency target.
Those items need PostgreSQL integration and load tests.

## Persona

I approached BogKit as Priya, a developer seeing it for the first time. I
maintain internal finance software at a 250-person manufacturer. I have three
years of Rust experience and regularly use Axum, SQLx, and PostgreSQL.

My service stores the current state of purchase requests in PostgreSQL.
Thirty-day logs are not a sufficient audit record. Finance needs an immutable,
ordered seven-year history that includes the policy version used for each
change.

Expected load:

- 80,000 requests
- About 400,000 changes per year, or roughly 2.8 million changes over seven
  years
- 150 concurrent users
- Less than 20 ms added write p95
- No hosted infrastructure

## Baseline and acceptance criteria

The baseline is an append-only `purchase_request_audit` table in the same
PostgreSQL database as `purchase_request`. Each service mutation updates the
current row and inserts exactly one audit row in one SQL transaction. The
ordinary application role can select and insert audit rows but cannot update,
delete, or truncate them. A conventional index on `(request_id, sequence)`
serves ordered timelines.

The required behavior is:

1. Every successful mutation creates exactly one audit event.
2. A forced failure leaves neither the mutation nor its event.
3. Timelines are ordered and accurately capture before state, after state,
   actor, amount, and policy version.
4. The ordinary application role cannot update or delete audit events.
5. A 10,000-event query completes in less than 250 ms locally.

Cryptographic tamper evidence, migration, UI, cross-service collection,
retention automation, and production authentication are out of scope.

The reference table and privilege shape is in
[`baseline.sql`](baseline.sql). It was not run because this prototype does not
include PostgreSQL.

## Discovery and friction trail

This is the order in which I learned the project.

1. I read the repository `README.md`. It describes Fold as an incremental
   programming framework that materializes changing streams into fast views.
   It describes ESE as an embedding tool and ANNy as nearest-neighbor search.
   Only Fold appeared relevant to an audit timeline.
2. I read `examples/starter/src/main.rs`. Its `Stream::wtx` closure looked
   promising because the example says all of its materialized views commit
   atomically.
3. I ran the advertised starter command. It did not run. The starter manifest
   directly depends on ESE and ANNy even though its source only uses Fold.
   Building ESE tried to download `model.safetensors` and failed when DNS was
   unavailable.
4. I read Fold's crate documentation and stream implementation. This resolved
   the main ambiguity: Fold stores state in an embedded `fjall` LSM store and
   creates its own `SingleWriterTxDatabase` transaction. Its rollback guarantee
   covers only that store.
5. I read the keyed stream and terminal implementations. Keyed state and
   materialized views are atomic with one another because they share the Fold
   store. There is no caller-owned PostgreSQL or SQLx transaction hook.
6. I read the timeseries and chat examples. They demonstrate useful
   incremental views, but the chat example explicitly makes Fold the source of
   truth and has one thread own all writes. That conflicts with the requirement
   that PostgreSQL remain the source of truth.
7. I read the `Bag` terminal. A holder of the Fold write handle can call
   `remove`, and the terminal deletes an element when its multiplicity reaches
   zero. Fold has no PostgreSQL-style role grants for separating ordinary
   application access from audit administration.
8. I implemented both the smallest baseline model and a failing-boundary
   reproduction using the real Fold `Bag`. The result confirmed that Fold's
   internal atomicity does not close the two-store failure window.

I did not investigate ESE or ANNy further. Embeddings and nearest-neighbor
search do not address any acceptance criterion.

## Prototype contents

- [`src/lib.rs`](src/lib.rs) contains the deterministic single-store baseline
  model, create/approve/reject/cancel operations, ordered audit queries,
  injected failure, role-denial model, and the two real-Fold boundary
  reproductions.
- [`src/main.rs`](src/main.rs) runs the comparison and prints the outcome.
- [`baseline.sql`](baseline.sql) records the recommended PostgreSQL table,
  timeline index, and role-grant shape.
- `Cargo.lock` pins the runnable prototype dependencies.

The model stages each current-state change and audit event before its commit
point. A forced failure returns before either becomes visible. Events have a
monotonic sequence and snapshot request amount, previous and new status,
actor, and policy version.

For the 10,000-event check, the prototype creates 5,000 requests and applies
one approve, reject, or cancel transition to each. It then reads the first
10,000 events in global sequence order. This is a deterministic local data
structure check, not a PostgreSQL query benchmark.

## Exact commands and results

All commands below were run from the sanitized repository or the prototype
directory on July 28, 2026.

### Public starter attempt

```console
$ cargo run -p starter
error: failed to run custom build command for `ese v0.1.0`
...
cargo:warning=Downloading .../target/ese-cache/model.safetensors...
download failed: ... "failed to lookup address information: nodename nor servname provided, or not known"
```

Result: failed. This was a discovery issue, not evidence against Fold's audit
semantics.

### Formatting

```console
$ rustfmt --edition 2024 src/lib.rs src/main.rs
$ rustfmt --edition 2024 --check src/lib.rs src/main.rs
```

Result: passed with no output.

I first tried `cargo fmt --all -- --check`. Because the prototype has a local
path dependency on the repository, that command also reported existing format
differences in `examples/search/src/main.rs`. I did not change those existing
files and used the two-file check above.

### Tests with warnings denied

```console
$ RUSTFLAGS='-D warnings' cargo test --manifest-path Cargo.toml --all-targets --offline
running 8 tests
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.19s
running 0 tests
test result: ok. 0 passed; 0 failed
```

The first online attempt tried to refresh the crates.io index and failed DNS.
All required dependencies were already cached, so `--offline` was the
deterministic command.

### Strict lint check

```console
$ RUSTFLAGS='-D warnings' cargo clippy --manifest-path Cargo.toml --all-targets --offline -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.86s
```

Result: passed with no warnings or lint findings.

### Runnable comparison

```console
$ cargo run --manifest-path Cargo.toml --offline --quiet
Decision: use the PostgreSQL baseline; BogKit is not a fit.
Baseline forced failure: request absent, audit count unchanged at 2.
Baseline timeline: [Create, Approve].
Split-store failure: current state Approved, Fold has 1 older event.
Fold writer retraction: 0 events remain.
Local-model query: 10000 events in 889.042µs.
```

Result: passed. The measured query was below 250 ms on this run. It is evidence
only for the local model.

## Acceptance results

| Criterion | Local result | What remains unproven |
| --- | --- | --- |
| Exactly one event per successful mutation | Pass in the baseline model for create, approve, reject, and cancel | SQLx/PostgreSQL integration |
| Failure leaves neither state nor event | Pass in the baseline model | Real rollback on database, driver, and process failures |
| Ordered, accurate timeline | Pass; event sequence and policy snapshots are asserted | PostgreSQL query plan and concurrent ordering |
| Ordinary app role cannot update/delete | Pass in the model; reference SQL revokes these privileges | Grants under a real non-owner role |
| 10,000-event query under 250 ms | Pass at 889.042 microseconds on this local-model run | PostgreSQL data volume, cache state, hardware, and index plan |
| Added write p95 under 20 ms | Not tested | PostgreSQL load test with 150-user concurrency |
| Seven-year immutable storage | Structurally represented, not tested | Capacity, backup/restore, permissions, and operations over about 2.8 million rows |

## Categorized findings

### Architecture

**A1 — Fixed storage and transaction boundary**

- Severity: Critical
- Confidence: High
- Finding: Fold opens and commits its own embedded-store transaction. It cannot
  atomically join a caller-owned PostgreSQL transaction, and replacing
  PostgreSQL with Fold would violate a fixed scenario requirement.
- Reproduction:
  `cargo test fold_sidecar_cannot_share_the_state_commit --offline`
- Observed result: the current state is `Approved`, while Fold contains only
  the earlier `Create` event.
- Minimal improvement: add a capability matrix to the public documentation
  that distinguishes embedded source-of-truth use from external-database
  integration. A PostgreSQL-backed sink would be a substantial architectural
  addition and is not proposed from this trial.

### Scenario fit

**S1 — A Fold writer can retract audit events**

- Severity: High
- Confidence: High
- Finding: The scenario requires a role-protected append-only audit log, while
  `Bag` is intentionally a retractable multiset. This is a no-fit reason, not a
  Fold security defect.
- Reproduction:
  `cargo test fold_sidecar_allows_retraction_by_its_writer --offline`
- Observed result: zero events remain after insert followed by retraction.
- Minimal improvement: none to Fold from this evidence. Use PostgreSQL grants
  and the audit table designed for this requirement.

**Q1 — The general-purpose Bag is not an audit timeline index**

- Severity: Informational
- Confidence: High
- Finding: `Bag` documents serialized-element iteration rather than audit-log
  ordering. Choosing it for this scenario requires an explicit sort and is
  another fit mismatch, not an ordering defect.
- Reproduction: inspect the `fold_events.sort_by_key` call in `src/lib.rs` and
  the `BagReader::iter` documentation in Fold.
- Minimal improvement: none from this trial. An append-log terminal would not
  solve the transaction or role-separation requirements.

### Developer experience

**D1 — The advertised smallest example unexpectedly requires an embedding
model download**

- Severity: Medium
- Confidence: High
- Finding: `examples/starter` declares ESE and ANNy dependencies even though
  its source uses only Fold. The first-run starter build therefore invokes the
  ESE download.
- Reproduction: `cargo run -p starter`
- Observed result: build failure while downloading `model.safetensors` because
  DNS was unavailable.
- Minimal improvement: remove unused ESE and ANNy dependencies from the
  starter manifest or put them behind opt-in features.

**D2 — External transaction and concurrency limits are not surfaced early**

- Severity: Medium
- Confidence: High
- Finding: The top-level README says writes are transactional but does not say
  that the boundary is one embedded store. The chat example's single-writer
  shape appears only after reading its source.
- Reproduction: follow the discovery sequence above from `README.md` to
  `fold/src/stream`.
- Minimal improvement: state the storage engine, external-transaction
  limitation, and single-writer architecture in the top-level Fold
  description.

### Positive evidence

**P1 — Fold's internal transaction behavior is clear and useful within its
intended boundary**

- Severity: Informational
- Confidence: High
- Finding: Current keyed state and Fold materialized views share one embedded
  transaction, and panic handling aborts pending pipeline state.
- Reproduction: the Fold stream and keyed-stream implementations, plus the
  public starter example.
- Minimal improvement: none for embedded use; the issue is fit, not a failure
  of the documented internal transaction.

## Decision audit

1. **PostgreSQL current state plus PostgreSQL audit table — selected.** It is
   the only option evaluated that can use one native transaction for the
   mandatory state and event writes. It also has direct role grants and a
   conventional ordered index.
2. **PostgreSQL current state plus Fold audit sidecar — rejected.** Either
   commit order has a partial-success window. State-first can lose the audit
   event; Fold-first can retain an event for a mutation that never committed.
   Retry logic cannot remove the crash window without a shared transaction.
3. **Fold as the source of truth — rejected.** It could keep its own state and
   views atomic, but it directly violates the PostgreSQL source-of-truth
   requirement and would create an unnecessary migration.
4. **PostgreSQL outbox feeding Fold — rejected for this scope.** A PostgreSQL
   outbox row could be atomic with the mutation, but the immutable audit table
   already is that durable record. Copying it into Fold adds lag, duplicate
   storage, replay, and operational work without improving the required
   timeline.
5. **ESE or ANNy — not applicable.** Neither embeddings nor nearest-neighbor
   search help transaction atomicity, immutability, or ordered audit queries.

The no-fit decision does not claim Fold is generally unsuitable. It says that
its strongest feature here—atomic incremental views in one embedded
store—ends at exactly the boundary this finance system must cross.
