# Offline flag parity prototype

A local Rust library and CLI for loading a validated JSON flag snapshot,
evaluating ordered targeting rules, and retaining the last valid snapshot when
a reload fails.

The evaluator is deliberately independent from BogKit. `EVIDENCE.md`
explains why the direct immutable-snapshot baseline fit this workload better
than a durable incremental database.

Run from this directory, while keeping build output outside the archive:

```console
export CARGO_TARGET_DIR=/tmp/offline-flag-parity-target
cargo run --offline --release -- generate /tmp/offline-flag-fixtures
cargo run --offline --release -- verify /tmp/offline-flag-fixtures
cargo run --offline --release -- demo /tmp/offline-flag-fixtures
cargo run --offline --release -- eval /tmp/offline-flag-fixtures/snapshot.json /tmp/offline-flag-fixtures/contexts.ndjson
cargo run --offline --release -- fingerprint /tmp/offline-flag-fixtures
node baseline/reference-evaluator.mjs /tmp/offline-flag-fixtures
cargo run --offline --release -- generate-benchmark /tmp/offline-flag-benchmark.json
cargo run --offline --release -- bench /tmp/offline-flag-benchmark.json
```

Snapshot rules are arrays, so their order is meaningful. JSON object order is
not meaningful. Percentage rules use FNV-1a 64 over null-separated snapshot
salt, flag key, rule id, and user attribute, then map the result into 10,000
basis-point buckets. This algorithm is implemented locally so it does not vary
with process-randomized hash maps or restarts. Rust and the independent
JavaScript reference matched on the measured host; broader platform parity
still needs a CI matrix.
