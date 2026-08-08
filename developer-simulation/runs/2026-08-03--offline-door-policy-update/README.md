# Offline door policy update

This is a host-side fit probe for frequent offline door-controller policy
updates. Fold successfully applies grant retractions and policy-version metadata
in one logical transaction, but the stated controller requires a fixed 16 MiB
flash image, a 4 MiB working-memory bound, signed and truncation-safe delivery,
and old-or-new recovery after every modeled 4 KiB write.

The reviewed conclusion is no fit for the controller. Fold uses a
filesystem-backed embedded store and does not expose the required raw-block fault
model. No BogKit correctness defect was found.

## Reproduce

Run from the repository root. The binary requires an empty caller-supplied state
directory, so tests and demonstrations never generate databases inside the
archive.

```sh
export CARGO_TARGET_DIR=/private/tmp/offline-door-policy-target
cargo fmt --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe -- --check
cargo test --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe --offline --locked
cargo clippy --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe --all-targets --offline --locked -- -D warnings
cargo build --manifest-path developer-simulation/Cargo.toml \
  -p offline-door-policy-fit-probe --release --offline --locked

DOOR_ROOT="$(mktemp -d /private/tmp/offline-door-policy.XXXXXX)"
"$CARGO_TARGET_DIR/release/offline-door-policy-fit-probe" "$DOOR_ROOT"
```

Expected evidence includes:

- zero mismatches across 40,000 generated authorization queries;
- 50,000 revoked grants applied within the two-second host criterion;
- a wrong-base next version rejected as `BaseMismatch` without mutation;
- same-version delivery labeled `SameVersionIgnoredUnverified` because bundle
  identity and authenticity are outside the probe;
- old and skipped versions rejected, contiguous repair accepted, and active
  policy preserved across clean checkpoint/reopen;
- a regular 16 MiB file rejected as the database path;
- explicit `not_provided` or `not_injectable` results for signature,
  truncation, and per-4 KiB power-cut requirements.

Whole-process RSS includes the reference map, generated bundles, runtime, code,
and database mappings. It is not a Fold-only or controller-memory measurement.
The missing-version diagnostic is process-local; only active version and last
verified time are persisted.

See `TRIAL_REPORT.md` for the full blind-developer trail, the reviewer-discovered
wrong-base defect and regression, decision audit, and exact limitations.
