# Deterministic freight clearing prototype

This is an advisory, file-to-file safe-Rust prototype. It validates an immutable NDJSON snapshot and JSON manifest, clears each `(lane_id, service_date)` market using the exact declared priority rules, rechecks fills and checked gross value, then atomically replaces a canonical NDJSON proposal. Before any temporary output is created, it rejects proposal paths that resolve to either input or identify the same existing file. It does not connect to PostgreSQL or perform booking, payment, notification, or approval work.

The crate is intentionally standalone and uses no BogKit component. Fold's durable incremental database adds a persistent state lifecycle that this one-shot batch does not need. ESE and ANNy solve text-embedding and approximate-search problems, not exact price-time matching.

## Verify

Run from this directory:

```console
cargo test --offline --locked --manifest-path ../../Cargo.toml --package freight-clearing --all-targets
cargo fmt --manifest-path ../../Cargo.toml --package freight-clearing -- --check
cargo clippy --offline --locked --manifest-path ../../Cargo.toml --package freight-clearing --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
```

`cargo test` includes 40 literal golden markets, 10,000 fixed-seed generated markets compared with the deliberately slow scan reference, validation and bound cases, fixed-seed shuffle determinism, publication failure injection, forced I/O errors, and the real CLI. It also permanently retains the skeptical-review regressions for input aliasing, relative output, post-rename durability state, bounded one-MiB corrupt input, and missing or malformed integrity fields.

## Generate the exact large fixture

```console
cargo run --release --offline --locked --manifest-path ../../Cargo.toml --package freight-clearing -- generate /tmp/freight-fixture
```

The defaults are exactly 5,000 markets, 400,000 buys, 250,000 sells, and seed `104372539623448`. A custom shuffle seed uses:

```console
cargo run --release --offline --locked --manifest-path ../../Cargo.toml --package freight-clearing -- generate /tmp/freight-fixture 5000 400000 250000 1
```

The seed changes only row order; the underlying orders remain the same. The generated manifest records counts, the 645,000-fill cap, expected checked gross value, and the order-file SHA-256 digest. The `clear` and `benchmark` boundaries require a non-null expected gross value and a syntactically valid 64-hex order digest; narrow library tests may construct optional manifests without invoking those evidence-producing boundaries.

## Clear an existing fixture

```console
cargo run --release --offline --locked --manifest-path ../../Cargo.toml --package freight-clearing -- clear /tmp/freight-fixture/orders.ndjson /tmp/freight-fixture/manifest.json /tmp/freight-fixture/proposal.ndjson
```

The command prints a single JSON result. Relative proposal filenames are supported. Pre-rename errors have `RunErrorState::NotPublished`, display with `NOT_PUBLISHED:`, and preserve the prior final path. A directory-sync failure after rename has `RunErrorState::PublishedDurabilityUncertain` and displays with `PUBLISHED_DURABILITY_UNCERTAIN:`: the new complete proposal is already visible, but its directory-entry durability is uncertain. These states are intentionally not described as equivalent failures. On Linux the result reads the process high-water mark from `/proc/self/status` without unsafe code; on other hosts that field is `null` and should be supplied by the OS harness.

## Benchmark

```console
cargo run --release --offline --locked --manifest-path ../../Cargo.toml --package freight-clearing -- benchmark /tmp/freight-fixture
```

This generates the default large fixture, times the validate/clear/publish phase, and prints machine-readable counts, elapsed milliseconds, peak RSS when Linux exposes it, maximum temporary bytes, proposal bytes/digest, and gate status. For a clear-only macOS peak-memory measurement, excluding fixture generation:

```console
/usr/bin/time -l ../../target/release/freight-clearing clear /tmp/freight-fixture/orders.ndjson /tmp/freight-fixture/manifest.json /tmp/freight-fixture/proposal.ndjson
```

For the declared Linux target, run `clear` as a fresh process so its `peak_resident_memory_bytes` covers the clearing phase only. Run the supplied Ruby reference and Ruby baseline separately on that same machine before making an acceptance decision.

## Evidence boundary

The retained tests and generator are runnable without a service. The repaired 2026-08-18 trial reran the full 650,000-row shape and all ten shuffle seeds on macOS; the proposal digest remained `e7ce11984ca3e04197df9dd43013361b123e5ecf4b877f3a36f0a08dd64ebcad`. A fresh repaired clear took 1.03 seconds and measured 445,087,744 bytes peak RSS on this host. The authoritative Ruby evaluator, 80 supplied negative fixtures, declared two-core Linux machine, and three Ruby baseline runs were not present in this checkout, so their acceptance gates remain explicitly unverified.
