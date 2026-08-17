# Test-first development log

All commands below were run from `simulation-output/` with the build target isolated at `/private/tmp/bogkit-reach-target`.

## Cycle 1: exact set baseline and routing validation

**Breaks named before the tests:** counting records instead of distinct IDs; accepting a tenant/hour outside the declared day; overflowing the day boundary; excluding the minimum or maximum declared tenant.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test baseline
```

**Observed RED:** exit 101. Rust reported unresolved `BucketKey`, `DayWindow`, `ExactRollup`, and `Record` imports. This was the expected missing-feature failure from an otherwise valid test crate.

**GREEN change:** added only the record/key model, checked day-window validation, and exact map-of-sets behavior required by the three tests.

**GREEN command/result:** the same command exited 0; 3 tests passed.

## Cycle 2: bounded mergeable sketch

**Breaks named before the tests:** biased hash/rank calculation; loss of duplicate idempotence; order-sensitive merge; unbounded state; missing version/hash identity; and accepting corrupt or truncated state.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test sketch
```

**Observed RED:** exit 101. Rust reported missing `HllState` and `IncompatibleState`, the intended absent candidate surface.

**GREEN change:** added a 4,096-register state, explicit fixed-seed 128-bit-ID hash, register-wise maximum merge, corrected estimator, and a checksummed 4,124-byte encoding with format and hash identity.

**GREEN command/result:** the same command exited 0; 4 tests passed.

## Cycle 3: deterministic corpus and streaming record validation

**Breaks named before the tests:** workload count drift; uneven or sorted shards; incorrect bucket boundaries; novel IDs in the duplicate tail; buffered-only parsing; and accepting unknown, truncated, trailing, or out-of-window records.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test generator_reader
```

**Observed RED:** exit 101. Rust reported the generator constants/type and record encoding/streaming functions were missing.

**GREEN change:** added a procedural 12-million-record corpus with exact equal shuffled shards, fixed cardinality layout and deterministic duplicates, plus a versioned streaming reader that validates before forwarding each record.

**GREEN command/result:** the initial GREEN run exposed one unused import warning; after a no-behavior refactor, the same command exited 0 with 3 tests passed and no warnings.

## Cycle 4: canonical rollup, merge-order proof, and atomic reporting

**Breaks named before the tests:** hidden dependence on input/shard/map order; missing or reordered report rows; digesting different bytes; and overwriting a prior report before validation or final rename.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test rollup_report
```

The first compile also exposed ambiguous integer types in the test fixture. I corrected that test error and reran RED. **Observed valid RED:** exit 101 with only the intended missing `ApproxRollup`, publication, and digest surface.

**GREEN change:** added canonical keyed candidate rollups, merge compatibility checks, deterministic state/report bytes, stable digesting, report/oracle completeness validation, and temporary-file publication with rename only after validation.

**GREEN command/result:** the same command exited 0; 3 tests passed, including 50 input permutations and 50 shard-merge permutations.

## Cycle 5: complete accuracy matrix and bias evidence

**Break named before the test:** a candidate that meets the error contract only for handpicked cardinalities or seeds, or emits incomplete/nondeterministic evidence.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test accuracy
```

**Observed RED:** exit 101 with the intended missing `run_accuracy_matrix` surface.

**GREEN change:** added all 20 seeds × 6 cardinalities × 24 buckets, 25% duplicate replay, nearest-rank percentiles, separate signed-bias summaries, named worst positive/negative cases, and deterministic row-level TSV evidence.

**GREEN verification:** the first run correctly failed the p95 gate (`3.6115%`) and exposed transition-range bias concentrated at cardinality 10,000. Switching the estimator to the independently derivable empty-register estimate through 12,000 removed that bias; the acceptance test then passed. Temporary diagnostic printing was removed before the final GREEN run.

## Cycle 6: shard-state transport and compact oracle

**Breaks named before the tests:** shard state that cannot be decoded safely; accepting corrupt, truncated, or wrong-seed rollups; retaining 10.8 million IDs during candidate comparison; and losing report completeness after compacting the oracle to exact counts.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test state_file
```

**Observed RED:** exit 101 with the intended missing state-file decoder, exact-count compaction, and count-based report methods.

**GREEN change:** added strict canonical state-file decoding with nested state validation and seed checks, exact-set consumption into compact counts, and complete count-based report generation.

**GREEN command/result:** the same command exited 0; 2 tests passed.

## Cycle 7: full 12-million-record correctness and determinism harness

**Break named before the test:** logic that works only on small fixtures but drifts in workload counts, duplicate behavior, shard merging, 50 merge orders, or complete-run byte determinism at the representative scale.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --release --test load_contract
```

**Observed RED:** exit 101 with only the intended missing `run_load_verification` entry point.

**GREEN change:** added a procedural full-load harness that runs the exact oracle, compacts it to counts, verifies direct/unique/sharded/repeated candidate states, checks 50 full-scale shard orders, produces the canonical 5,760-row report and digests, and removes temporary shard states on every return path.

**GREEN command/result:** the release test exited 0 after one 19.38-second representative run; all asserted workload and non-performance gates passed, and its external temporary directory was absent at return.

## Cycle 8: comparable isolated benchmark modes

**Break named before the test:** exact and candidate timing/RSS commands that process different records, omit full outputs, or allow the optimizer to discard results.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --release --test benchmark
```

**Observed RED:** exit 101 with the intended missing exact/candidate benchmark entry points.

**GREEN change:** added isolated benchmark functions over the identical procedural shards, with complete bucket/result digests printed by the caller. The representative test is intentionally ignored in routine suites and is run explicitly in release mode for evidence.

**GREEN command/result:** the same command exited 0 with the representative test compiled and explicitly reported as ignored.

## Cycle 9: deterministic command-line regeneration recipe

**Breaks named before the tests:** evidence that requires source edits to regenerate, and a typo that silently launches an expensive mode.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test cli
```

**Observed RED:** exit 101 because the intended binary target did not yet exist.

**GREEN change:** added explicit help, exact/candidate benchmark commands, and one release demo command that writes tuning, held-out, load-row, and summary evidence atomically.

**GREEN command/result:** the same command exited 0; 2 CLI behavior tests passed.

## Cycle 10: adversarial IDs and skewed shards

**Break named before the test:** hashing and merging that works for balanced pseudorandom examples but fails on sequential IDs, a shared 120-bit prefix, one-changing-byte IDs, reversed input, 1,000 duplicate replays, or one hot shard.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --test adversarial
```

**Observed RED:** exit 101 with the intended missing adversarial-verification entry point.

**GREEN change:** added explicit pattern, order, duplicate, skewed-shard, empty-input, and one-bucket checks over the real candidate.

**GREEN command/result:** the same command exited 0; the adversarial behavior test passed.

## Initial pre-review verification after GREEN

```console
cargo fmt --all -- --check
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo test --release --all-targets --all-features
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo clippy --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-target cargo run --release -- demo evidence /private/tmp/bogkit-reach-demo-temp
```

- Formatting check: passed after applying `cargo fmt --all` once.
- Release all-target tests: 20 passed, 0 failed, 1 representative benchmark test intentionally ignored; the same benchmark entry points were run separately for one warm-up plus three timed/RSS samples each.
- Strict Clippy: passed with warnings, all, and pedantic denied. The crate records six narrowly scoped pedantic allowances for bounded numeric casts, public prototype error/panic prose, simple fixed-register byte counting, similar evidence names, and boolean-heavy evidence result records.
- Release demo: passed and atomically regenerated two 2,880-row accuracy artifacts, one 5,760-row load report, and the summary evidence. The temporary shard directory was removed.
- Accuracy disclosure: the original specified matrix exposed the 10,000-cardinality transition bias and was therefore used while selecting the final 12,000 transition threshold. It is labeled `tuning`; a different predeclared generator seed was then run once as held-out validation and was not used for further tuning.

## Skeptical-review repair cycle: complete state-file binding

**Break named before the test:** valid-range bit corruption that changes an outer tenant or hour while leaving the nested sketch bytes and nested checksum untouched.

**RED command**

```console
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo test --test state_file state_file_checksum_binds_valid_in_window_tenant_and_hour_keys -- --nocapture
```

**Observed RED:** exit 101. The new regression expected complete-binding format version 2 but observed version 1, precisely identifying the unprotected outer format.

**GREEN change:** versioned the state file to v2, added an eight-byte file checksum over magic, version, reserved bytes, bucket count, every tenant/hour key, and all nested state bytes, and made decoding verify length and that checksum before constructing any rollup.

**GREEN command/result:** the complete `state_file` test target exited 0; 3 tests passed, including valid-window tenant mutation, valid-window hour mutation, checksum mutation, round-trip, version, wrong-seed, and truncation checks.

## Final skeptical-review repair verification

```console
cargo fmt --all -- --check
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo test --release --all-targets --all-features
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo clippy --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
CARGO_TARGET_DIR=/private/tmp/bogkit-reach-repair-target cargo run --release -- demo evidence /private/tmp/bogkit-reach-repair-demo-temp
```

- Formatting: passed.
- Release all-target tests: 21 passed, 0 failed, 1 benchmark wrapper intentionally ignored; the exact benchmark entry points were separately run after one warm-up and measured three times each.
- Strict Clippy with warnings, all, and pedantic denied: passed.
- Corrected release demo: passed. It reran both 2,880-row matrices, the exact 12-million-record oracle, duplicate and self-merge checks, eight-shard direct equality, 50 full-load merge orders, repeated state/report bytes, malformed/publication tests through the release suite, and adversarial cases. Temporary shard states were removed.
- Corrected v2 state digest: `ecda69b5ba1467d1`.
- Corrected repeated summary digest: `ca4d7c625e0eec4d`.
- Canonical report digest remained `6219b8fa82ef6a79` because estimates and row ordering did not change.
- Corrected slower-of-three exact/candidate wall times: `1.00 s` / `0.55 s`; candidate ratio `0.550×`.
- Corrected largest candidate peak RSS: `49,627,136` bytes; conservative reduction against the smallest exact peak is `12.61×`.
