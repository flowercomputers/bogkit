# Snapshot GC Safety

This is a runnable prototype for bounded-memory, recoverable garbage collection of
content-addressed blobs referenced by append-only JSONL snapshot manifests. It is
self-contained and does not modify BogKit or use a BogKit component.

## What it does

`plan` validates every committed `manifests/*.jsonl` record before doing anything
destructive. It externally sorts fixed-size binary hashes in small chunks, compares
them with the blob inventory, and commits a named candidate list.

`apply` takes the publication lock, validates all currently committed manifests
again, and atomically renames only currently unreferenced candidates into the plan's
quarantine. A successful apply deliberately stops there so recovery remains possible.

`resume` takes the same lock, validates manifests again, restores any quarantined
blob that has become referenced, deletes the remaining quarantined blobs, and writes
a completion marker. The phase markers and filesystem state make all three commands
safe to repeat after ordinary process termination at any mutation boundary.

`publish` demonstrates the required publisher side of the protocol without changing
the manifest format: write and flush a temporary JSONL file, take the publication
lock, reject an existing final name, restore a referenced blob from quarantine if
necessary, then atomically rename the manifest into its committed `.jsonl` name.
Production publishers must follow this lock-and-rename contract for the concurrency
guarantee to hold.

Only regular files with a `.jsonl` suffix are treated as committed manifests. Every
record must end in a newline and contain a string `hash` field with exactly 64 hex
characters. A bad committed record reports its manifest and one-based record number.

## Build and verify

The only dependency is `serde_json`, used to parse JSON correctly. The commands below
keep generated build output outside this archive-safe directory.

```sh
cd developer-simulation
export CARGO_TARGET_DIR=/private/tmp/snapshot-gc-safety-target
cargo fmt --manifest-path runs/2026-08-02--snapshot-gc-safety/Cargo.toml -- --check
cargo test -p snapshot-gc-safety --all-targets --offline
cargo clippy -p snapshot-gc-safety --all-targets --offline -- -D warnings
cargo build -p snapshot-gc-safety --release --offline
python3 runs/2026-08-02--snapshot-gc-safety/acceptance.py "$CARGO_TARGET_DIR/release/snapshot-gc-safety"
```

The acceptance harness checks 30 differently seeded repositories against an
independent in-memory oracle. It also checks truncated and malformed committed
manifests, a manifest published during planning, publication from quarantine, two
same-name publishers forced to contend for the lock, every quarantine and
finalization crash boundary, and repeated plan/apply/resume calls.

## Small demonstration

```sh
DEMO_ROOT="$(mktemp -d /private/tmp/snapshot-gc-demo.XXXXXX)"
BIN="$CARGO_TARGET_DIR/release/snapshot-gc-safety"
"$BIN" fixture "$DEMO_ROOT" --manifests 10 --references 1000 --unique 250 --inventory 300 --missing 3
"$BIN" plan "$DEMO_ROOT" nightly
"$BIN" verify-plan "$DEMO_ROOT" nightly
"$BIN" apply "$DEMO_ROOT" nightly
"$BIN" status "$DEMO_ROOT" nightly
"$BIN" resume "$DEMO_ROOT" nightly
"$BIN" status "$DEMO_ROOT" nightly
```

Expected key lines are `planned 53 candidates`, `oracle match ... zero referenced
blobs selected`, then `quarantined`, and finally `complete`.

To publish a manifest safely, pass its final name and referenced hashes:

```sh
"$BIN" publish "$DEMO_ROOT" snapshot-new.jsonl 0000000000000000000000000000000000000000000000000000000000000001
```

## Realistic scale and measurement

```sh
SCALE_ROOT="$(mktemp -d /private/tmp/snapshot-gc-scale.XXXXXX)"
"$BIN" fixture "$SCALE_ROOT"
python3 runs/2026-08-02--snapshot-gc-safety/measure.py --scratch "$SCALE_ROOT/.snapshot-gc" -- "$BIN" plan "$SCALE_ROOT" realistic
"$BIN" verify-plan "$SCALE_ROOT" realistic
python3 runs/2026-08-02--snapshot-gc-safety/measure.py -- "$BIN" apply "$SCALE_ROOT" realistic
python3 runs/2026-08-02--snapshot-gc-safety/measure.py -- "$BIN" resume "$SCALE_ROOT" realistic
```

The default fixture is the requested scale: 10,000 manifests, 1,000,000 reference
records, 250,000 unique hashes, 300,000 zero-byte inventory blobs, 1,000 missing
referenced blobs, duplicates, and 51,000 unreachable inventory blobs. To exercise the
malformed guard, add a committed file without its final newline before `plan`:

```sh
printf '%s' '{"hash":"abc"}' > "$SCALE_ROOT/manifests/truncated.jsonl"
"$BIN" plan "$SCALE_ROOT" must-abort
```

## Frozen comparison baseline

`baseline.py` is the runnable Python behavior frozen before the BogKit fit decision.
It loads every referenced hash into one Python set and directly unlinks unreferenced
blobs. Its SHA-256 is
`682bc139ce3ed25b16e16daef616014d6be890f0da30eaecd5fb6c9719b27bea`.

```sh
BASELINE=runs/2026-08-02--snapshot-gc-safety/baseline.py
shasum -a 256 "$BASELINE"
python3 "$BASELINE" seed /private/tmp/snapshot-gc-baseline --references 100000 --unique 25000 --inventory 30000 --manifests 1000
python3 "$BASELINE" collect /private/tmp/snapshot-gc-baseline
```

The detailed observed results, design audit, limitations, and discovery trail are in
`TRIAL_REPORT.md`.
