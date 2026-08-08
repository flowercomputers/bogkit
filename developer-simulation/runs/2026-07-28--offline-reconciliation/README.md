# Offline warehouse reconciliation evaluation

## Outcome

BogKit is usable as a local proof store, but this trial did not demonstrate a
BogKit-specific advantage and it is not a fit for the required production
architecture.

Fold can persist immutable operation records atomically and support a
deterministic local projection. The prototype scans all stored operations and
recomputes its snapshot, so it does not demonstrate incremental
materialization. It also lacks a direct Fjall, SQLite, or PostgreSQL control.
Fold's embedded single-writer store does not replace the required PostgreSQL
source of truth or horizontally deployed API. The production recommendation is
to keep the algorithm and tests, but implement them in PostgreSQL.

## Persona and existing system

I approached BogKit as Mateo, a warehouse device developer seeing it for the
first time. I have six months of Rust experience after working in Go and
Kotlin.

Forty Android scanners currently store pallet rows in SQLite and upload those
rows to a central Rust/PostgreSQL API. Devices can remain offline for 12 hours.
Duplicate, reordered, interrupted, or concurrent uploads can silently restore
an old pallet location.

The baseline stores immutable operations in PostgreSQL under a unique
`(device_id, sequence)` key, derives current pallet state centrally, and puts
incompatible moves into a review queue.

## Discovery and friction trail

1. I read the root README, then the starter, time-series, chat, and search
   examples in their published order.
2. Starter and time-series made Fold's durable transactional views clear.
3. Chat exposed the intended single-owner write architecture.
4. Search exposed `KeyedStream::upsert` replacement semantics.
5. I read the documented stream, keyed stream, table, aggregation, and keyed
   stream test APIs.
6. The first ordinary Cargo build could not resolve `index.crates.io`. An
   offline build worked from locally cached dependencies.
7. The first interruption test proved disk rollback, but the deliberately
   caught panic poisoned Fjall's writer lock. Reopening the store, as a
   restarted process would, allowed the retry.
8. Strict lint found an oversized diagnostic error. Boxing the stored and
   incoming operations kept the diagnostic without retaining a large error
   value.

## Prototype

The CLI accepts JSON Lines operation batches and persists them in a Fold
`KeyedStream` keyed by `(device_id, sequence)`. Before each upsert it rejects a
different operation that reuses an existing identity. A deterministic snapshot
groups each pallet's latest operation from every device:

- one latest location becomes the current location;
- disagreeing latest locations become an explicit conflict containing every
  candidate;
- timestamps are retained as evidence but never decide ordering.

The prototype supports `ingest`, `show`, `demo`, and `benchmark` commands.

## Exact commands and observed results

Run from `developer-simulation/`:

```console
$ rustfmt --edition 2024 --check \
    runs/2026-07-28--offline-reconciliation/src/lib.rs \
    runs/2026-07-28--offline-reconciliation/src/main.rs

$ RUSTFLAGS='-D warnings' cargo test \
    -p offline-reconciliation --all-targets --offline
running 7 tests
test result: ok. 7 passed; 0 failed

$ RUSTFLAGS='-D warnings' cargo clippy \
    -p offline-reconciliation --all-targets --offline -- -D warnings
Finished `dev` profile

$ cargo run -p offline-reconciliation --offline -- \
    demo /tmp/bogkit-offline-reconciliation-demo.db
first upload: 2 inserted
exact replay: 2 duplicates
thread 'main' panicked at ... Box<dyn Any>
interrupted upload: rolled back
whole-batch retry: 2 inserted

$ cargo run -p offline-reconciliation --offline --release -- \
    benchmark /tmp/bogkit-offline-reconciliation-benchmark.db 20000
received=20000 inserted=20000 duplicates=0 stored=20000 pallets=1000 elapsed_ms=57
```

The simulator also ran 128 shuffled arrival orders. Every order produced the
same snapshot. The exact sample replay inserted nothing, and the two disagreeing
locations for `pallet-7` remained visible as conflict candidates.

The panic diagnostic is printed by Rust's panic hook even though the
interruption is caught and the program continues after reopening the store.

The simulator observed 57 ms; the skeptical review observed 54 ms; final
archive verification observed 76 ms. These synthetic, machine-specific results
show only that the prototype clears five seconds. Without a direct control they
do not show that BogKit improves on Fjall, SQLite, or PostgreSQL.

## Findings

### Missing causality metadata

- Category: missing capability in the scenario data
- Severity: Critical
- Confidence: High
- Finding: Device ID, local sequence, and device timestamp cannot distinguish a
  genuinely concurrent cross-device move from a later edit that observed the
  first move.
- Reproduction: the same pair of cross-device operation records can describe
  either history.
- Minimal improvement: include a per-pallet base revision or observed-head
  identifiers with every operation. Until then, conservatively review every
  disagreeing cross-device frontier.

### Production storage mismatch

- Category: poor product fit
- Severity: High
- Confidence: High
- Finding: Fold uses an embedded single-writer store, while the required
  service keeps PostgreSQL and runs multiple API instances.
- Reproduction: compare the chat example's single-owner write path with the
  scenario constraints.
- Minimal improvement: document the embedded source-of-truth boundary clearly.
  A PostgreSQL-backed materialization path would be an architectural addition,
  not a small adapter.

### Keyed upsert is replacement, not immutable insertion

- Category: usage constraint
- Severity: Informational
- Confidence: High
- Finding: Reusing `(device_id, sequence)` with changed content replaces the
  earlier value unless the caller checks it. This behavior is explicit in the
  API and is not a defect.
- Reproduction: `KeyedStream::upsert` retracts the previous value and inserts
  the replacement; the prototype's divergent-identity test guards it.
- Minimal improvement: none from one trial. The existing transactional
  `get`-then-`upsert` path implements compare-and-reject safely.

### Caught-panic test caveat

- Category: test-harness caveat
- Severity: Informational
- Confidence: High
- Finding: The injected panic rolled back disk state but poisoned the current
  embedded writer for later writes, and Rust's panic hook printed a diagnostic
  even though the harness caught the panic.
- Reproduction: the interruption test must reopen the model before retrying.
- Minimal improvement: no product change from this synthetic test. A real
  process restart naturally reopens the store; repeat evidence would be needed
  before proposing recovery behavior.

### Onboarding does not expose component boundaries

- Category: documentation gap
- Severity: Medium
- Confidence: High
- Finding: The README and project generator do not quickly explain which
  component fits which storage and concurrency shape.
- Minimal improvement: add a concise capability matrix and an immutable-event
  example.

### Local atomic persistence worked

- Category: positive evidence
- Severity: Informational
- Confidence: High
- Finding: Fold supported atomic local batches, checkpointed reopen, exact
  replay checks, and local ingestion under the scenario threshold. The
  snapshot still performs a full scan and recomputation.

## Decision audit

1. **Immutable operations with caller-side identity checks — selected for the
   proof.** Fold's upsert API alone would silently replace divergent content.
2. **Conservative conflict detection — selected.** The available operation
   schema lacks cross-device causality, so false-positive review is safer than
   silent loss.
3. **Fold as the local proof store — selected.** It exercises atomic batch,
   checkpointed reopen, and deterministic read behavior with little
   infrastructure. No control was built, so this is convenience evidence, not
   a demonstrated advantage.
4. **Fold as the production source of truth — rejected.** It conflicts with the
   fixed PostgreSQL and horizontal-deployment requirements.
5. **Timestamps for conflict resolution — rejected.** Device clocks cannot be
   trusted or coordinated.

## Limits

- The conflict model deliberately over-reports until causality metadata exists.
- Snapshot derivation scans and recomputes all stored operations; it does not
  exercise Fold's incremental materialization advantage.
- Interruption uses a caught panic and reopen, not an operating-system kill.
- No direct Fjall, SQLite, or PostgreSQL control was built.
- Warehouse-specific action compatibility is intentionally undefined.
- No review-resolution workflow is implemented.
