# Edge spool pressure prototype

This local executable compares:

1. A streaming model of the current age-based newline-file queue.
2. A small Fold-backed priority spool.

The candidate intentionally stops short of claiming production fit. Fold
provides useful atomic, crash-safe state, but its public interface does not
provide a hard allocated-disk quota guarantee. The `quota-probe` command
demonstrates why logical accounting alone cannot enforce that requirement.

Run from `developer-simulation/`.

Run the tests and strict lint:

```console
cargo test -p edge-spool-pressure --all-targets
cargo clippy -p edge-spool-pressure --all-targets -- -D warnings
```

Run all representative checks:

```console
cargo run -p edge-spool-pressure --release -- demo
```

Run only the deterministic one-million-event baseline model:

```console
cargo run -p edge-spool-pressure --release -- baseline
```

Run a quota probe in a new directory:

```console
cargo run -p edge-spool-pressure --release -- quota-probe /tmp/bogkit-quota-probe
```

The demo creates a unique directory under the operating system temporary
directory and prints the path. It does not delete prior runs.

The 1 MiB probe shows that retained logical bytes can stay under their limit
while allocated database bytes exceed it. It does not predict exact allocation
at 256 MiB or evaluate an external filesystem quota. See
[`EVIDENCE.md`](EVIDENCE.md) for the bounded decision audit.
