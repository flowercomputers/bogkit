# Evidence

Run date: 2026-07-31 (America/New_York)

## Outcome

The prototype outcome is **BogKit no-fit for the authoritative catalog path**. The smallest credible solution keeps SQLite authoritative and uses ordinary transactional tables and compact facet indexes. Fold's incremental views are promising for optional derived analytics later, but adopting them here would introduce a second durable store without solving schema validation, conditional updates, SQLite-file preservation, or online category revisions.

ESE and ANNy are also no-fit because embeddings, semantic search, and recommendations are explicit non-goals.

## Ordered discovery and friction

1. `sed -n '1,240p' README.md`
   - The top-level description says Fold incrementally maintains fast views and names ESE/ANNy, but gives no catalog, SQLite coexistence, schema evolution, or optimistic-update guidance.
2. Read the four public examples under `examples/`.
   - `starter` demonstrates durable counts/bags and atomic retraction.
   - `timeseries` demonstrates typed, fixed-at-compile-time materialized views.
   - `chat` demonstrates one thread owning Fold and publishing snapshots to Axum clients.
   - `search` demonstrates keyed upsert/retraction plus BM25/HNSW derived indexes.
3. Read Fold's public crate documentation and relevant public operators.
   - Fold persists through Fjall, not the existing SQLite file. Pipelines are concrete Rust types assembled at startup. This is a direct mismatch for SQLite authority and user-defined category schemas activated online.
4. Evaluated the baseline before choosing a component.
   - SQLite already provides atomic compare-and-swap updates, online additive tables/indexes, WAL restart safety, parameterized exact/range queries, and atomic import checkpoints in the same file.
5. Initial dependency check found no cached `rusqlite` package in the sanitized workspace.
   - Smallest improvement made in the prototype: a narrow parameterized wrapper around the system SQLite library in `src/sqlite.rs`. This added implementation friction and is not a recommendation to replace `rusqlite` in production.
6. First compile found two expressions unsupported directly inside `serde_json::json!`.
   - Moved the selected seed values into local variables. Tests then compiled and passed.
7. First test run reported two unused SQLite functions.
   - Removed them; the required lint command then completed with no warnings.
8. Scale verification was run first on 25,000 records, then repeated at the full 250,000-record boundary.
9. Skeptical review found that import jobs were bound only to a record count and
   `INSERT OR IGNORE` allowed conflicting pre-existing IDs to advance the
   checkpoint. Independent reproducers produced mixed payload sizes and a false
   completion. The importer now persists a generator/payload fingerprint,
   rejects changed sources, compares the complete expected product on duplicate
   IDs, and rolls back before checkpoint advancement on conflict.
10. Review narrowed the HTTP, concurrency, schema, interruption, performance,
    and storage claims and removed the unused optional Fold dependency.

## Exact verification and observed results

### Formatting

Command:

```console
cargo fmt --check
```

Observed: exit 0, no output.

### Tests

Command:

```console
CARGO_NET_OFFLINE=true cargo test
```

Observed:

```text
running 9 tests
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The tests cover stable nested error paths, omitted-field patch behavior, stale
versions, v1 readability and v1-to-v2 migration, exact/range agreement with an
independent evaluator, orderly import reopen with every expected ID, rejection
of changed resume sources and conflicting pre-existing rows, POST/GET response
shape within this prototype, and preservation of an unrelated legacy table.

### Lint

Command:

```console
CARGO_NET_OFFLINE=true cargo clippy --all-targets -- -D warnings
```

Observed: exit 0; finished with no warnings.

### Demo

Command:

```console
CARGO_NET_OFFLINE=true cargo run --release -- demo
```

Observed:

```text
legacy SQLite table preserved: yes
v1 readable after v2 activation: spec_version=1
tested migration: spec_version=2, version=2
partial patch preserved connector="usb-c", new version=2
stale conditional patch rejected: true
independent filter evaluator: exact=1, range=1, all matched
Fold decision: no-fit for authoritative catalog path; SQLite retained
```

### Full orderly reopen and resume

Command:

```console
CARGO_NET_OFFLINE=true cargo run --release -- import-check 250000 2048
```

Observed:

```text
simulated interruption checkpoint: 7000/250000
resume completed: true
rows: 250000; distinct ids: 250000; expected: 250000
record payload: 2048 bytes; database: 1139924992 bytes
elapsed: 8.623s
```

The command drops the database connection after the first seven committed
batches, reopens the same SQLite file, validates the source fingerprint, resumes
by durable ordinal, and verifies total and distinct IDs. This is an orderly
reopen after committed batches, not a crash or power-loss test. The unit test
additionally checks the complete expected ID set on a 2,503-row uneven final
batch, changed-source rejection, and duplicate-content conflict rollback.

### Full-population 150-request burst

Command:

```console
CARGO_NET_OFFLINE=true cargo run --release -- burst 250000
```

Observed:

```text
simultaneous burst: 150 requests (120 reads / 30 writes)
seed: 250000 representative 2 KiB records
read p95: 4.037 ms
write p95: 2.314 ms
targets: reads <50 ms; writes <100 ms
```

This is an in-process Axum burst against the full population on the trial host.
One global mutex serializes SQLite access. Two reviewer reruns measured read
p95 at 1.903 and 1.518 ms and write p95 at 1.615 and 1.389 ms. The post-fix run
measured 2.104 and 2.721 ms, making the observed ranges 1.518–4.037 ms and
1.389–2.721 ms. The test excludes network latency, a second process, a
same-version write race, and a memory-limited VM.

### Full-population storage comparison

Command:

```console
CARGO_NET_OFFLINE=true cargo run --release -- storage-check 250000 2048
```

Observed:

```text
storage sample: 250000 records at 2048 payload bytes
baseline SQLite: 1039552512 bytes
indexed SQLite: 1139924992 bytes
ratio: 1.097x; target: <1.5x
```

The baseline contains identical product rows with primary-key, category, and price indexes. The indexed version adds category schemas, import progress, per-product scalar facets, and exact/range indexes.

## Acceptance coverage

| Requirement | Evidence | Status |
|---|---|---|
| CRUD compatibility | Axum POST/GET route smoke test plus direct patch/filter/delete behavior | Only the prototype shape is exercised; the real service contract was unavailable |
| Stable nested validation paths | `specs.battery_wh` test | Demonstrated |
| Safe partial updates | omitted connector/name preserved; stale version gives 409 | Demonstrated |
| Exact/range correctness | SQLite results compared to separate JSON evaluator across three query shapes | Demonstrated on generated data |
| v1 readable after v2 | read old row after activating v2; explicit migration adds `battery_wh` | Demonstrated |
| p95 targets at burst | 1.518–4.037 ms reads, 1.389–2.721 ms writes at 150 in-process requests | Passed under one global mutex on the trial host, not a 512 MiB VM |
| Reopened 250,000 import | orderly close, reopen, source check, resume; total = distinct = 250,000 | Demonstrated at full count with 2 KiB payloads; no crash injection |
| Storage under 1.5x | 1.097x against a synthetic SQLite baseline | Demonstrated at full count with 2 KiB payloads and three categories |
| Existing SQLite preserved | unrelated table survives schema initialization | Demonstrated |
| Category revision | active schema flip is transactional while v1 stays readable | Demonstrated only for hardcoded laptop v1/v2 |

## Categorized findings

### 1. Storage integration — high severity, high confidence

- Reproduction: top-level and Fold crate documentation identify Fjall as Fold's durable store; public examples open a Fold-owned database path.
- Finding: Fold is not an extension of an existing SQLite transaction. Making it authoritative would break the preserve-SQLite constraint; making it derived introduces untested dual-write/rebuild responsibilities. Sidecar storage cost was not measured.
- Smallest improvement: document this boundary prominently in the top-level README, including a supported SQLite-derived-view synchronization pattern if one exists.

### 2. Dynamic category schemas — high severity, high confidence

- Reproduction: examples build typed pipelines from Rust closures and concrete record types at startup.
- Finding: that model is strong for known static records but does not directly validate 30 user-revised category schemas online with stable JSON paths.
- Smallest improvement: add an explicit no-fit example or guide for runtime-defined schemas, and identify the intended external validation layer.

### 3. Conditional updates — high severity, high confidence

- Reproduction: keyed upsert retracts the prior value, but the examples expose no expected-version condition.
- Finding: safe concurrent editing still needs a compare-and-swap check in the authoritative transaction.
- Smallest improvement: document an expected-version keyed update recipe or add a conditional-upsert result type.

### 4. Query fit — medium severity, high confidence

- Reproduction: Ranked provides range traversal for one compile-time score; filtering operators materialize predicates chosen at pipeline construction.
- Finding: 30 evolving categories create many runtime paths and combinations; a compact SQLite facet table is simpler for the required exact/range subset.
- Smallest improvement: provide guidance on dynamic facet indexing and storage amplification, including a benchmark against a relational baseline.

### 5. Onboarding — medium severity, high confidence

- Reproduction: the top-level README describes Fold in one paragraph and routes users to internal docs; examples contain the clearest operational explanations.
- Finding: a new developer must infer transaction ownership, storage format, schema stability expectations, and migration boundaries from source and examples.
- Smallest improvement: add a short “fits / does not fit” matrix covering source-of-truth storage, runtime schemas, query types, and update concurrency.

### 6. Local SQLite wrapper — medium severity, high confidence

- Reproduction: `CARGO_NET_OFFLINE=true cargo info rusqlite@0.37.0` reported that the package was unavailable in the sanitized registry cache.
- Finding: the trial needed a small direct wrapper to stay runnable offline. It is appropriately narrow but less mature than `rusqlite`.
- Smallest improvement: productionize with a maintained SQLite crate, pooled read connections, migration tooling, and error-code mapping.

### 7. Import identity defect, fixed — high severity, high confidence

- Reproduction: the reviewer resumed one job with a changed payload size and
  inserted a conflicting `bulk-000000`; the original code accepted both and
  advanced the checkpoint.
- Finding: count-only job identity and unchecked `INSERT OR IGNORE` could report
  a mixed or conflicting import as complete. This was a prototype defect, not a
  BogKit defect.
- Smallest improvement: persist a source fingerprint and compare complete
  duplicate content before advancing a checkpoint. Both regressions now pass.

## Decision audit

| Option | Decision | Reason |
|---|---|---|
| Keep hand-written optional columns | Rejected | Repeats category conditionals and migrations |
| Unvalidated JSON only | Rejected | Cannot provide reliable validation, filtering, or safe patch semantics |
| Fold as the authority | Rejected, no-fit | Does not preserve the existing SQLite file |
| SQLite authority plus Fold sidecar views | Rejected for this slice | Dual-store recovery is unproven and unnecessary for the tested exact/range filters; sidecar storage was not measured |
| ESE or ANNy | Rejected, no-fit | Search/recommendations are non-goals |
| SQLite products + schema registry + compact facets | Chosen | Meets the compact boundary in one crash-consistent file with measured headroom |

## Unresolved uncertainty

- The real service's exact HTTP response and error shapes were not present. The
  route-level smoke test covers POST/GET only; other operations are exercised
  directly or in the demo.
- The burst ran on the trial host, not under a 512 MiB cgroup or VM; peak resident memory was not measured.
- Records use deterministic 2 KiB descriptions, the low end of 2–20 KiB. Larger mixed-size payload behavior was not measured.
- The compact boundary uses three categories, not all 30; schema rules are
  handwritten rather than runtime-defined.
- The “interruption” is an orderly connection close at a committed checkpoint,
  not a forced process exit or power loss during an SQLite commit. SQLite WAL
  durability itself was not fault-injected.
- One global mutex serializes database access. Same-version races and
  multi-process concurrency were not tested.
- Only scalar nested specification fields are indexed. Arrays or deeper objects would need a declared canonical facet policy.
- The direct SQLite wrapper is intentionally prototype-sized; production should use a maintained binding and operational migration tooling.

## Files for coordinator review

- `README.md` — reproduction, component decision, API examples, and honest scope.
- `EVIDENCE.md` — this trial record.
- `src/main.rs` — catalog, routes, generator, evaluator, import, measurements, and tests.
- `src/sqlite.rs` — narrow system-SQLite wrapper; highest implementation-risk file.
- `Cargo.toml` — nested-workspace dependencies; no BogKit component dependency.

No BogKit core or public example files were modified.
