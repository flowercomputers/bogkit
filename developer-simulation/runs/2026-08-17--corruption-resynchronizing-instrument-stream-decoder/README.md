# Instrument decoder trial prototype

This archived Rust package contains:

- an append-and-search baseline;
- a bounded, per-connection incremental candidate decoder;
- CRC-32/ISO-HDLC framing and an independent whole-frame oracle;
- deterministic clean, damaged, chunk-schedule, property, memory-bound, and throughput harnesses;
- a small command-line runner.

It has no external dependencies and does not use Fold, ESE, or ANNy. The trial report explains that decision.

The decoder deliberately treats payload bytes as opaque. It never emits a valid-looking nested frame while an accepted-length containing frame is incomplete. The retained regressions demonstrate why this safe policy cannot also satisfy the brief's immediate-recovery rule for every plausible false header; this workspace is an incompatibility reproducer, not a production candidate.

Use an external target directory so build artifacts do not remain in the evidence folder:

```sh
export CARGO_TARGET_DIR=/private/tmp/bogkit-instrument-decoder-target
export MANIFEST=developer-simulation/Cargo.toml
cargo test --locked --manifest-path "$MANIFEST" --package instrument-decoder-trial --all-targets
cargo test --locked --manifest-path "$MANIFEST" --package instrument-decoder-trial candidate_never_emits_valid_nested_payload_before_outer_is_proved -- --nocapture
cargo test --locked --manifest-path "$MANIFEST" --package instrument-decoder-trial candidate_defers_nested_followers_until_plausible_header_fails -- --nocapture
cargo clippy --locked --manifest-path "$MANIFEST" --package instrument-decoder-trial --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
cargo build --locked --manifest-path "$MANIFEST" --package instrument-decoder-trial --release
$CARGO_TARGET_DIR/release/instrument-decoder-trial demo
$CARGO_TARGET_DIR/release/instrument-decoder-trial representative
$CARGO_TARGET_DIR/release/instrument-decoder-trial full
$CARGO_TARGET_DIR/release/instrument-decoder-trial property 100000
$CARGO_TARGET_DIR/release/instrument-decoder-trial bench 100000
```

Remove the external target directory after use.
