# Localization catalog compiler simulation report

Date: 2026-08-07

Scope: this isolated checkout only. No prior simulation archive, report,
coverage ledger, other trial, GitHub state, or automation state was inspected
or changed.

## Ordered discovery and friction trail

1. The public root README describes one Cargo workspace and points new
   projects at Fold, ANNy, and ESE. Its examples are database demonstrations,
   not localization tooling.
2. The public examples were inspected in order: starter (persistent count and
   bag), timeseries (incremental aggregates), chat (transactional snapshots),
   and search (BM25/HNSW/embedding search). None contains a catalog format,
   validator, fallback compiler, or runtime-table generator.
3. A public-source symbol search found Fold stream/pipeline sinks, ESE
   embedding APIs, and ANNy search APIs, but no catalog/compiler component.
4. The stated baseline was evaluated before selecting a component. The
   workspace-wide test command failed offline because ESE tried to download
   model.safetensors and DNS was unavailable. This is a baseline build
   friction, not a prototype failure.
5. The dependency-local baseline test, cargo test -p fold --quiet, passed
   18 unit tests and 9 doctests. The existing workspace formatting check also
   reports pre-existing formatting differences in examples/search.
6. Decision: use no BogKit component. Fold's durable incremental views do not
   address batch catalog parsing and whole-input diagnostics; ESE and ANNy are
   unrelated to catalog correctness and add offline/build surface.

## Prototype boundary

The standard-library-only crate in this directory implements:

- line-oriented representative catalogs with text, plural, select, and nested
  locale/message references;
- all-diagnostic validation for duplicate IDs, fallback references and cycles,
  branch shape, and placeholder sets against en-US;
- sorted locale/message/branch emission into a normalized runtime-table file;
- a lookup harness for source catalogs and emitted tables;
- a deterministic stress-fixture generator.

This is not a production ICU/CLDR implementation. It intentionally does not
claim complete plural rules, rich ICU syntax, translation quality, or a
production catalog-format migration.

## Reproduction commands and observed results

Run from the checkout root:

~~~text
cargo test --offline -p catalog-compiler-prototype --quiet
~~~

Result: 4 library tests passed; binary tests had 0 tests. The same tests pass
without --offline.

~~~text
cargo fmt -p catalog-compiler-prototype -- --check
cargo clippy -p catalog-compiler-prototype --all-targets -- -D warnings
cargo build --release -p catalog-compiler-prototype
~~~

Result: all passed.

~~~text
target/release/catalogc compile runs/2026-08-07--localization-catalog-compiler/fixtures/valid runs/2026-08-07--localization-catalog-compiler/evidence-valid.table
target/release/catalogc lookup runs/2026-08-07--localization-catalog-compiler/fixtures/valid de nested
target/release/catalogc lookup-table runs/2026-08-07--localization-catalog-compiler/evidence-valid.table de nested
~~~

Results:

~~~text
compiled runs/2026-08-07--localization-catalog-compiler/fixtures/valid into runs/2026-08-07--localization-catalog-compiler/evidence-valid.table
Shared message for Ada
Shared message for Ada
~~~

The two lookup results are the same, including the nested fallback reference.

~~~text
target/release/catalogc compile runs/2026-08-07--localization-catalog-compiler/fixtures/invalid runs/2026-08-07--localization-catalog-compiler/evidence/invalid.table
~~~

Expected exit status: 1. It reports 6 diagnostics rather than stopping at the
first:

~~~text
error catalog=en-US message=duplicate branch=- source=runs/2026-08-07--localization-catalog-compiler/fixtures/invalid/en-US.cat:17: duplicate message identifier
error catalog=fr message=- branch=- source=runs/2026-08-07--localization-catalog-compiler/fixtures/invalid/fr.cat:2: fallback locale missing-locale does not exist
error catalog=fr message=bad-ref branch=- source=runs/2026-08-07--localization-catalog-compiler/fixtures/invalid/fr.cat:14: fallback reference targets missing locale missing-locale
error catalog=fr message=apples branch=count=other source=runs/2026-08-07--localization-catalog-compiler/fixtures/invalid/fr.cat:5: missing branch
error catalog=fr message=bad-ref branch=- source=runs/2026-08-07--localization-catalog-compiler/fixtures/invalid/fr.cat:13: message identifier is not present in en-US
error catalog=fr message=greeting branch=- source=runs/2026-08-07--localization-catalog-compiler/fixtures/invalid/fr.cat:11: placeholder mismatch: expected {"name"}, found {"username"}
6 diagnostic(s)
~~~

Stress fixture and repeated release runs:

~~~text
target/release/catalogc generate-stress runs/2026-08-07--localization-catalog-compiler/evidence/stress 100000 18
CATALOG_MEMORY_REPORT=1 /usr/bin/time -p target/release/catalogc compile runs/2026-08-07--localization-catalog-compiler/evidence/stress runs/2026-08-07--localization-catalog-compiler/evidence/stress-a.table
CATALOG_MEMORY_REPORT=1 /usr/bin/time -p target/release/catalogc compile runs/2026-08-07--localization-catalog-compiler/evidence/stress runs/2026-08-07--localization-catalog-compiler/evidence/stress-b.table
cmp -s runs/2026-08-07--localization-catalog-compiler/evidence/stress-a.table runs/2026-08-07--localization-catalog-compiler/evidence/stress-b.table
shasum -a 256 runs/2026-08-07--localization-catalog-compiler/evidence/stress-a.table runs/2026-08-07--localization-catalog-compiler/evidence/stress-b.table
~~~

Results: exactly 100,000 records across 18 locales; each compile took 0.05
seconds real time in release mode. Both runs reported
27,010,299 peak live allocated bytes (about 25.8 MiB). Both output hashes were
f4d5206bc4dd975da35221f78153ab4897ba9bdf0f3c65620e4bbdee36d5e31a.
The output is 4,386,105 bytes and 200,037 lines. The allocator figure is
live heap allocation, not full process RSS; OS RSS sampling was unavailable in
this restricted environment because ps and time -l were denied.

## Categorized findings

| Category | Severity | Confidence | Reproduction | Smallest improvement |
| --- | --- | --- | --- | --- |
| Offline baseline build | High | High | cargo test --workspace --quiet attempts the ESE model download and fails DNS | Vendor/cache the model or make ESE fixtures fully offline |
| BogKit component fit | Medium | High | README/examples/source inspection shows no catalog API | Add a purpose-built catalog compiler crate rather than adapting Fold/ESE/ANNy |
| Seeded defect coverage | High | High | Invalid fixture reports missing branch, placeholder mismatch, invalid fallback locale/reference, and duplicate ID | Add production-format parsing and equivalent fixture coverage |
| Complete diagnostics | High | High | Invalid run reports 6 diagnostics with locale, message, branch, file, and line | Preserve source spans through the production parser |
| Valid lookup preservation | High | High for this fixture | Source lookup and emitted-table lookup both return Shared message for Ada | Differential-test against the real runtime library on production catalogs |
| Reproducibility | High | High for repeated runs; medium cross-machine | Two release outputs have the same SHA-256; BTreeMap ordering is explicit | Run the same corpus on at least two clean machines/toolchains |
| Resource budget | High | Medium | Release run reports 27,010,299 peak live bytes and 0.05 seconds; RSS unavailable | Add CI RSS measurement and streaming/arena parsing if production data exceeds this shape |
| Production semantics | High | Low-to-medium | Prototype supports only its documented small grammar | Confirm ICU/CLDR plural rules, escaping, rich placeholders, and reference semantics before adoption |
| Fallback traversal | High | High for the prototype grammar | Skeptical review found a comma-separated fallback list stopped after the first locale | Try every fallback locale before returning not-found and validate all fallback edges |

## Decision audit

Decision: **PARTIAL**.

The narrow validation and deterministic-normalization idea is promising: all
seeded defect categories were detected, valid fixtures were clean, diagnostics
were accumulated, lookup behavior was preserved for the representative
fixture, repeated release output was byte-identical, and the measured live
allocation was far below 512 MiB. The result is not evidence to adopt an
existing BogKit component or replace the production compiler.

Choices made:

- Chose a dependency-free batch prototype to respect offline CI and isolate
  catalog semantics from unrelated BogKit subsystems.
- Chose en-US as the structural baseline while preserving whole-message
  fallback for missing localized messages.
- Chose ordered maps and normalized textual runtime tables to make output
  reproducible and inspectable.
- Added an emitted-table loader so the lookup harness tests the generated
  artifact, not only the source representation.

Alternatives rejected:

- Fold: useful for incremental persistent views, but it would add state and
  lifecycle complexity to a batch validator and does not provide catalog
  parsing or diagnostic semantics.
- ESE: embedding compiler/runtime, unrelated to catalogs, and its public build
  currently violates the offline constraint without a model cache.
- ANNy: nearest-neighbor indexing, unrelated to validation or table emission.
- Production-format redesign or release packaging: outside the prototype
  boundary.

Uncertainty that blocks a full adoption decision:

- The real baseline compiler/runtime and production catalog corpus are not
  present in this isolated checkout, so there is no fair 100,000-message
  baseline comparison.
- The prototype's one/other plural rule and placeholder syntax are
  representative only, not a claim about the application's actual format.
- The memory number is peak live allocator bytes, not a kernel-measured RSS.
- Cross-machine reproducibility was reasoned from canonical ordering and
  repeated locally, not executed on a second machine.

## Skeptical review correction

The reviewer reproduced a real fallback-list defect: when `xx` existed but did
not contain `hello`, a catalog with `fallback xx,en-US` stopped at `xx` instead
of continuing to `en-US`. The coordinator corrected lookup to try every
fallback and changed cycle validation to traverse every fallback edge. A new
regression covers the missing-first/found-second case. The corrected prototype
now passes 4 library tests; formatting, strict Clippy, release build, valid and
invalid fixture runs, and both repeated 100,000-record release compiles were
rerun. The corrected stress outputs remain byte-identical with
27,010,299 peak live allocated bytes and 0.05 seconds per compile. The
`PARTIAL` decision remains unchanged.

Smallest next experiment: bring one real, sanitized production-format slice
and the baseline executable into an offline test harness, then differential
test lookup results, diagnostics, output hashes, RSS, and 100,000-message
runtime under the same three-minute/512-MiB budget.
