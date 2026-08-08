# CNC job bundle preflight prototype

This offline Rust command checks untrusted CNC job ZIPs before staging. It
validates safe names, a versioned JSON manifest, exact/case-colliding names,
declared file presence, undeclared members, byte counts, SHA-256 and ZIP CRC,
entry-program and tool references, and a 1 GiB total-member policy. It stages
only a fully valid bundle through a temporary directory, rechecks every copied
member, and assigns the `ready` name only after the complete copy succeeds.
Incomplete temporary output is removed after an ordinary write failure.

This prototype intentionally accepts only classic, uncompressed ZIP members.
Compressed, encrypted, multi-disk, and ZIP64 archives fail closed.

## Build and test

```console
cargo fmt --all -- --check
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --offline --release
```

The nested empty `[workspace]` in `Cargo.toml` keeps this crate isolated from
the surrounding BogKit workspace. It uses no BogKit crate because the trial
found no component that improves a one-shot streaming trust-boundary check.

## Generate and inspect fixtures

Keep generated fixtures and staging outside this archiveable directory:

```console
target/release/cnc-job-bundle-preflight generate-fixtures /tmp/cnc-fixtures --include-huge
target/release/cnc-job-bundle-preflight demo /tmp/cnc-fixtures /tmp/cnc-staging
target/release/cnc-job-bundle-preflight check /tmp/cnc-fixtures/valid.zip \
  --tools /tmp/cnc-fixtures/tools.json --staging /tmp/cnc-one-stage
```

`demo` exits successfully only if `valid.zip` is ready and all adversarial
fixtures are invalid. `check` prints deterministic JSON and exits 0 for ready,
1 for invalid, or 2 for command/setup errors.

`baseline.py BUNDLE.zip` reproduces the filename-only Python comparison.
See `EVIDENCE.md` for the evidence and limitations.
