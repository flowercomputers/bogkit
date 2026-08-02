# Carrier label ambiguity reliability core

This is a self-contained Rust simulation of the safety boundary around a carrier label purchase. It models the existing PostgreSQL transaction as a checksummed local decision journal, while the carrier simulator remains a separate authority for charges. It deliberately does not use a BogKit crate; the reason is recorded in `TRIAL_REPORT.md`.

The central rule is simple: persist one purchase intent before contacting the carrier, and never automatically call the purchase endpoint again after that intent exists. A missing response is resolved by carrier lookup, a trustworthy callback, or `needs_review`.

## What is included

- deterministic carrier outcomes, including exactly 10% ambiguous timeouts;
- a synced workflow journal whose recorded post-state is verified by replay;
- repair of a recognized incomplete final journal record before later appends;
- duplicate and reordered carrier callbacks with monotonic state changes;
- a reconciliation worker that resolves known labels and exposes unknown outcomes;
- four real child-process exits around network and persistence boundaries;
- a 20,000-shipment, 30-seed acceptance harness with invariant checks;
- no third-party dependencies, secrets, private data, databases, or generated fixtures.

The journal is a stand-in for an existing PostgreSQL transaction, not a proposed production storage replacement. In production, each journal commit corresponds to updating the shipment row and inserting the decision-history row in one PostgreSQL transaction.

## Exact reproduction

Run these commands from the repository's `developer-simulation` directory.

```sh
cargo fmt --manifest-path runs/2026-08-02--carrier-label-ambiguity/Cargo.toml -- --check
cargo clippy -p carrier-label-ambiguity --all-targets -- -D warnings
cargo test -p carrier-label-ambiguity --all-targets
cargo build -p carrier-label-ambiguity --release

DEMO_ROOT="$(mktemp -d /private/tmp/carrier-label-demo.XXXXXX)"
./target/release/carrier-label-ambiguity demo --dir "$DEMO_ROOT/run"

CRASH_ROOT="$(mktemp -d /private/tmp/carrier-label-crashes.XXXXXX)"
./target/release/carrier-label-ambiguity crash-demo --dir "$CRASH_ROOT/run"

ACCEPTANCE_ROOT="$(mktemp -d /private/tmp/carrier-label-acceptance.XXXXXX)"
./target/release/carrier-label-ambiguity acceptance \
  --dir "$ACCEPTANCE_ROOT/run" \
  --shipments 20000 \
  --seeds 30
```

On macOS, measure peak resident memory while running the same realistic fixture:

```sh
MEASURED_ROOT="$(mktemp -d /private/tmp/carrier-label-measured.XXXXXX)"
./runs/2026-08-02--carrier-label-ambiguity/scripts/measure-acceptance.sh \
  ./target/release/carrier-label-ambiguity \
  "$MEASURED_ROOT/run" \
  "$MEASURED_ROOT/output.log"
```

The measurement script uses `ps` to sample the process. A restricted sandbox may need permission to inspect the local process.

## Expected completion lines

The small demo ends with `ACCEPTANCE PASS seeds=1 shipments=100`. The crash harness ends with `CRASH/RESTART PASS scenarios=4 automatic_retries=0`. The realistic fixture ends with `ACCEPTANCE PASS seeds=30 shipments=600000` and the measurement script prints `MEASURED PEAK RSS`.

The fixture fails immediately if a purchase is called twice, a carrier-created label is lost, a timeout without authoritative evidence avoids review, a shipment remains nonterminal, convergence exceeds 60 simulated seconds, an attempt disappears across restart, or replay disagrees with recorded workflow state. The focused journal regression also proves partial write, reopen, later commit, and second reopen.

The durability evidence covers ordinary process termination after completed sync calls on the measured host. It is not a PostgreSQL, concurrent-worker, network, kernel-failure, or power-loss qualification.
