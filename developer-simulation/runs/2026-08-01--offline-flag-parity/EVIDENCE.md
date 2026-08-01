# Offline Flag Parity Trial Report

Run date: 2026-08-01 EDT
Persona: client-platform SDK developer, production TypeScript experience, Rust beginner
Sanitized checkout: `/private/tmp/bogkit-2026-08-01-trial-a.AF7gv4`
Prototype: `/private/tmp/bogkit-2026-08-01-trial-a.AF7gv4/trial-output/offline-flag-parity`

## Outcome

**Runnable prototype; BogKit no-fit for this bounded workload.**

The direct baseline—validate a complete JSON candidate, then atomically replace
one immutable in-memory snapshot—was smaller and more directly aligned than a
durable incremental database. I therefore used no BogKit crate and did not edit
BogKit core or its examples.

The prototype met the trial's measured checks:

- 250 stored golden decisions matched in Rust and in a dependency-free
  TypeScript-style JavaScript reference evaluator.
- Reversing every JSON object's key order left all 250 decisions and full
  explanations unchanged in both evaluators.
- All 12 malformed fixtures were rejected. A bad reload left the last good
  in-memory snapshot and its decisions unchanged.
- Ten fresh CLI processes produced the one fingerprint `99c04dd07bb96094`.
- Four clean release benchmark processes each ran five 20,000-evaluation
  batches against 5,000 flags and 50,000 rules. Across the 20 batches, p95 was
  1.000–1.333 microseconds; all were below 250 microseconds.
- The highest sampled same-size reload RSS across the developer and
  post-review runs for the generated 6.2 MB, 5,000-flag/50,000-rule workload
  was 63,569,920 bytes, below the
  67,108,864-byte (64 MiB) ceiling. This is a sampled process-level
  measurement for that workload, not a guarantee for every accepted file or a
  proof that no shorter spike occurred.

## Brief

Build a local library and CLI that loads generated JSON flag snapshots,
evaluates ordered rules against flat scalar contexts without network access,
records a rule-level explanation, retains the active configuration after a bad
reload, uses stable percentage assignment across machines and restarts, and
demonstrates correctness and speed with synthetic fixtures.

Target workload: up to 5,000 flags, 50,000 rules, and about 100,000 evaluations
per minute within 64 MiB working memory. Non-goals were authoring, delivery,
analytics, UI, authentication, and commercial-format compatibility.

## Ordered discovery, friction, and debugging trail

1. Read only the public root `README.md`. It presented Fold as a durable,
   transactional incremental-programming framework, ESE as static embeddings,
   ANNy as nearest-neighbor search, and four public examples.
2. Listed public example files, then read them in this order: `chat`, `search`,
   `starter`, `timeseries` (each manifest followed by its `src/main.rs`). The
   examples showed transactional writes, persistent sinks, consistent reads,
   and incremental aggregation/search. None showed configuration admission or
   an immutable hot-path rules evaluator.
3. Before inspecting any BogKit implementation, defined the concrete baseline:
   parse a candidate snapshot completely; reject syntax, duplicate keys,
   unknown fields, invalid limits, and invalid rule semantics; retain the
   current `Arc<Snapshot>` until validation succeeds; evaluate rule arrays in
   order; use a specified stable hash rather than a process-randomized map hash;
   and return the trace of every visited rule.
4. Inspected the root workspace manifest, Fold manifest, and the Fold stream
   transaction/snapshot API. Compared those capabilities with the baseline.
   Fold can atomically update durable materialized views, but it does not remove
   the need to parse and validate the entire candidate before activation. A
   database on the evaluation path would add files, lifecycle work, and memory
   without improving this prototype's decision lookup.
5. Chose no BogKit component. Added an independent Cargo package under
   `trial-output/offline-flag-parity` with its own empty `[workspace]` table, so
   no parent-workspace edit or membership was needed.
6. Implemented the Rust library and CLI. The first ordinary `cargo test` tried
   to update the crates.io index and failed because DNS/network was unavailable.
   Re-running with `--offline` used cached dependencies and succeeded. All
   subsequent Cargo evidence commands used `--offline` where dependency
   resolution was involved.
7. The first fixed-bucket unit test contained my incorrect expected number
   (`7452`). The Rust implementation returned `7307`. I did not accept that
   value only because the implementation produced it: an independent Node
   `BigInt` FNV-1a calculation also returned `7307`, after which the fixed known
   vector was corrected.
8. Strict Clippy initially found two manual modulo expressions. Replaced them
   with the standard `is_multiple_of` form and reran formatting and Clippy.
9. Generated 250 golden contexts, a normal snapshot, a recursively
   reverse-object-key-order snapshot, good/bad reloads, and 12 malformed
   snapshots. Rust verification passed all fixtures.
10. The first full benchmark built, serialized, and loaded the 50,000-rule
    snapshot inside one process. macOS retained temporary generator allocations,
    and sampled RSS reached 146,587,648 bytes, failing the 64 MiB requirement.
    This was treated as a real failure, not reported as a passing evaluator.
11. Changed duplicate-key validation from a retained generic JSON tree to a
    streaming preflight, followed by direct typed deserialization. Separated the
    deterministic large-fixture generator into its own CLI process. The clean
    benchmark process then loaded an existing snapshot and performed a
    same-size reload while the old snapshot remained active.
12. The sandbox blocked `/bin/ps` and made `/usr/bin/time -l` fail its macOS
    system query. The final benchmark was run with read-only access to its own
    process statistics. Four clean runs passed latency and sampled RSS limits.
13. Added and ran a dependency-free TypeScript-style reference evaluator. It
    independently matched all 250 Rust golden decisions, their explanations,
    reordered-object results, and the known bucket vector.
14. Preserved the small 276 KiB fixture corpus in the prototype, reran ten fresh
    process fingerprints, and began the final validation sweep.
15. Skeptical review rejected the broad memory wording: a 48 MiB policy did not
    follow from the measured 6.2 MB workload, and `fs::read` could allocate an
    entire oversized input before enforcing the cap. The coordinator changed
    file loading to read at most 8 MiB plus one byte, added a sparse-file
    regression, and limited the 64 MiB statement to the measured workload.

## Concrete baseline and comparison

The baseline is the shape a small TypeScript SDK would normally use:

1. Parse JSON into a candidate object.
2. Validate the complete candidate without mutating live state.
3. Swap the active immutable reference only on success.
4. Look up a flag by key and walk its ordered rule array.
5. Hash `salt`, flag key, rule id, and a typed user attribute with one published
   algorithm for percentage rules.
6. Return the selected value plus every visited rule's result.

| Concern | Direct immutable baseline | Fold comparison | Decision |
| --- | --- | --- | --- |
| Malformed snapshot admission | Parse and validate before swap | A database transaction starts after parsing; it does not validate JSON by itself | Baseline fits directly |
| Failed reload | Candidate error leaves active `Arc` untouched | Could make storage writes atomic, but still needs an admission layer | Baseline is smaller |
| Offline evaluation | In-memory key lookup and short ordered scan | Durable reads are available but unnecessary on each decision | Baseline is faster/simpler |
| Restart-stable percentage | Explicit local FNV-1a 64 specification | Not a database concern | Baseline owns it |
| Durable derived views | Not needed by the brief | Strong Fold fit | Non-goal |
| Persistent last-known-good after process exit | Snapshot file remains an external responsibility | Fold could help if this becomes a requirement | Unresolved product boundary |

The JavaScript reference is at
`baseline/reference-evaluator.mjs`. It provides concrete evidence that the Rust
rule and hashing semantics can match a TypeScript-style client. It is not a
claim of compatibility with an unspecified production evaluator.

## Prototype contents

- `src/lib.rs`: strict loader, semantic validation, immutable evaluator,
  atomic reload, ordered-rule evaluation, stable bucketing, explanations, and
  active-snapshot memory estimate.
- `src/main.rs`: fixture generation, NDJSON evaluation, demo, verification,
  fresh-process fingerprint, large deterministic snapshot generation, and
  repeated benchmark.
- `baseline/reference-evaluator.mjs`: independent TypeScript-style reference.
- `fixtures/`: 250 contexts and golden cases, normal/reordered snapshots,
  good/bad reloads, and 12 malformed cases.
- `Cargo.lock`: exact cached dependency resolution.

No BogKit path dependency is present because no BogKit component fit the
bounded problem. The package uses cached `serde` and `serde_json` only.

## Exact evidence

Environment:

```console
$ rustc --version
rustc 1.95.0 (59807616e 2026-04-14)
$ cargo --version
cargo 1.95.0 (f2d3ce0bd 2026-03-21)
$ uname -a
Darwin violaceae 25.5.0 Darwin Kernel Version 25.5.0: Tue Jun  9 22:28:34 PDT 2026; root:xnu-12377.121.10~1/RELEASE_ARM64_T6041 arm64
```

All commands below were run from
`/private/tmp/bogkit-2026-08-01-trial-a.AF7gv4` with build output at
`/private/tmp/offline-flag-parity-target`.

Formatting:

```console
$ cargo fmt --manifest-path trial-output/offline-flag-parity/Cargo.toml -- --check
# exit 0, no output
```

Tests:

```console
$ CARGO_TARGET_DIR=/private/tmp/offline-flag-parity-target cargo test --offline --manifest-path trial-output/offline-flag-parity/Cargo.toml
running 4 tests
test tests::bucket_has_a_fixed_known_value ... ok
test tests::duplicate_json_keys_are_rejected ... ok
test tests::failed_reload_keeps_active_snapshot ... ok
test tests::file_reader_stops_at_the_snapshot_limit ... ok
test result: ok. 4 passed; 0 failed
# main tests: 0 passed; doc tests: 0 passed; command exit 0
```

Strict lint:

```console
$ CARGO_TARGET_DIR=/private/tmp/offline-flag-parity-target cargo clippy --offline --manifest-path trial-output/offline-flag-parity/Cargo.toml --all-targets -- -D warnings
Finished `dev` profile ...
# exit 0
```

Fixture generation and Rust verification:

```console
$ /private/tmp/offline-flag-parity-target/release/offline-flag-parity generate trial-output/offline-flag-parity/fixtures
generated snapshot, reordered snapshot, 250 NDJSON contexts/golden cases, reload fixtures, and 12 malformed snapshots in trial-output/offline-flag-parity/fixtures

$ /private/tmp/offline-flag-parity-target/release/offline-flag-parity verify trial-output/offline-flag-parity/fixtures
verified 250 golden cases; object ordering invariant; 12 malformed snapshots rejected; good reload true -> false; bad reload preserved false
```

The `true -> false` line is an intentional good-reload behavior change for the
first context. The subsequent bad reload kept `demo-v2` and the same `false`
decision.

Independent TypeScript-style comparison:

```console
$ node trial-output/offline-flag-parity/baseline/reference-evaluator.mjs trial-output/offline-flag-parity/fixtures
TypeScript-style reference matched 250 golden cases and reordered-object decisions; known bucket=7307
```

NDJSON CLI:

```console
$ /private/tmp/offline-flag-parity-target/release/offline-flag-parity eval trial-output/offline-flag-parity/fixtures/snapshot.json trial-output/offline-flag-parity/fixtures/contexts.ndjson > /private/tmp/offline-flag-parity-eval-output.ndjson
$ wc -l /private/tmp/offline-flag-parity-eval-output.ndjson
250 /private/tmp/offline-flag-parity-eval-output.ndjson
```

Ten fresh-process restart check:

```console
$ for i in 1 2 3 4 5 6 7 8 9 10; do /private/tmp/offline-flag-parity-target/release/offline-flag-parity fingerprint trial-output/offline-flag-parity/fixtures; done > /private/tmp/offline-flag-parity-final-restarts.txt
$ wc -l /private/tmp/offline-flag-parity-final-restarts.txt
10 /private/tmp/offline-flag-parity-final-restarts.txt
$ sort -u /private/tmp/offline-flag-parity-final-restarts.txt
99c04dd07bb96094
$ sort -u /private/tmp/offline-flag-parity-final-restarts.txt | wc -l
1
```

Large deterministic fixture:

```console
$ /private/tmp/offline-flag-parity-target/release/offline-flag-parity generate-benchmark /private/tmp/offline-flag-parity-benchmark-20260801.json
generated 5000-flag/50000-rule benchmark snapshot: 6203976 bytes at /private/tmp/offline-flag-parity-benchmark-20260801.json
```

Each benchmark run loaded that file, held the parsed snapshot active, reloaded
the same full snapshot while sampling process RSS, then timed five separate
20,000-evaluation batches. The checksum was `324795` in all four runs.

```text
Run 1 p95 ns: 1209, 1084, 1042, 1083, 1042
Run 1 median p95: 1083 ns; max p95: 1209 ns
Run 1 sampled peak same-size reload RSS: 55,197,696 bytes
Run 1 current RSS after evaluation: 55,705,600 bytes

Run 2 p95 ns: 1291, 1333, 1292, 1125, 1084
Run 2 median p95: 1291 ns; max p95: 1333 ns
Run 2 sampled peak same-size reload RSS: 55,132,160 bytes
Run 2 current RSS after evaluation: 55,672,832 bytes

Run 3 p95 ns: 1250, 1125, 1084, 1083, 1208
Run 3 median p95: 1125 ns; max p95: 1250 ns
Run 3 sampled peak same-size reload RSS: 55,115,776 bytes
Run 3 current RSS after evaluation: 55,640,064 bytes

Run 4 p95 ns: 1292, 1167, 1209, 1209, 1167
Run 4 median p95: 1209 ns; max p95: 1292 ns
Run 4 sampled peak same-size reload RSS: 55,066,624 bytes
Run 4 current RSS after evaluation: 55,623,680 bytes
```

All 20 original measured p95 batches were more than two orders of magnitude
below the 250-microsecond ceiling. Four post-review runs added 20 more batches:
their maximum p95 was 1,250 ns, while sampled peak reload RSS ranged from
63,471,616 to 63,569,920 bytes. The closest post-review run left 3,538,944
bytes of headroom under 64 MiB, materially narrower than the original process
observations.

## Categorized findings

### Correctness and safety

| Finding | Severity | Confidence | Reproduction | Smallest improvement |
| --- | --- | --- | --- | --- |
| Standard JSON-to-map parsing can silently accept duplicate object keys and let a later value replace an earlier one. | High | High | `fixtures/malformed/06-duplicate-flag-key.json`; `verify` rejects it | Keep the streaming duplicate-key preflight, or use a parser with duplicate rejection built in |
| Candidate parsing must complete before any live reference changes; otherwise semantic validation failures can partially activate. | High | High | `verify` activates `demo-v2`, rejects `bad-reload.json`, then compares config id and decision | Preserve a single replace-on-success method; do not expose partial mutation APIs |
| Rule-array order is semantic, while object-key order is not. Treating both kinds of reordering alike would be a compatibility bug. | High | High for this schema | `snapshot.json` and `snapshot-reordered.json` match all 250 cases; rules remain arrays | State this distinction in the public snapshot contract and version it |
| FNV-1a 64 is stable and cross-language reproducible, but is not collision-resistant or abuse-resistant. | Medium | High | Rust unit vector and independent Node vector both return bucket `7307` | If untrusted users can choose identifiers adversarially, move to a specified keyed or cryptographic hash after a migration plan |

### Performance and memory

| Finding | Severity | Confidence | Reproduction | Smallest improvement |
| --- | --- | --- | --- | --- |
| A retained generic JSON tree plus typed snapshot, combined with in-process fixture generation, exceeded the memory budget (146,587,648-byte sampled RSS). | High | High for the failed implementation | Original full benchmark run | Keep duplicate checking streaming and keep tooling fixture construction out of the evaluator process |
| The fixed implementation passed 64 MiB for the measured 6.2 MB synthetic snapshot, but the closest post-review same-size reload left only about 3.5 MB headroom on this machine. | Medium | Medium-high for the measured workload | Four original and four post-review `bench SNAPSHOT` runs | Keep file reads bounded, add production telemetry, and remeasure each production data shape before relying on the limit |
| Explanation allocation is included in the roughly 1.0–1.3 microsecond p95, so it is not a current latency concern. | Low | High for this synthetic mix | Four clean benchmark runs | Retain structured explanations; optimize only if production profiles show pressure |

### BogKit fit and developer experience

| Finding | Severity | Confidence | Reproduction | Smallest improvement |
| --- | --- | --- | --- | --- |
| Fold's durable incremental views do not address strict JSON admission or deterministic percentage semantics, and a database is unnecessary on this read path. | Informational | High for the bounded brief | Compare public examples and Fold transaction API with the baseline table | Add a short “when not to use Fold” note and a configuration-snapshot example only if this becomes a supported use case |
| An independent package inside the repository can avoid a parent workspace edit with its own `[workspace]` table. | Low | High | Prototype manifest builds directly with `--manifest-path` | Mention this option in prototype/hackathon documentation |
| The README's normal Cargo path attempted an index refresh in a network-disabled environment even though dependencies were cached. | Low | High | First `cargo test` DNS failure; `cargo test --offline` passed | Document `--offline` for offline trials or vendor the tiny dependency set |

## Consequential decision audit

| Decision | Consequence | Evidence considered | Reversibility / guardrail |
| --- | --- | --- | --- |
| Use no BogKit crate | Trial concludes no-fit rather than forcing a component | Public README/examples, Fold stream transaction API, direct baseline | Fully reversible; all work is isolated and no core file changed |
| Make rule arrays ordered | Reordering targeting rules may intentionally change a decision | Existing baseline description says ordered rules | Schema contract and golden cases make the behavior explicit |
| Make object maps order-insensitive | Serializers may reorder fields/flags without changing results | Rust and JS both matched the reverse-key-order fixture | BTreeMap lookup plus 250 parity cases guard this behavior |
| Reject duplicate keys, unknown fields, and invalid semantics | Some previously tolerated snapshots would now fail closed | Risk of silent replacement/partial activation | Errors are explicit; last active config remains available in process |
| Use FNV-1a 64 with typed, null-separated fields | Stable assignment is portable, but not cryptographically strong | Fixed Rust/Node vector and ten restart fingerprints | Algorithm is documented; changing it requires an explicit migration/version |
| Include explanations on every decision | Hot path allocates a short trace | Acceptance requires rule-level explanation; benchmark includes it | Can add a borrowed/compact representation later without changing semantics |
| Keep generated fixtures in the archive | Adds 276 KiB, improves reproducibility | Golden/reorder/malformed acceptance checks | Small, synthetic, regenerable, and contains no private data |

## Skeptical review and coordinator corrections

The reviewer reproduced the format, test, strict-lint, fixture-verification,
reference-evaluator, restart-fingerprint, and release-benchmark paths. The
bounded no-fit decision stood: the direct immutable evaluator met this compact
workload without a BogKit component.

The reviewer rejected two broader claims. First, one host running Rust and Node
does not establish cross-machine portability. Second, the measured 6.2 MB
snapshot did not justify a 48 MiB accepted-input policy under a 64 MiB process
budget. The coordinator changed file loading to read at most 8 MiB plus one
byte, added the sparse oversized-file regression above, and scoped the memory
result to the measured 5,000-flag/50,000-rule snapshot. Persisted last-known-good
state, a platform CI matrix, every accepted data shape, and concurrent reloads
remain unresolved.

The coordinator then ran four fresh corrected release benchmarks. All 20 new
p95 batches remained at or below 1.250 microseconds. Peak sampled reload RSS was
63,471,616–63,569,920 bytes, so the workload still passed the 64 MiB criterion
but with only about 3.5 MB of closest observed headroom.

No BogKit correctness defect was found. Configuration-snapshot guidance remains
a one-trial observation below the dashboard threshold.

## Rejected alternatives

- **Fold `KeyedStream` for flags/rules:** rejected because the candidate still
  requires full JSON validation and the evaluation workload does not need
  incremental materialized views or database persistence.
- **Persist every decision or explanation:** rejected as analytics, explicitly a
  non-goal, and incompatible with the small offline hot path.
- **Use Rust's default map hasher for percentage buckets:** rejected because its
  seed is process-specific and not a cross-language contract.
- **Canonicalize/sort rule arrays:** rejected because targeting rule order is
  meaningful. Only JSON object keys are irrelevant.
- **Accept duplicate keys with “last value wins”:** rejected because malformed
  delivery could silently activate a different configuration.
- **Claim the original 146 MB benchmark was only tooling overhead and ignore
  it:** rejected. The loader was changed and the workload was re-measured in a
  clean process.
- **Add commercial feature-flag schema compatibility:** rejected as a stated
  non-goal; the prototype schema stays intentionally small.

## Unresolved uncertainty

- The TypeScript-style evaluator is an independent reference written for this
  trial, not the unspecified production kiosk implementation. Real production
  parity needs its actual snapshots, operators, coercion rules, and golden
  outputs.
- The malformed corpus covers 12 important syntax/shape/semantic cases, not all
  possible malformed byte strings. Fuzzing the streaming duplicate checker and
  typed deserializer is the next correctness step.
- Ten restart checks ran on one arm64 macOS machine. The fixed algorithm is
  specified in integer operations and independently matched Node on that host,
  but cross-machine wording was rejected until a CI matrix provides evidence.
- RSS was sampled via `ps` during a same-size reload at roughly millisecond
  intervals and checked again after evaluation. A shorter transient peak could
  be missed. The observed result should be read as strong prototype evidence,
  not a formal maximum-memory proof.
- Benchmark contexts are sequential and synthetic. There was no concurrent
  evaluator access, allocator stress, thermal study, or long-duration soak.
- The evaluator preserves last-known-good state for the lifetime of the
  process. Persisting a last-known-good copy across a process restart after an
  external file replacement is not implemented and should be decided as a
  delivery/storage responsibility before production use.
- The 8 MiB file-read cap and 5,000/50,000 semantic caps bound prototype input.
  They are policy choices, not proof that every accepted shape stays below the
  measured process-memory ceiling.

## Archive hygiene

At the pre-report check, `git status --short` showed only `?? trial-output/`.
BogKit core and existing examples remained untouched. The prototype occupied
332 KiB before this report; fixtures occupied 276 KiB. No `target` directory,
database, `.git` directory, file larger than 1 MiB, credential, or private data
was present under `trial-output/offline-flag-parity`. All contexts and user ids
are deterministic synthetic values.
