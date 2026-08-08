# Mixed-version contract gate trial

This standalone Rust CLI checks every producer-version × consumer-version pair
per permitted topology relationship. It implements only this deliberately small
contract language:

- `type`: `string`, `integer`, `array`, or `object`
- string `enum`
- integer `minimum` and `maximum` (inclusive, signed 64-bit)
- array `items`
- object `properties`, `required`, and boolean `additionalProperties`
- `default` on any supported schema; a required property with a valid default
  may be absent because the receiver materializes that default

Anything else is `review-required`, with a source and semantic JSON-pointer
location. Defaults do not otherwise assert application semantics.

## Input files

`contracts.json` and `candidate.json` contain:

```json
{"contracts":[{"service":"api","topic":"orders","version":3,"schema":{"type":"object","properties":{},"required":[],"additionalProperties":false}}]}
```

Candidate entries replace a base contract with the same service, topic, and
version. Identical duplicate entries are ignored; conflicting duplicates need
review.

`topology.json` contains:

```json
{"relationships":[{"topic":"orders","producer":"api","consumer":"worker"}]}
```

`fleet.json` contains the complete permitted version set for each service:

```json
{"services":{"api":[1,2,3],"worker":[1,2,3]}}
```

## Run

Run from the BogKit repository root. Generated inputs and build output stay
outside the archive.

```console
export CARGO_TARGET_DIR=/private/tmp/mixed-version-contract-target
DEMO_DIR="$(mktemp -d /private/tmp/mixed-version-contract-demo.XXXXXX)"
cargo run --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p mixed-version-contract-gate --bin generate -- demo "$DEMO_DIR"
cargo run --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p mixed-version-contract-gate --bin contract-gate -- \
  "$DEMO_DIR/contracts.json" \
  "$DEMO_DIR/topology.json" \
  "$DEMO_DIR/fleet.json" \
  "$DEMO_DIR/candidate.json"
```

Exit status is 0 for `allow`, 1 for `block`, and 2 for `review-required` or
input/usage errors. JSON output is deterministic. A witness is ranked first by
JSON structural size, then encoded byte length, then canonical JSON bytes.

The CLI uses an incremental path: it reuses the base result for unaffected
pairs and reevaluates every pair touched by a candidate key. Tests compare that
result exactly with a fresh full evaluation.

## Verification

```console
export CARGO_TARGET_DIR=/private/tmp/mixed-version-contract-target
cargo fmt --manifest-path developer-simulation/Cargo.toml \
  -p mixed-version-contract-gate -- --check
cargo clippy --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p mixed-version-contract-gate --all-targets -- -D warnings
cargo test --offline --locked --manifest-path developer-simulation/Cargo.toml \
  -p mixed-version-contract-gate
python3 developer-simulation/runs/2026-08-04--mixed-version-contract-gate/oracle.py \
  developer-simulation/runs/2026-08-04--mixed-version-contract-gate/fixtures/semantic_cases.json
```

Generate the stated workload with:

```console
WORKLOAD_DIR="$(mktemp -d /private/tmp/mixed-version-contract-workload.XXXXXX)"
cargo run --release --offline --locked \
  --manifest-path developer-simulation/Cargo.toml \
  -p mixed-version-contract-gate --bin generate -- workload "$WORKLOAD_DIR"
```

It contains exactly 300 services, 120 topics, 1,800 contracts, 12,000 unique
relationships, three permitted versions per service, and 25 candidate entries.
