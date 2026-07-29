# Blind trial report: edge spool pressure

## Decision

**No fit for the strict 256 MiB edge spool.**

Fold is useful for atomic queue state, durable upload intent, and durable
drop counters. The prototype recovered every retained event in the two
representative crash cases and explained the duplicate window.

The blocker is the disk-bound guarantee. Fold exposes a logical view of
records, but its public interface does not expose a hard allocated-byte limit
or a documented way to make eviction and compaction satisfy one. In the
minimal probe, the queue held 1,048,512 logical bytes under a 1,048,576-byte
logical limit, while the database had 3,305,472 allocated bytes. This proves
that logical accounting is not a physical cap. It does not predict exact
allocation at 256 MiB or test an external filesystem quota.

This is a deliberately bounded no-fit result. I did not run one million
events through the Fold candidate after the quota prerequisite failed.

## Deliverables

- Prototype: `runs/2026-07-29--edge-spool-pressure/`
- Main executable: `runs/2026-07-29--edge-spool-pressure/src/main.rs`
- Prototype instructions: `runs/2026-07-29--edge-spool-pressure/README.md`
- This report: `runs/2026-07-29--edge-spool-pressure/EVIDENCE.md`

The prototype uses only `fold` by a local path and `serde`. It has no
network client and makes no external writes beyond its own temporary test
directories.

## What the prototype compares

### Existing newline-file baseline

The baseline is a deterministic streaming model, not a disk implementation.
It generates one million events with the required exact priority mix:

- 850,000 debug
- 120,000 operational
- 30,000 critical

Payloads range from 100 bytes to 8 KiB. The model rolls 4 MiB newline files,
caps retained file bytes at 256 MiB, deletes the oldest whole file, and
models the duplicate window from retrying a whole file after a mid-file
disconnect.

Observed result:

- Generated: 1,000,000
- Retained: 770,212
- Retained modeled bytes: 267,030,039, below 256 MiB
- Deleted as oldest: 229,788
- Critical events deleted: 6,894
- Security events deleted: 4,596
- Hardware-failure events deleted: 2,298
- One modeled whole-file retry exposes up to 12,064 events to duplication
- Model runtime: 79-89 ms in the final runs

The age-only policy therefore allows bulk debug traffic to delete critical
events, and whole-file retry creates a large unexplained duplicate window
unless the daemon records the active file and offset.

### Fold candidate

The candidate uses one Fold stream with four durable views:

1. A priority-ordered event bag.
2. Per-priority and per-category retained counts and logical bytes.
3. Drop counts keyed by priority, category, and reason.
4. A durable upload intent that contains the attempted batch.

The first serialized event field is a priority rank. Fold documents that a
Bag iterates in postcard-encoded order, so reads return critical,
operational, then debug events. When a higher-priority event needs room, the
prototype removes the oldest eligible lower-priority records and increments
their drop counters in the same transaction.

Before upload, the candidate stores and checkpoints an intent. After the
collector accepts the batch, it removes the retained events and the intent
in one transaction. If the process dies after sending but before that
transaction, every intended event remains retained. The intent gives a
conservative possible-duplicate count.

## Verified results

### Representative priority and quota workload

The real Fold-backed run used 20,000 generated events and a 4 MiB logical
limit.

- Retained: 13,319
- Retained logical bytes: 4,194,221
- Allocated database bytes: 6,676,480
- Apparent database file bytes: 67,138,588
- Critical retained: all 600 generated
- Operational retained: all 2,400 generated
- Debug retained: 10,319
- Debug evicted for higher priority: 1,024
- Incoming debug dropped for lack of eligible space: 5,657
- Runtime: 8.0–9.2 seconds across reviewed and final archive runs

The run preserves critical and operational events ahead of debug events and
accounts for every drop in this representative scope. It also shows that
logical byte accounting is not a disk cap. The demonstrated candidate stage
processed roughly 2,200–2,500 events per second when its checkpoints and
verification were included. This is below the scenario's
5,000-events-per-second burst and is a prototype limitation, not a general Fold
throughput result.

### Crash while a write transaction is open

The parent created and checkpointed one event. A child process began a
100-event transaction and exited with code 72 after its fiftieth insert,
before the transaction returned.

After reopen:

- Retained count: 1
- Retained event IDs matched the pre-crash set: true
- None of the partial transaction appeared

### Crash during upload

The queue held 120 events. A child durably recorded a 25-event upload
intent, wrote 7 event IDs to the mock collector, and exited with code 73
before acknowledging the queue.

After reopen:

- Retained count: 120
- All 120 retained IDs matched: true
- Reported possible duplicates: 25
- Recovery and inspection: 4 ms in the final timed runs

The retry sent all 25 intended events:

- Collector deliveries: 32
- Unique events: 25
- Actual duplicates: 7
- Retained after durable acknowledgement: 95

At-least-once delivery is therefore explained: the intent bounds possible
duplicates at 25, and the mock collector observed the exact 7 already sent
before the crash.

### Minimal disk-limit failure

The probe used a 1 MiB logical limit. It first added 8 KiB debug events,
then 8 KiB critical events that evicted debug records.

- Logical limit: 1,048,576 bytes
- Retained logical bytes: 1,048,512
- Allocated database bytes from Unix block accounting: 3,305,472
- Apparent database file bytes: 67,123,220
- Strict allocated-byte limit satisfied: false

This is the minimal acceptance failure. It does not prove what the exact
overage would be at 256 MiB. It proves that the prototype's logical control
loop cannot enforce a hard allocated-byte cap through Fold's public interface.
External filesystem enforcement was not evaluated.

### Memory

I sampled the final demonstration process every 50 ms with `ps`.

- Simulator maximum sampled resident memory: 19,392 KiB
- Reviewer maximum sampled resident memory: 19,456 KiB
- Limit in the brief: 65,536 KiB

This is evidence only for the one-million-event streaming baseline model
plus the 20,000-event real candidate run. It is not a million-event Fold
memory claim, and 50 ms sampling can miss a short peak.

## Acceptance audit

| Acceptance item | Result | Evidence boundary |
| --- | --- | --- |
| Deterministic one-million-event workload | Partial | The baseline model processes exactly one million events. The Fold candidate was not scaled to one million after the disk-cap prerequisite failed. |
| Stay within 256 MiB disk and 64 MiB memory | No fit / partial | The 1 MiB probe shows that the public interface's logical accounting is not a hard allocated-byte guarantee. Exact 256 MiB behavior and external quotas were not tested. Sampled host RSS was 19,392–19,456 KiB and may miss short peaks. |
| Preserve critical ahead of lower priority | Pass, representative | All 600 critical events were retained in the 20,000-event run; only debug events dropped. |
| Recover every retained event after crashes | Pass, representative | Exact ID sets matched after the interrupted write and interrupted upload cases. |
| Report possible duplicates | Pass, representative | Intent reported 25 possible; collector observed 7 actual duplicates. |
| Dropped counts by priority/category/reason | Pass, representative | Durable counters reported 1,024 debug/diagnostics evictions and 5,657 incoming debug/diagnostics drops. |
| Recovery within two seconds | Pass, representative | Reopen plus exact-ID and intent inspection took 4 ms. |
| One CPU core | Unresolved | The executable is single-writer, but I did not measure or constrain internal database background threads. |
| Burst to 5,000 events per second | Unresolved | The candidate stage demonstrated roughly 2,200–2,500 events per second with prototype checkpoint and verification work. No sustained production ingest test was run. |

## Ordered discovery and friction trail

1. I confirmed the assigned checkout and listed only its root README,
   examples, and Cargo manifests.
2. I read the public root `README.md` first. It described Fold as a
   persistent incremental framework and recommended the project generator.
3. I read the root workspace manifest and the `starter`, `timeseries`,
   `chat`, and `search` examples in that order. The starter example made the
   atomic write and persistent Bag pattern clear. The other examples were
   useful context but not required for this problem.
4. I did not run `scripts/new-project.sh`. Its README description says it
   adds `fold`, `anny`, and `ese`; this prototype needs only Fold. I created
   a standalone crate with a local Fold path instead.
5. Before selecting Fold, I specified and implemented the age-only
   newline-file baseline. The one-million-event result demonstrated
   priority-blind critical loss and whole-file duplicate exposure.
6. I then inspected Fold's public module documentation and the Stream, Bag,
   Table, Aggregate, and Retain implementations. Stream transactions, Bag
   ordering, and Aggregate views fit the atomic accounting problem.
   Retain did not fit because it is time-based rather than byte- and
   priority-based.
7. The first `cargo check` could not resolve `index.crates.io`. I retried
   with network access and Cargo downloaded the checkout's declared
   dependencies. No credential or source-code problem was involved.
8. The first test compile found one method-reference type mismatch in a
   test. I replaced it with an explicit closure.
9. The first warnings-denied Clippy run found two manual modulus checks. I
   used Rust's `is_multiple_of` method and reran the lint successfully.
10. `/usr/bin/time -l` could not read `kern.clockrate` in the restricted
    environment. I used `/usr/bin/time -p` for elapsed time and sampled
    resident memory with `ps` every 50 ms.
11. My first directory-size helper summed apparent file lengths. A `du`
    cross-check showed that sparse/preallocated files made that an
    unsuitable physical-disk measure. I changed the probe to use Unix
    allocated block counts. Both values remain reported, with the quota
    decision based on allocated bytes.

## Exact validation commands and results

From `developer-simulation/`:

```console
cargo test -p edge-spool-pressure --all-targets
```

Result: 10 passed, 0 failed, 0 ignored.

```console
cargo fmt --check -p edge-spool-pressure
```

Result: exit 0 with no formatting differences.

```console
cargo clippy -p edge-spool-pressure --all-targets -- -D warnings
```

Result: exit 0 with no warnings.

```console
cargo build -p edge-spool-pressure --release
```

Result: optimized build completed successfully.

```console
/usr/bin/time -p target/release/edge-spool-pressure demo
```

Result: exit 0. The simulator observed 10.31 seconds and final archive
verification observed 11.95 seconds. The detailed baseline, candidate, crash,
retry, and quota results are recorded above.

Final memory sampling command:

```console
target/release/edge-spool-pressure demo &
devsim_pid=$!
devsim_max_rss=0
while kill -0 $devsim_pid 2>/dev/null; do
  devsim_rss=$(ps -o rss= -p $devsim_pid | tr -d ' ')
  if [[ -n $devsim_rss && $devsim_rss -gt $devsim_max_rss ]]; then
    devsim_max_rss=$devsim_rss
  fi
  sleep 0.05
done
wait $devsim_pid
```

Result: exit 0; the simulator sampled 19,392 KiB maximum RSS and the reviewer
sampled 19,456 KiB. The 50 ms interval can miss short peaks.

## Categorized findings

### Baseline correctness defect: age-only deletion loses critical events

- Severity: high
- Confidence: high
- Scope: defect in the supplied newline-file baseline, not in BogKit
- Evidence: The deterministic baseline deleted 6,894 critical events,
  including 4,596 security and 2,298 hardware-failure events.
- Reproduction: Run the `baseline` command or the full demo.
- Smallest plausible improvement: Separate files or byte budgets by
  priority, and record every eviction with priority, category, and reason.

### Baseline correctness defect: whole-file retry has a large explanation gap

- Severity: high
- Confidence: high
- Scope: defect in the supplied newline-file baseline, not in BogKit
- Evidence: The modeled first retained file contains 12,064 events. A
  disconnect after any prefix followed by whole-file retry exposes that prefix
  to duplication, while the baseline has no durable request intent or offset.
- Reproduction: Run the `baseline` command.
- Smallest plausible improvement: Durably record the exact attempted batch
  before sending, then clear it atomically with retained-event
  acknowledgement.

### Prototype limitation: existing ranked range scans were not evaluated

- Severity: informational
- Confidence: high that the trial left an alternative untested; low that Fold
  itself causes the observed throughput
- Evidence: The prototype uses a full Bag iterator for priority eviction and
  its candidate stage processed roughly 2,200–2,500 events per second. Fold
  already exposes `Ranked` range scans over tuple scores, but the trial did not
  try that API.
- Reproduction: Run the 20,000-event demo and inspect the eviction path.
- Smallest plausible improvement: Evaluate the existing `Ranked` API before
  proposing any range-scan or eviction API.

### API friction: removal requires reproducing the full event

- Severity: informational
- Confidence: medium
- Evidence: `Stream::remove` needs the exact original value. The upload
  intent stores full event copies so acknowledgement can retract them.
  `KeyedStream` removes by key, but its primary table does not have a public
  iterator; adding a terminal Table would duplicate payload storage.
- Reproduction: Inspect `acknowledge_upload` and compare it with Fold's
  `Stream` and `KeyedStream` APIs.
- Smallest plausible improvement: None from this one trial. Evaluate
  `KeyedStream` and bounded intent representations before proposing an API.

### Documentation gap: no edge-storage suitability boundary

- Severity: medium
- Confidence: high
- Evidence: The root README explains persistence and atomic writes, but it
  does not discuss allocated disk growth, compaction, database minimums,
  hard quotas, write amplification, or memory/thread bounds. I had to read
  implementation files and build the quota probe.
- Reproduction: Start from the root README and examples as instructed.
- Smallest plausible improvement: Add a “storage limits and durability”
  section with hard-limit non-goals, allocated-vs-logical sizing, process
  crash vs power-loss guarantees, and operational sizing guidance.

### Missing scenario capability: hard allocated-byte quota

- Severity: critical
- Confidence: high that the public interface has no documented hard-cap
  guarantee; medium for behavior at exactly 256 MiB because that scale was not
  run
- Evidence: A 1 MiB logical limit produced 3,305,472 allocated bytes. Fold
  exposes checkpointing but no public allocated-byte quota or compaction
  control.
- Reproduction: Run `quota-probe` in a new directory.
- Smallest plausible improvement: Document hard allocated-byte limits as an
  unsupported boundary. Consider admission control, compaction reserve, and
  allocated-byte telemetry only if strict quotas are an intended use case.

### Poor product fit: strict bounded edge spool

- Severity: critical
- Confidence: high
- Evidence: The candidate passes representative transaction, recovery,
  priority, duplicate, and accounting checks but fails the disk-bound
  prerequisite.
- Reproduction: Run the full demo.
- Smallest plausible improvement: Use a storage engine designed for a hard
  ring/segment budget, while retaining Fold only for small derived counters
  if its own database allocation is separately bounded.

## Decision audit

### Consequential choices

- I evaluated the newline-file baseline before selecting a candidate.
- I used `Stream` plus Bag rather than KeyedStream. This avoids storing every
  payload in both the KeyedStream root and a separately iterable Table.
- I put the priority rank first in the serialized Event so the documented
  Bag order provides critical-first upload without an in-memory sort.
- I made eviction and drop accounting one Fold transaction.
- I made upload intent durable before collector delivery and cleared it in
  the same transaction as retained-event removal.
- I kept upload batches small, so storing full events in the intent stays
  bounded in this prototype.
- I based the no-fit decision on allocated Unix blocks, not apparent file
  lengths.

### Rejected alternatives

- `Retain`: It expires by elapsed time and cannot express byte pressure or
  priority preservation.
- `TopK`: It is count-based, not byte-based, and does not provide the
  required durable drop reason accounting.
- `KeyedStream` plus terminal Table: It enables removal by ID but duplicates
  payloads and still does not provide a hard disk cap.
- Direct use of Fold's internal Fjall store: It would bypass the public
  BogKit interface and still require designing compaction headroom and
  quota guarantees.
- Exactly-once delivery: Explicitly outside scope and unnecessary; durable
  intent makes at-least-once behavior explainable.
- Scaling the Fold candidate to one million events: Rejected after the
  minimal disk-limit prerequisite failed. Doing so would add runtime and
  disk cost without changing the fit decision.
- `Ranked` range scans: Not evaluated. This is why the review rejected the
  trial's proposed range-scan API and performance attribution.

### Unresolved uncertainty

- Exact allocated-byte behavior at a 256 MiB logical limit.
- Sustained ingest at 5,000 events per second.
- One-million-event Fold recovery time and memory.
- Behavior under power loss rather than process termination.
- Behavior when the operating system terminates the process during the
  storage engine's own commit sequence.
- Internal database background-thread CPU use under compaction.
- Long-running allocated space after multiple compaction cycles.
- Enforcement through an external filesystem quota.
- A real HTTP client's buffering and disconnect behavior; the mock
  collector persists event IDs locally.
- Cross-version stability of a schema that deliberately relies on postcard
  field ordering.

These uncertainties do not overturn the no-fit decision because the required
hard-cap guarantee is absent from the public interface the trial evaluated.
