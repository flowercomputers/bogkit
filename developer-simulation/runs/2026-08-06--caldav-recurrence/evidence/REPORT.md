# BogKit daily-lab trial report

Date: 2026-08-06

Scenario: CalDAV recurrence and time-zone correctness

Scope: only `/private/tmp/bogkit-sim-2026-08-06-one-rEQ2PZ`. No other checkout,
prior lab report, developer-simulation directory, or GitHub surface was
inspected or changed.

## Result in one paragraph

The stated baseline is not sufficient for the required correctness or
determinism guarantees. A standalone prototype was built under `developer-simulation/runs/2026-08-06--caldav-recurrence/`. It
uses Fold only for the event-master upsert/remove boundary; calendar rules,
civil-time conversion, per-UID shards, and publication stay outside Fold. The
local test suite passes. A fresh release run over 5,000 masters and 2,000,000
candidate occurrences, with 100 supplied transition tables, completed in 3.85
seconds wall time and reported 17,547,264 bytes peak RSS on this host. A
second run reused all 5,000 fingerprinted shards in 2.93 seconds and produced
byte-identical output. The supplied
5,000-case oracle and production SQLite service are not present in this
checkout, so neither production correctness nor oracle agreement is claimed.

## Ordered discovery and friction trail

The commands below are in the order used for onboarding and inspection.

1. `sed -n '1,260p' README.md`

   Observed the public onboarding path: run `scripts/new-project.sh`, then the
   examples in the order `starter`, `timeseries`, `chat`, `search`. The README
   describes Fold as a persistent, incrementally maintained dataflow engine;
   it does not describe recurrence or time-zone primitives.

2. `rg --files examples/starter | sort`

   Observed only `Cargo.toml` and `src/main.rs`. Reading both showed the
   smallest Fold shape: `Stream`, `wtx`, `rtx`, `Count`, and `Bag`. The public
   `starter` manifest also declares unused `anny` and `ese` dependencies.

3. `cargo run -p starter`

   Observed a build failure in `ese` while its build script attempted to
   download `target/ese-cache/model.safetensors`; name resolution was
   unavailable. This is onboarding friction in the example dependency graph,
   not a recurrence finding.

4. `rg --files examples/timeseries | sort`, followed by reading its manifest
   and `src/main.rs`

   Observed `KeyBy` + `Aggregate` + `Table`, explicit snapshot sorting, and
   retraction-safe aggregates.

5. `cargo run -p timeseries`

   Observed a successful run: six readings were materialized, hourly and daily
   views were printed, one rainy reading was retracted, and the updated totals
   were correct. This was the first public example that ran end to end in this
   environment.

6. `rg --files examples/chat | sort`, followed by reading its manifest and
   `src/main.rs`

   Observed the documented single-ingest-thread pattern, transactional writes,
   watch snapshots, and explicit sorting of client-facing state.

7. `cargo run -p chat`

   Observed successful compilation and startup text, followed by a bind panic:
   `Operation not permitted` at the listener bind. This is a sandbox network
   restriction, not a BogKit dataflow defect.

8. `rg --files examples/search | sort`, followed by reading its manifest and
   `src/main.rs`

   Observed `KeyedStream`, `upsert`, remove-by-key, BM25, HNSW, and hybrid
   search. The example is unrelated to calendar semantics but reinforced the
   keyed replacement model.

9. `cargo run -p search`

   Observed the same ESE model download/name-resolution failure as `starter`.

10. Read the public Fold source documentation for `Stream`, `KeyedStream`,
    `KeyedTx::upsert`, `KeyedTx::remove`, `Table`, `Aggregate`, and the public
    tests for keyed transactions.

    Observed that keyed upserts retract the old record before inserting the new
    record, transactions are atomic, reads use one snapshot, and reopening the
    same path resumes state. Also observed that Fold has no recurrence, civil
    time, transition-table, SQLite, or JSONL publication abstraction.

11. Wrote [`BASELINE.md`](../BASELINE.md) before selecting a component.

12. Added the standalone prototype, fixtures, tests, and this report under
    `developer-simulation/runs/2026-08-06--caldav-recurrence/`; no root Cargo file, Fold source, or existing example was changed.

13. The first fresh release workload attempt measured 30.09 seconds. Inspection
    identified 5,000 per-shard fsyncs plus a second full read of all shards to
    assemble the output. The publication path was changed to stream one sorted
    atomic output pass while still retaining per-UID shards for recovery.

14. The corrected fresh release workload measured 3.85 seconds wall time and
    17,547,264 bytes peak RSS. A second run reused all 5,000 shards after
    verifying their byte lengths and SHA-256 fingerprints. Separate `TZ=UTC`
    and `TZ=Pacific/Honolulu` runs produced byte-identical output.

## Prototype model and boundaries

The CLI is:

```text
caldav-recurrence-prototype \
  --events EVENTS.jsonl \
  --transitions ZONES.json \
  --from RFC3339 --to RFC3339 \
  --output OCCURRENCES.jsonl \
  --state-dir STATE [--edits EDITS.jsonl]
```

The input format is deliberately small and documented in the CLI help and
fixtures. It supports timed and all-day events, one `DAILY`, `WEEKLY`, or
`MONTHLY` rule, interval/count/until, weekly `BYDAY`, monthly `BYMONTHDAY`,
EXDATE values, and confirmed or cancelled occurrence overrides.

The prototype gives each occurrence a stable `(UID, recurrence_id)` identity.
All-day values remain civil dates. UTC values bypass local conversion. Local
and floating times use only the supplied transition table. The chosen policy is
explicit and deterministic: a fall-back fold chooses the earlier UTC instant;
a spring-forward gap shifts the nonexistent wall time forward by the gap.
Output is sorted by UID and recurrence ID and contains no summary, location, or
event text.

Fold's `KeyedStream<String, Event, Table<String, Event>>` is used for the
event-master cache. A full validated input snapshot and optional edits are
applied as one keyed transaction; absent UIDs are removed, replacements retract
the old master, and the store is checkpointed. Each UID has a JSONL shard and
SHA-256/length fingerprint. A changed or tampered UID rebuilds only its shard.
All desired events are expanded and validated before the Fold event-store
transaction; the final output is written to a temporary file, synced, and
renamed only after all input and expansion work succeeds. An interrupted run
leaves the previous published output intact; the next run ignores any
unreferenced shard and rebuilds from the manifest.

This is not a SQLite adapter and it is not a CalDAV server. JSONL stands in for
a validated export at the prototype boundary. The production integration must
keep SQLite authoritative and must connect the existing HTTP/CalDAV layer to
the same occurrence identity and publication rules.

## Verification commands and observed results

All Rust commands below were run with `--offline` after the public onboarding
attempts showed that network access was unavailable.

```text
cargo fmt --manifest-path developer-simulation/runs/2026-08-06--caldav-recurrence/Cargo.toml -- --check
```

Final result: passed. One intermediate check correctly reported formatting
differences after the 100-zone fixture generator was added; `cargo fmt` fixed
them and the final check passed.

```text
cargo test --manifest-path developer-simulation/runs/2026-08-06--caldav-recurrence/Cargo.toml --offline --all-targets
```

Final result: 10 integration tests passed. They cover DST gap/fold conversion,
all-day and floating times, partial-day query intersection, daily/weekly/monthly
rules, exclusions, canonical and unseen override rejection, stable ordering,
host `TZ` independence, tampered-shard rebuilds, preflight store integrity,
single-UID rebuilds, interruption recovery, and no publication after malformed
input.

```text
cargo clippy --manifest-path developer-simulation/runs/2026-08-06--caldav-recurrence/Cargo.toml --offline --all-targets -- -D warnings
```

Final result: passed with warnings denied.

```text
developer-simulation/runs/2026-08-06--caldav-recurrence/target/debug/caldav-recurrence-prototype --help
```

Final result: passed; the usage text contains no stray help-marker character.

```text
cargo run --manifest-path developer-simulation/runs/2026-08-06--caldav-recurrence/Cargo.toml --offline --bin generate_fixture -- developer-simulation/runs/2026-08-06--caldav-recurrence/evidence/workload-100zones
```

Observed: `generated 5000 masters and 2000000 candidate occurrences`. The
generated transition file contains 100 fixed transition tables plus
`FLOATING`.

```text
cargo build --manifest-path developer-simulation/runs/2026-08-06--caldav-recurrence/Cargo.toml --offline --release --bin caldav-recurrence-prototype
```

Observed: release build passed.

```text
/usr/bin/time -p developer-simulation/runs/2026-08-06--caldav-recurrence/target/release/caldav-recurrence-prototype \
  --events developer-simulation/runs/2026-08-06--caldav-recurrence/evidence/workload-100zones/workload-events.jsonl \
  --transitions developer-simulation/runs/2026-08-06--caldav-recurrence/evidence/workload-100zones/workload-zones.json \
  --from 2026-01-01T00:00:00Z --to 2027-06-01T00:00:00Z \
  --output developer-simulation/runs/2026-08-06--caldav-recurrence/evidence/workload-100zones-output-rerun.jsonl \
  --state-dir developer-simulation/runs/2026-08-06--caldav-recurrence/evidence/workload-100zones-state-rerun
```

Observed diagnostics:

```json
{"events":5000,"occurrences":2000000,"rebuilt_uids":5000,"reused_uids":0,"removed_uids":0,"resumed":false,"elapsed_ms":3585,"peak_rss_bytes":17547264,"publication":"atomic-rename"}
```

Observed `/usr/bin/time -p`: `real 3.85` on this host.

The output contained 2,000,000 lines. A second run with the same inputs reported
`rebuilt_uids:0`, `reused_uids:5000`, and `elapsed_ms:2923`; `cmp` found no byte
difference between the two rerun output files. Separate CLI runs under
`TZ=UTC` and `TZ=Pacific/Honolulu` also passed `cmp` with identical output.

## Findings

### F1 — Baseline has no stable occurrence identity or explicit local-time policy

- Severity: high
- Confidence: high
- Category: baseline design defect, not a BogKit defect
- Reproduction: the baseline expands local wall time, converts to UTC, replaces
  whole rows, and emits insertion order; it has no recurrence ID, fold/gap
  policy, all-day type distinction, or unique output key.
- Smallest improvement: retain a civil recurrence identity, model all-day dates
  separately, define supplied-table gap/fold rules, and publish by a stable key.

### F2 — Fold fits keyed event replacement, but not calendar semantics

- Severity: medium
- Confidence: high
- Category: component-fit finding, not a core defect
- Reproduction: public APIs provide keyed upsert/remove, snapshots, and
  retraction, but no RRULE parser, civil-date arithmetic, transition lookup,
  SQLite adapter, or artifact publisher.
- Smallest improvement: keep a narrow external calendar adapter and use
  `KeyedStream` for event-master replacement/materialization only. Do not force
  recurrence expansion into `Aggregate` or use `Bag` as a uniqueness index.

### F3 — Public `starter` and `search` examples require an unavailable ESE model

- Severity: low for this scenario; medium for onboarding
- Confidence: high
- Category: existing-example friction, not a prototype defect
- Reproduction: `cargo run -p starter` and `cargo run -p search` both fail in
  ESE's model download build step when DNS/network access is unavailable;
  `starter` also declares ESE even though its source does not use it.
- Smallest improvement: remove unused ESE/ANNy dependencies from `starter` and
  make model-dependent examples fail with a clearer opt-in setup message.

### F4 — Public `chat` cannot bind its demo listener in this sandbox

- Severity: low
- Confidence: high
- Category: environment restriction, not a BogKit defect
- Reproduction: `cargo run -p chat` compiles, prints its startup URL, then
  panics with `Operation not permitted` at `TcpListener::bind`.
- Smallest improvement: document a network-enabled run requirement or provide a
  non-listening smoke mode.

### F5 — Prototype does not yet preserve SQLite as the authoritative store

- Severity: high for production acceptance; low for the standalone prototype
  boundary
- Confidence: high
- Category: intentional prototype limitation, not a BogKit defect
- Reproduction: the CLI consumes event JSONL and stores its cache in Fold's
  local fjall state; no SQLite dependency is present in `developer-simulation/runs/2026-08-06--caldav-recurrence/Cargo.toml`.
- Smallest improvement: add a read/transaction adapter over the existing SQLite
  rows, keep Fold as a derived cache, and verify source-version plus publication
  atomicity against the production schema.

### F6 — External oracle and reference machine are absent

- Severity: high unresolved acceptance risk
- Confidence: high
- Category: missing evidence, not a BogKit defect
- Reproduction: the assigned checkout contains no supplied 5,000-case oracle and
  no reference-machine specification.
- Smallest improvement: run the prototype and the production candidate against
  the supplied oracle, especially its exact gap/fold, override, and all-day
  policies, then repeat the workload on the named reference machine.

### F7 — Prototype intentionally covers a bounded recurrence subset

- Severity: medium
- Confidence: high
- Category: prototype boundary
- Reproduction: unsupported RRULE fields, multiple rules, RDATE, ordinal
  BYDAY, and time-zone-changing overrides are rejected. This is deliberate
  validation, not silent approximation.
- Smallest improvement: add only the constructs present in the supplied oracle,
  one at a time, with a differential test for each.

## Skeptical review and final corrections

The separate skeptical reviewer reproduced the DST, all-day, override, ordering,
incremental, tampered-shard, preflight, host-time-zone, and full-workload
claims. The review initially found five prototype correctness blockers: a
partial-day all-day omission, equivalent timed override IDs overwriting one
another, unseen overrides creating phantom occurrences, tampered shards being
trusted, and invalid expansion mutating the durable event store before
publication. It also found an incomplete trial README and a stray `+` in CLI
help.

The coordinator fixed all six issues before archival:

- all-day intersection now uses a half-open date range that includes a date
  touched by a partial-day query endpoint;
- timed and all-day overrides are canonicalized before duplicate detection;
- overrides must correspond to a generated recurrence identity;
- every reusable shard carries and verifies byte length plus a SHA-256 digest;
- all desired events are expanded before the Fold event-store transaction;
- the trial README documents the supported schema, policies, commands, and
  explicit durability boundary, and the help text was corrected.

The corrected 10-test suite, strict formatting and Clippy, fresh release
workload, fingerprinted shard reuse, tamper regression, and independent host
time-zone comparisons all passed in the coordinator's reruns. The oracle,
production SQLite integration, standards completeness, and
filesystem or power-loss durability remain explicitly unverified. No BogKit
correctness defect was demonstrated; Fold remains a narrow event-master fit,
not a calendar-semantics or publication solution.

## Decision audit

1. The baseline was modeled before choosing a component. The problem is
   primarily identity, local-time semantics, deterministic ordering, and
   publication—not a generic aggregation problem.
2. `KeyedStream` was selected because its documented upsert retracts the old
   value and its transaction/reopen behavior matches event-master edits.
3. `Table` was selected as the keyed sink because the event master has one value
   per UID. A `Bag` was rejected because multiplicity is not occurrence
   uniqueness. `Aggregate` was rejected because recurrence expansion is not an
   invertible scalar accumulation. ANNy and ESE have no fit.
4. Calendar semantics remain outside Fold because no public BogKit component
   covers the required rules or supplied transition tables.
5. The output is sharded and streamed so the 2-million-candidate fixture stays
   within the measured memory budget while still allowing a one-UID rebuild.
6. No BogKit core, existing example, GitHub, or automation state was changed.

## Unresolved uncertainty

- The local gap/fold policy is explicit, but oracle agreement is unknown until
  the supplied oracle is available.
- SQLite authority and integration with the deployed HTTP/CalDAV layer remain
  untested by this standalone CLI.
- The workload and RSS numbers are from this environment, not the unavailable
  named reference machine.
- Process interruption recovery was tested with the CLI's simulated interruption
  hook. Full OS/power-loss durability of the parent-directory rename metadata
  still needs a platform-specific verification.
- The prototype rejects malformed or unsupported input before publication, but
  the complete production iCalendar parsing surface is intentionally outside
  the prototype boundary.
