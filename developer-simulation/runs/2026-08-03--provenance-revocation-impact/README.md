# Provenance revocation impact

This is a minimal fit reproducer for transitive software-provenance revocation.
It persists input facts with Fold, then compares an intentionally incomplete
one-hop negative control with a bounded recursive correctness oracle. The
negative control is ordinary Rust, not a composition of Fold operators; its two
unsafe approvals are prototype defects, not BogKit defects.

The reviewed conclusion is no fit for the stated PostgreSQL-authoritative gate.
The current public Fold surface does not supply joins or recursive reachability,
and moving derived decisions into a local embedded store would add another
authority and reconciliation boundary.

## Reproduce

Run from the repository root. All generated state and build output remain under
`/private/tmp`.

```sh
export CARGO_TARGET_DIR=/private/tmp/provenance-revocation-target
cargo fmt --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer -- --check
cargo test --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --offline --locked
cargo clippy --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --all-targets --offline --locked -- -D warnings
cargo build --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --release --offline --locked

PROV_ROOT="$(mktemp -d /private/tmp/provenance-revocation.XXXXXX)"
PROV_BIN="$CARGO_TARGET_DIR/release/provenance-revocation-reproducer"
"$PROV_BIN" generate | "$PROV_BIN" run "$PROV_ROOT/negative-control" candidate
"$PROV_BIN" generate | "$PROV_BIN" run "$PROV_ROOT/reference" reference
```

The negative control approves `transitive` and `cyclic`; the reference blocks
them as `revoked` and `invalid_cycle`. Both block `unknown` as
`missing_manifest`. This is a three-query failure reproducer, not a scale test.

## Process-abort boundaries

Build a separate abort-on-panic binary, then inject before and after the first
Fold transaction commits:

```sh
export CARGO_TARGET_DIR=/private/tmp/provenance-revocation-abort-target
export RUSTFLAGS='-C panic=abort'
cargo build --manifest-path developer-simulation/Cargo.toml \
  -p provenance-revocation-reproducer --release --offline --locked

CRASH_ROOT="$(mktemp -d /private/tmp/provenance-revocation-abort.XXXXXX)"
ABORT_BIN="$CARGO_TARGET_DIR/release/provenance-revocation-reproducer"
printf '%s\n' '{"op":"artifact","id":"app"}' \
  | PROVENANCE_CRASH=before_commit:1 "$ABORT_BIN" run "$CRASH_ROOT/before" reference
printf '%s\n' '{"op":"artifact","id":"app"}' \
  | PROVENANCE_CRASH=after_commit:1 "$ABORT_BIN" run "$CRASH_ROOT/after" reference
```

Each injected run exits by abort. Reopening the first store shows `app` absent;
reopening the second shows it present. This covers two local process-abort
boundaries only, not OS failure, power loss, PostgreSQL integration, versioned
publication, concurrent ingestion, or the requested 500,000-artifact scale.

See `TRIAL_REPORT.md` for the full blind-developer trail, skeptical corrections,
decision audit, and exact limitations.
