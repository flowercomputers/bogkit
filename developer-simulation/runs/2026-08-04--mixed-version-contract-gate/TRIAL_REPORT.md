# Trial notes — mixed-version contract gate

## Skeptical review and coordinator correction

The independent reviewer reproduced the demo, 64-case oracle agreement, and
full 108,000-pair workload, but found a high-severity parser defect before
archival: duplicate raw JSON member names used last-value-wins parsing and could
turn `{"type":"string","type":"integer"}` into a false allow. The
coordinator replaced that path with duplicate-rejecting parsing at every JSON
nesting level and added regressions for root objects, contract records, schemas,
topology, fleet, and candidates. All 8 corrected tests and strict lint pass.

The reviewer also ran the previously missing full-scale reversed/identical-
duplicate input and a two-direction cyclic relationship. Both passed with
byte-identical or structurally bounded results. The final nested-workspace
workload again evaluated 108,000 pairs with 237 issues and no review issues in
0.52 seconds, using 119,144,448 bytes maximum RSS on this host. These are
workload-specific results, not a formal proof, portability guarantee, or
comparison with a runnable existing-system contract gate. Earlier corrected
standalone and reviewer runs completed in 0.24-0.25 seconds with materially
similar memory. The no-fit conclusion remains; no BogKit defect was
demonstrated.

The remainder preserves the blind developer's trail. This reviewed correction
controls where an initial claim differs.

Date: 2026-08-04
Persona: deployment-platform developer; production Go, intermediate Rust
Checkout: sanitized detached current-main checkout at `/private/tmp/bogkit-sim-2026-08-04-a`
Trial: `/private/tmp/bogkit-sim-2026-08-04-a/trial`

## Finishing criteria used

The trial was considered complete only if it had a runnable four-input Rust CLI,
deterministic mixed-version evaluation and witnesses, strict review behavior for
unsupported/malformed input, a generator, at least 60 fixed semantic cases, an
independent checker, shuffle/duplicate and incremental/full checks, a verified
demo, formatting/lint/test passes, and an actually measured stated workload.

## Exact discovery order and first-use friction

No prior report, simulation directory, other temporary checkout, or other
developer's work was inspected.

1. `README.md`, via `pwd && sed -n '1,240p' README.md`.
   - Learned that the recommended start is `./scripts/new-project.sh`, and that
     the four public examples are starter, timeseries, chat, and search.
2. `examples/starter/src/main.rs` and its file list, via
   `rg --files examples/starter | sort` and `sed -n '1,240p' ...`.
3. `examples/timeseries/src/main.rs`, using the same bounded file-list/read
   pattern.
4. `examples/chat/src/main.rs`, using the same bounded pattern.
5. `examples/search/src/main.rs`, using the same bounded pattern.
6. Runnable baseline attempt: `cargo run -p starter --offline`.
   - Failed after 6.2 seconds while the `ese` build script attempted to download
     `model.safetensors`; DNS was unavailable. This happened even though starter
     does not use ESE in its source.
7. Baseline retry with network: `cargo run -p starter`.
   - Passed. Build finished in 6.88 seconds and the demo printed three entries,
     then two after retracting `peat`.
8. Root `Cargo.toml`, `examples/starter/Cargo.toml`, and a bounded Fold file list.
   - Confirmed starter directly declares `anny`, `ese`, and `fold`, and the
     workspace uses `examples/*` members.
9. `fold/src/lib.rs`, `fold/src/stream/mod.rs`, and
   `fold/src/pipeline/mod.rs`.
10. `scripts/new-project.sh`, `fold/src/stream/unkeyed.rs`, and
    `fold/src/pipeline/terminal/table.rs`.
    - Confirmed the generator always adds all three local dependencies and Fold
      provides persistent delta streams/materialized sinks, not schema-language
      parsing or inclusion.

The public README was useful for reaching a runnable example quickly. The main
friction was that the advertised smallest example and generated-project template
pull in the heavyweight ESE build artifact regardless of use.

## Fit decision

Decision: **do not use a BogKit component in this prototype**.

The required work is a bounded, pure, order-independent language-inclusion pass
over immutable manifests. Fold's value is durable incremental materialization of
runtime deltas. Using it here would add a persistent store, transaction lifecycle,
serialization constraints, and the observed workspace/model setup cost without
removing the hard parts: strict parsing, recursive inclusion, minimal witnesses,
and exact diagnostics. Candidate incrementality is cheaper and clearer as a
sorted in-memory pair-result map that selectively reevaluates changed keys.

This is a poor product fit, not evidence of a Fold correctness defect. No Anny or
ESE capability relates to the problem.

## Prototype delivered

- Standalone Rust 2024 crate with only `serde` and `serde_json`.
- CLI: `contract-gate <contracts.json> <topology.json> <fleet.json> <candidate.json>`.
- Supported schema subset: required/optional object properties, strings, finite
  string enums, bounded/unbounded signed integers, defaults, arrays, and
  open/closed objects.
- Every unique topology relationship expands to the full cross product of the
  producer's and consumer's permitted fleet versions.
- Candidate entries replace matching immutable base keys. The implementation
  reuses unaffected base pair results and reevaluates every touched pair.
- Unsupported keywords/types and malformed structures return `review-required`.
  Contract diagnostics use a stable semantic identity pointer such as
  `/contracts/service=a/topic=t/version=1/schema/pattern`; malformed JSON uses
  parser line and column.
- Conflicting contract-array duplicates and all duplicate JSON members require
  review. Identical contracts, duplicate
  relationships, and duplicate fleet versions are normalized.
- Block issues are sorted by topic/service/version pair. Per-pair witness choice
  is deterministic: fewest JSON nodes, shortest canonical encoding, canonical
  byte order, then rule/path.
- Generator modes:
  - `demo`: six contracts, one relationship, nine version pairs, one seeded
    required-field change.
  - `workload`: exactly 300 services, 120 live topics, 1,800 immutable contracts,
    12,000 unique relationships, three versions per service, and 25 candidates.
- Fixed 64-case fixture catalog plus a structurally independent Python oracle.

Default semantics are explicit and deliberately narrow: a valid default permits
a receiver to materialize an absent required property. Different default values
are not treated as breakage because arbitrary application semantics are a stated
non-goal.

## Commands and observed results

Build and checks:

- `cargo check --all-targets --offline`
  - Initial compile exposed one Rust borrow error in the prototype; fixed.
  - Final run passed.
- `cargo fmt --all -- --check`
  - Passed after formatting.
- `cargo clippy --all-targets --offline -- -D warnings`
  - Initial run found five library style warnings, then one generator loop
    warning. All were fixed; final run passed with warnings denied.
- `cargo test --offline`
  - Final reviewed result: 8 tests passed, 0 failed.
  - Includes 64 semantic fixture cases, exact diagnostic locations,
    producer-accepted/consumer-rejected witness validation, malformed default,
    unsupported construct behavior, shuffle/identical-duplicate stability, and
    incremental/full equality.
- `python3 oracle.py fixtures/semantic_cases.json`
  - `independent oracle verified 64 semantic cases`.

Demo:

- `cargo run --offline --bin generate -- demo generated/demo`
  - Generated all four inputs.
- `cargo run --offline --bin contract-gate -- generated/demo/contracts.json generated/demo/topology.json generated/demo/fleet.json generated/demo/candidate.json`
  - Exit 1 (`block`), evaluated all 9 pairs, returned exactly 3 issues: producer
    versions 1, 2, and 3 against consumer version 3. Each rule was
    `required-field-missing` at `/region`, witness `{"id":0}`.

Generated workload validation:

- Generator assertions and `jq` checks confirmed 300 services, 120 contract and
  topology topics, 1,800 contracts, 12,000 relationships, every fleet entry with
  3 versions, and 25 candidates.
- Release build: `cargo build --release --offline --bins` passed.
- Timed run (outside the restricted process-accounting sandbox):
  `/usr/bin/time -l target/release/contract-gate ...`
  - Exit 1 (`block`), as seeded.
  - 0.24 seconds real, 0.23 seconds user.
  - 118,784,000 bytes maximum resident set size = about 113.3 MiB.
  - 99,697,048 bytes reported as peak memory footprint.
  - Both goals passed: under 5 seconds and under 128 MiB in one process.
  - 108,000 pairs evaluated (`12,000 × 3 × 3`).
  - 237 issues, all the expected incoming producer-version combinations against
    the one narrowed consumer contract; 0 review issues.
- A repeat run produced the identical SHA-256
  `d5cbec30584c237b56d03a507292ffb4772234c34be1f47a5ebbb3436e4fd5d7`.
- The 25-candidate incremental result was normalized and diffed against a full
  evaluation of the merged 1,800-contract manifest; `diff` exited 0.

## Categorized findings

### Documentation gap — moderate severity, high confidence

The README calls starter the smallest Fold database and recommends the project
script, but neither explains that the generated manifest unconditionally adds
ANNy and ESE or that ESE's build downloads a model. Reproduction:
`cargo run -p starter --offline` in a clean target directory. Smallest plausible
improvement: make new-project dependencies opt-in and remove unused Anny/ESE
dependencies from starter; document the optional ESE artifact.

### Performance/setup problem — moderate severity, high confidence

The smallest baseline compiled ESE and required its model despite no ESE use in
starter source. This is baseline packaging/build behavior, not a contract-gate
defect and not evidence about Fold runtime performance. Smallest improvement:
remove the unused direct dependencies from the starter manifest.

### Poor product fit — informational severity, high confidence

Fold's persistent delta/materialized-view abstraction does not supply schema
language inclusion and adds state that this read-only bounded gate does not need.
Reproduction path: compare the documented `Stream`/`Push`/terminal interface to
the four immutable inputs and deterministic batch output. Smallest improvement is
documentation showing when a plain in-memory pass is preferable; no new Fold API
is justified by this trial.

### Missing capability — informational severity, high confidence

No inspected BogKit component parses this schema subset, proves producer-language
inclusion, or constructs counterexamples. This is outside the currently described
BogKit scope, so it is not classified as a defect. The smallest plausible product
change would be a separate contract-analysis crate only if this use case becomes
intentional product scope.

### API friction — low severity, medium confidence

Public examples note that closure-containing pipeline types are hard to name and
therefore use macros for snapshot helpers. This did not block the baseline and was
not exercised in the prototype. A named/boxed pipeline-reader ergonomics example
could help, but there is insufficient evidence here to recommend an API change.

### Prototype correctness defects found and fixed

- One internal borrow conflict prevented the first compile.
- One fixture initially expected the typed-open-object rule while its consumer was
  closed; the smaller valid witness correctly exercised the closed-object rule.
  The fixture was corrected to isolate the intended open-object case.
- Clippy findings were mechanical and fixed. No known defect remained after the
  final verification set.

## Consequential-choice decision audit

1. **Use Fold or stay standalone?** Chose standalone after running the baseline
   and reading Fold's public stream/pipeline interfaces. Consequence: minimal
   dependencies and no persistence; the trial does not evaluate whether Fold
   could cache results across separate CI jobs.
2. **What does `default` mean?** Chose receiver materialization only. Consequence:
   adding a required field with a valid receiver default is allowed, while
   removing that protection can block. Default-value semantic changes remain out
   of scope.
3. **Fail open or require review?** Chose review for every unsupported keyword,
   unsupported type, invalid bound/default, missing active contract, conflicting
   duplicate, or malformed input. No unsupported construct can produce allow.
4. **One issue or all issues?** Chose one smallest deterministic witness per
   breaking version pair, while returning every breaking pair. Consequence: output
   identifies rollout combinations without multiplying redundant rules per pair.
5. **Incremental representation?** Chose an ordered pair-result cache and touched
   key reevaluation. Consequence: simple exact equivalence with full evaluation;
   no durable cross-run cache.
6. **Output order?** Chose sorted semantic identities rather than source offsets.
   Consequence: shuffle/duplicate stability while diagnostics retain stable exact
   contract/schema pointers.

## Unresolved uncertainty and limits

- The 64 fixed cases and independent oracle are broad, but not an exhaustive
  formal proof of the recursive schema inclusion implementation.
- Minimality is exact under the documented ranking for the supported mismatch
  constructors; no separate exhaustive witness enumerator was built for arbitrary
  deeply nested schemas.
- Shuffle/duplicate stability was executed in the test fixture; skeptical review
  also reversed and identically duplicated the full input and received
  byte-identical output.
- Cyclic relationships are structurally bounded because relationships are expanded
  independently with no graph traversal. Skeptical review added the reverse
  relationship and observed 18 evaluated pairs with the same three issues; this
  is not evidence of graph-level cyclic semantics.
- Peak RSS passed by about 14.7 MiB. Only the recorded full-size process-accounting
  run is claimed; no multi-run memory distribution was measured.
- JSON numbers outside signed 64-bit integer bounds require review by design.

## Separation of claims

The ESE download and unused-dependency friction belongs to the documented BogKit
starter/new-project baseline. Compile issues and fixture correction belonged only
to this prototype and were fixed. The measured timing/memory and semantic results
apply only to the generated workload and this machine/run; they are not general
BogKit performance claims.
