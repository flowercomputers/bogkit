# Blind trial report: provenance revocation impact

## Outcome

**Explicit no-fit for BogKit's current public components.** Fold's atomic writes,
durable local state, retractions, and consistent read snapshots are useful
building blocks, but its documented operators do not provide a join, recursive
fixed point, or dynamic reachability primitive. Those are the center of this
problem, not an incidental feature. A correct solution would require a custom
graph engine plus deterministic witness and restart protocols, while also
creating a second durable authority beside PostgreSQL.

The runnable reproducer deliberately stops at the smallest useful boundary. It
uses Fold to persist input facts, implements an intentionally incomplete one-hop
negative control in ordinary Rust, and compares it with a slow deterministic
graph reference. The negative control is not a composition of Fold operators and
its defects are not BogKit defects. On the demonstration corpus, it disagrees
with the reference on 2 of 3 release decisions: it approves both a transitively
revoked release and a release containing a reachable cycle.

This is a product-fit conclusion, not a claim that arbitrary Rust code cannot be
written behind Fold's `Push` trait. It can. The problem is that doing so would be
implementing the missing product.

## Scope and finishing criteria

I worked only in this assigned checkout and wrote only under `trial-output/`. I
did not inspect prior simulations, reports, another checkout, GitHub, automation
state, or internet resources. I did not commit or push.

I considered the trial complete when all of the following were true:

1. The existing PostgreSQL baseline had been evaluated before selecting any
   BogKit component.
2. The public README, examples, and relevant public Fold API were inspected.
3. A minimal JSONL reproducer persisted state locally and exposed the key fit
   failure against a deterministic reference.
4. Duplicate edges, missing manifests, transitive revocation, cycles, fact-order
   independence, and two transaction crash boundaries had evidence.
5. Formatting, compilation, strict linting, tests, and the demonstration passed.
6. Findings, limitations, commands, observed results, and the decision audit
   were recorded without claiming unmeasured scale results.

## Baseline evaluation before BogKit selection

The PostgreSQL baseline already has two important properties: it is the named
authority, and recursive queries can calculate the exact reachable set from a
transactionally consistent database snapshot. Its problem is operational:
nightly full recomputation gives unacceptable revocation latency, while a
separate cached flag can represent a different generation than concurrently
uploaded manifests.

The real PostgreSQL implementation was evaluated conceptually, not reproduced.
The runnable slow reference instead loads Fold facts into ordinary Rust
collections and serves only as a bounded correctness oracle. The smallest
direction that preserves the baseline's strengths is an
incremental reverse-dependency index and a versioned decision generation that
is published atomically in PostgreSQL. That could bound recomputation to nodes
reachable from changed roots, keep ingestion and decisions tied to database
versions, and publish explanations with the same generation. This trial did not
build that system because the requested question was whether BogKit components
fit the prototype boundary.

Moving the derived decision set into Fold's embedded store would not, by itself,
fix the authority boundary. It would add a PostgreSQL-to-local-store checkpoint
and reconciliation problem. The prototype would need an explicit source offset,
idempotent replay, generation publication, and recovery protocol before its
decisions could safely gate deployment.

## Public surface evaluated

The root README presents Fold as an eager incremental programming framework and
points new users at four examples. I read all four public examples:

- `starter`: atomic insert/retract into a persistent count and bag.
- `timeseries`: keying, filtering, and invertible per-key aggregation.
- `chat`: one thread owns the Fold stream and publishes snapshots after each
  committed write.
- `search`: keyed upsert/retract fan-out into three independent indexes.

These show a clear, useful model for one input delta flowing through a static
pipeline. The public Fold module documents these transformation operators:
`Map`, `Filter`, `FilterMap`, `FlatMap`, `Distinct`, `Aggregate`, `TopK`, and
`Retain`, plus keying/scoring and terminal views. Tuples broadcast a delta into
independent branches. There is no public join, recursion, feedback edge,
fixed-point iteration, or graph reachability operator.

The public `Push` trait does permit a custom stateful operator with low-level
keyspaces and transaction hooks. For this workload, such an operator would need
to implement all of the following itself:

- forward and reverse adjacency with duplicate-edge set semantics;
- incremental transitive invalidation and revalidation under deletions/updates;
- well-founded behavior for malformed cycles and incomplete manifests;
- a specified deterministic witness policy under every ingestion order;
- source offsets, idempotent replay, versioned publication, and crash recovery;
- storage compaction and memory controls at five million edges.

That is the core application, so the custom-node escape hatch does not change
the no-fit decision.

## What the reproducer contains

- `Cargo.toml`: standalone Rust crate with a local path dependency on Fold.
- `src/main.rs`: one executable with two commands:
  - `generate` writes a deterministic JSONL corpus.
  - `run <state-directory> <candidate|reference>` consumes JSONL, stores facts
    in a Fold `Bag`, and emits JSONL decisions for query commands.
- `PROVENANCE_CRASH=before_commit:N` and
  `PROVENANCE_CRASH=after_commit:N`: deterministic crash injection around the
  Nth persistent fact.
- Five unit fixtures: complete DAG, transitive revocation, transitive missing
  manifest, reachable cycle, and order/duplicate-edge determinism.

Supported JSONL operations are `artifact`, `edge`, `release`, `revoke`, and
`query`. This intentionally does not claim full manifest update, attestation,
unrevocation, or multi-process ingestion support. It is a failure reproducer,
not a deployment-gate implementation.

The `candidate` engine is an intentionally incomplete negative control that
checks the release artifact and one dependency hop. It is not an implemented
Fold pipeline and therefore cannot establish that a concrete Fold composition is
incorrect. The `reference` engine scans the persisted facts,
deduplicates edges with ordered sets, and performs deterministic depth-first
reachability. Unknown manifests and cycles block. Sorted traversal makes its
first witness path independent of fact ingestion order on the tested fixtures;
global minimality is not established.

## Ordered discovery and friction trail

1. Read the root README and enumerated `examples/`. This established the intended
   onboarding path and the advertised component set.
2. Read `starter`, `timeseries`, `chat`, and `search`. Atomic snapshots and
   retractions were immediately promising; no example correlated two changing
   relations or fed results back to a fixed point.
3. Searched the public Fold API for join/recursion/iteration/feedback/cycle and
   inspected the operator and stream documentation. The operator list confirmed
   the missing primitive. The transaction API confirmed that all terminal
   updates in one `wtx` are atomic and restart from the last commit.
4. Considered a custom `Push` node. It is technically possible, but it exposes
   storage/transaction plumbing rather than a graph abstraction. Implementing it
   would consume the whole prototype budget and still leave PostgreSQL
   reconciliation outside the component.
5. Built the minimal candidate/reference executable under `trial-output/`.
6. The first validation command ran `cargo fmt --check`, which correctly showed
   formatting differences. Because this new standalone crate had no lock file,
   the subsequent non-offline Cargo commands attempted to update the crates.io
   index and failed DNS resolution. No internet content was accessed. Running
   `cargo fmt` and rerunning Cargo with `--offline` resolved dependencies from the
   existing local cache and created the lock file.
7. The first demo invocation looked for a normal binary after only `cargo check`
   and `cargo test`; only the test harness existed. `cargo build --offline
   --locked` produced the executable, after which the demo passed. This was local
   build-command friction, not a BogKit defect.
8. Ran the deterministic corpus through candidate and reference stores, then
   injected crashes before and after a Fold commit and reopened each store.
9. Attempted `/usr/bin/time -l` for peak resident memory. Wall time was reported,
   but the sandbox denied the required `sysctl kern.clockrate`, so peak memory was
   not available. I do not claim a memory result.

## Exact validation commands and observed results

Final commands ran from the repository root after this crate joined the nested
`developer-simulation` workspace and resolved its shared locked dependencies.
Generated state and build output stayed under `/private/tmp`.

### Formatting, compilation, lint, and tests

```console
cargo fmt --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer -- --check
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-final-target \
  cargo test --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --offline --locked
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-final-target \
  cargo clippy --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --all-targets --offline --locked -- -D warnings
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-final-target \
  cargo build --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --release --offline --locked
```

Formatting, compilation, strict Clippy, and all 5 tests passed.

### Deterministic demonstration

```console
PROV_ROOT="$(mktemp -d /private/tmp/provenance-final.XXXXXX)"
PROV_BIN=/private/tmp/bogkit-sim-final-target/release/provenance-revocation-reproducer
"$PROV_BIN" generate | "$PROV_BIN" run "$PROV_ROOT/candidate" candidate
"$PROV_BIN" generate | "$PROV_BIN" run "$PROV_ROOT/reference" reference
du -sk "$PROV_ROOT/candidate" "$PROV_ROOT/reference"
```

Corpus counts: 14 persistent facts and 3 queries. The facts include 5 declared
artifacts, 5 edges (one exact duplicate), 3 releases, and 1 revocation.

Candidate output:

```jsonl
{"engine":"one_hop_negative_control","release":"transitive","decision":"approved","reason":"complete","path":[]}
{"engine":"one_hop_negative_control","release":"cyclic","decision":"approved","reason":"complete","path":[]}
{"engine":"one_hop_negative_control","release":"unknown","decision":"blocked","reason":"missing_manifest","path":["missing-root"]}
```

Reference output:

```jsonl
{"engine":"slow_reference","release":"transitive","decision":"blocked","reason":"revoked","path":["app","middle","revoked-base"]}
{"engine":"slow_reference","release":"cyclic","decision":"blocked","reason":"invalid_cycle","path":["cycle-a","cycle-b","cycle-a"]}
{"engine":"slow_reference","release":"unknown","decision":"blocked","reason":"missing_manifest","path":["missing-root"]}
```

Observed decision match: 1 of 3. Observed mismatch: 2 of 3. Each tiny Fold state
directory occupied 48 KiB. That size is dominated by fixed storage overhead and
is not evidence for the 1.3x scale limit.

### Crash boundaries

The reviewer required process-abort, rather than only Rust-unwind, evidence. A
separate release binary was used for both boundaries:

```console
RUSTFLAGS='-C panic=abort' \
  CARGO_TARGET_DIR=/private/tmp/bogkit-sim-abort-target \
  cargo build --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --release --offline --locked
ABORT_BIN=/private/tmp/bogkit-sim-abort-target/release/provenance-revocation-reproducer
CRASH_ROOT="$(mktemp -d /private/tmp/provenance-abort.XXXXXX)"
```

Before-commit injection:

```console
print -r -- '{"op":"artifact","id":"app"}' \
  | PROVENANCE_CRASH=before_commit:1 "$ABORT_BIN" run "$CRASH_ROOT/before" reference
```

Observed exit 134 from the injected abort. After reopening and adding only the
release, the query returned blocked/missing `app`, proving the interrupted fact
was rolled back:

```json
{"engine":"slow_reference","release":"prod","decision":"blocked","reason":"missing_manifest","path":["app"]}
```

After-commit injection:

```console
print -r -- '{"op":"artifact","id":"app"}' \
  | PROVENANCE_CRASH=after_commit:1 "$ABORT_BIN" run "$CRASH_ROOT/after" reference
```

Observed exit 134 from the injected abort. After reopening and adding the
release, the query returned approved, proving the committed artifact survived:

```json
{"engine":"slow_reference","release":"prod","decision":"approved","reason":"complete","path":[]}
```

This validates Fold's local transaction boundary for the two tested process
aborts. It does not cover OS failure, power loss, or every phase of a versioned
derived-decision publication protocol, because the reproducer deliberately does
not invent that absent protocol.

## Acceptance evidence and exact limitations

| Acceptance item | Evidence | Result |
| --- | --- | --- |
| Exact impacted-release match | Three-query candidate/reference demo | **Fail: 2/3 mismatches** |
| 500k artifacts, 5m edges, 100 revocations under 60s | Not run after the correctness no-fit was established | **Unproven** |
| Admission below 250 ms during 100 updates/s | No concurrent ingestion harness; reference rescans all facts per query | **Unproven and unsuitable design** |
| Deterministic explanation path | Unit test reverses fact order and includes a duplicate edge | **Reference passes; candidate cannot explain transitive failures** |
| Unknown provenance never approved | Missing-root demo and transitive-missing unit fixture | **Reference passes tested cases** |
| Safe cycles | Reachable two-node cycle unit and demo | **Reference blocks; candidate incorrectly approves** |
| Crash restart equals uninterrupted publication | Before/after one Fold transaction tested | **Local atomicity passes; full publication protocol absent** |
| Peak memory below 512 MiB | Measurement unavailable; reference materializes the whole graph | **Unproven** |
| Auxiliary state below 1.3x input | Tiny store is 48 KiB, not scale-representative | **Unproven** |

The slow reference is recursive and materializes ordered strings and adjacency
sets in memory on every query. It exists only as a correctness oracle on bounded
fixtures. It is not claimed to meet the performance or memory acceptance
criteria. A very deep malformed chain could also exhaust its call stack; that is
another reason it is not a production candidate.

## Categorized findings

### F-01 — One-hop negative control approves unsafe releases

- Category: **prototype correctness defect**
- Severity: **blocker**
- Confidence: **high**
- Reproduction: run the generated corpus through both engines. `transitive` and
  `cyclic` are approved by the negative control and blocked by the reference.
- Smallest improvement: do not use one-hop logic as the gate. Keep computation
  in the authoritative database or choose a proven recursive graph system.

This is a deliberately exposed defect in the negative-control prototype, not a
defect in Fold or evidence that an implemented Fold composition failed.

### F-02 — No join or recursive fixed-point component

- Category: **missing capability**
- Severity: **blocker**
- Confidence: **high**
- Reproduction: inspect the public operator list in `fold/src/pipeline/mod.rs`
  and the public examples; search the public source for join, recursion,
  feedback, and fixed-point APIs.
- Smallest improvement: document that joins and recursive reachability are not
  supplied. A future durable recursive operator is only a one-trial observation,
  not a threshold-qualified candidate.

### F-03 — Embedded derived authority conflicts with PostgreSQL authority

- Category: **poor product fit**
- Severity: **blocker**
- Confidence: **high**
- Reproduction: compare the stated constraint that PostgreSQL remains
  authoritative with Fold's local embedded store and process-owned stream.
- Smallest improvement: offer a PostgreSQL-backed state/transaction adapter or
  a documented exactly-once source-offset and generation-publication protocol.
  Without that, keep computation and publication in PostgreSQL.

### F-04 — Correct fallback repeats full-scan baseline behavior

- Category: **performance problem**
- Severity: **blocker**
- Confidence: **high for asymptotic behavior; low for exact scale timing**
- Reproduction: every reference query iterates the entire persisted fact bag and
  rebuilds artifact, edge, release, and revocation collections before traversal.
- Smallest improvement: maintain reverse reachability and a specified stable witness
  incrementally by changed generation instead of scanning all facts per query.

No 500k/5m benchmark was run, so this report makes no fabricated claim about
seconds or peak memory.

### F-05 — Custom operator path exposes low-level implementation burden

- Category: **API friction**
- Severity: **major**
- Confidence: **high**
- Reproduction: inspect `Push`, `PipelineInitCtx`, and `WriteTx`. A custom node
  must manage named keyspaces, serialization, buffered deltas, repeated commit
  calls, abort cleanup, and downstream readers. Public examples also use macros
  when closure-containing pipeline types are difficult to name in helpers.
- Smallest improvement: ship supported join/recursive operators and ergonomic
  typed builders/readers rather than requiring application authors to construct
  storage engines at the `Push` layer.

### F-06 — Recovery and evolution guidance is insufficient for a gate

- Category: **documentation gap**
- Severity: **major**
- Confidence: **high**
- Reproduction: the public docs describe atomic `wtx`, snapshots, and
  checkpointing, but the inspected public surface does not explain source offset
  replay, schema/pipeline evolution, keyspace migration, crash points across
  generations, or PostgreSQL reconciliation.
- Smallest improvement: document and test an end-to-end versioned materialization
  protocol, including compatibility checks on reopen and crash matrices.

## Decision audit

1. **Keep nightly PostgreSQL recomputation unchanged:** rejected because it does
   not meet revocation latency and allows cached-generation disagreement.
2. **Compose only documented Fold operators:** rejected by the runnable
   correctness mismatch. The graph relation cannot be correlated to revocation
   roots through arbitrary depth.
3. **Write a custom recursive `Push` node:** technically possible, rejected as a
   BogKit selection because it means implementing the dynamic reachability,
   witness, cycle, storage, and recovery engine from scratch.
4. **Use Fold only as a local approved/blocked cache:** rejected because it leaves
   computation in PostgreSQL and adds a second publication/reconciliation
   boundary without solving the core problem.
5. **Use ANNy or ESE:** rejected as irrelevant; approximate search and embeddings
   cannot safely decide exact provenance reachability.
6. **Recommended direction:** preserve PostgreSQL authority and prototype an
   incremental reverse-dependency/affected-generation design with atomic
   generation publication and specified deterministic explanations. Revisit BogKit only after
   it has a supported recursive dataflow/join facility and an authority-safe
   PostgreSQL integration story.

## Final uncertainty statement

The trial proves that its deliberately incomplete one-hop negative control is
unsafe and identifies no supported public join or recursive-reachability
abstraction. It does not prove that a concrete Fold composition is incorrect or
that a bespoke Rust graph engine built as a custom Fold node could never meet the
numeric targets. It also does not measure the 500k/5m workload, concurrent update
latency, peak memory, or durable-state ratio. Those tests would only be justified
after selecting or building a correct incremental graph component.
