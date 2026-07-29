# Flash configuration journal prototype

This dependency-free Rust CLI tests a power-loss-safe journal for one complete
2–24 KiB configuration blob in 128 KiB of emulated NOR flash.

The journal writes the payload and checked header into the next circular run of
erase blocks. It writes a one-byte commit marker last. Boot scans exactly 32
block starts, validates headers and payload CRCs, and selects the highest valid
revision. The previous complete record is never erased while its replacement is
being written.

Run from `developer-simulation/`:

```console
cargo test -p flash-config-journal --all-targets
cargo clippy -p flash-config-journal --all-targets -- -D warnings
cargo run -p flash-config-journal
```

The CLI writes its 128 KiB file-backed flash image to
`target/flash-config-journal-demo.bin`. The in-memory test emulator checks NOR
1→0 programming, deterministic corruption, every modeled byte boundary of one
minimum- and one maximum-size update, 10,000 fixed 2 KiB updates for wear
balance, scan count, and the explicit-buffer design budget.

This is a host-side model, not a hardware driver or a claim about physical NOR
timing, whole-stack memory, or failure behavior. See
[`EVIDENCE.md`](EVIDENCE.md) for the bounded decision audit.
