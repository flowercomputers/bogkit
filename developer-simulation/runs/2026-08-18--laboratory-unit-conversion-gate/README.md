# Exact laboratory-unit admission gate prototype

This is a bounded safe-Rust prototype for exact, fail-closed preview conversion. It does not use Fold, ESE, or ANNy because the workload is an immutable exact batch rather than an incremental database, embedding, or nearest-neighbor problem.

The CLI validates the entire unit/analyte/mapping snapshot, reads bounded NDJSON observation lines, parses decimals without floating point, applies a checked rational affine transform, rounds once with round-half-to-even, sorts report rows by observation ID, and atomically replaces the complete report through a sibling temporary file. Before creating that temporary file it rejects final or temporary paths that resolve to, or share an existing file identity with, any immutable input. Invalid values, unknown mappings, incompatible dimensions, and arithmetic overflow become stable record rejections without echoing the raw value.

Prepublication errors leave the previous report untouched. The parent directory is opened before temporary-file creation and retained through rename and directory sync; a bare relative output uses `.`. A failure after rename is reported separately as `POST_RENAME_DURABILITY_UNCERTAIN` with publication state `PublishedDurabilityUncertain`, because the new report is already visible and preservation of the previous report can no longer be claimed.

## Verify

Run from this directory with no services or network access:

```sh
cargo test --offline --locked --manifest-path ../../Cargo.toml --package lab-unit-gate --all-targets
cargo fmt --manifest-path ../../Cargo.toml --package lab-unit-gate -- --check
cargo clippy --offline --locked --manifest-path ../../Cargo.toml --package lab-unit-gate --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
```

`cargo test` includes exact parsing and half-even boundaries, 20,000 deterministic arithmetic comparisons against a separately implemented slow rational evaluator, reference and NDJSON failures, one retained named unknown-field fixture, additional generated negative cases, read/write faults, duplicate IDs, final/resolved/hard-link/temporary alias probes, relative-path publication, explicit post-rename state, 1 percent/50 percent/pre-rename injected exits, five fixed input shuffles, three repeated digest checks, and the CLI.

## Small runnable demo

Choose an empty directory outside the checkout, then build, generate a 10,000-row fixture, and run the machine-readable benchmark:

```sh
cargo build --offline --locked --manifest-path ../../Cargo.toml --package lab-unit-gate --release
../../target/release/lab-unit-gate generate /tmp/lab-unit-gate-demo 9500 250 250 42
../../target/release/lab-unit-gate bench /tmp/lab-unit-gate-demo /tmp/lab-unit-gate-demo/report.ndjson
```

The generator always writes exactly 250 units, 300 analytes, and 12,000 unique mappings. Its four input digests and class counts are recorded in `manifest.json`. The five numeric arguments are `ordinary`, `boundary`, `rejected`, and `seed`; the default trial shape is expressed explicitly below.

## Full synthetic shape

```sh
../../target/release/lab-unit-gate generate /tmp/lab-unit-gate-full 950000 25000 25000 42
../../target/release/lab-unit-gate bench /tmp/lab-unit-gate-full /tmp/lab-unit-gate-full/report.ndjson
```

The benchmark prints one JSON line. `run_status:"complete"` means the transform finished and published. `acceptance` is `failed` when an internally measured peak exceeds 256 MiB and otherwise remains `unverified`, because signed-fixture parity, the Java comparison, and the declared two-core Linux environment are not available to this command. It never emits acceptance `pass`. On Linux the process reads peak resident bytes from `/proc/self/status`; on macOS `peak_rss_bytes` is `null`, so use `/usr/bin/time -l` for separately labeled host evidence.

## Input schema

- `units.ndjson`: `symbol`
- `analytes.ndjson`: `analyte_code`, `dimension`, `target_scale`
- `mappings.ndjson`: `source_system`, `analyte_code`, `source_unit`, `target_unit`, `expected_dimension`, `multiplier_num`, `multiplier_den`, `offset_num`, `offset_den`
- `observations.ndjson`: `observation_id`, `source_system`, `analyte_code`, `source_unit`, `declared_dimension`, `value`

Every field is required and unknown fields are rejected. Observation IDs must be nonempty ASCII and unique. A line may contain at most 1,024 bytes. Decimal values accept an optional leading minus, digits, and an optional fractional part; scientific notation, plus signs, NaN, infinity, more than 29 significant digits, and scale above nine are rejected.

## Evidence boundary

The supplied checkout did not include the signed fixtures, frozen Java evaluator, target two-core Linux harness, or Java 100,000-row baseline. Therefore byte parity with Java, all named supplied negative fixtures, target-machine timing/memory, and the 1.5-times Java comparison remain unverified.

The repaired macOS arm64 million-row run was fast and deterministic but used about 277.7 MiB peak resident memory, above the 256 MiB target. Five full shuffles plus three full repeats produced the same canonical report digest. The prototype keeps report rows and duplicate IDs in memory to stay small and inspectable. A production candidate would need bounded external merge sorting that detects duplicate IDs during merge before this gate could be considered passed.
