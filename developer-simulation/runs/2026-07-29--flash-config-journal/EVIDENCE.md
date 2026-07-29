# Blind BogKit trial: flash-config-journal

Completed: 2026-07-29 06:18 EDT

## Outcome

**BogKit is not a fit for this firmware storage problem.** `fold` provides
transactional application dataflow on a filesystem-backed `fjall` database. It
does not provide a raw NOR interface, fixed recovery bounds, a `no_std` path, or
a fixed memory envelope. Using it would import the exact filesystem and
allocation assumptions that this trial must avoid.

I kept no BogKit dependency. I built a dependency-free reference journal and
host-side NOR harness in
`runs/2026-07-29--flash-config-journal`. The journal candidate passes the tested
emulator checks at the minimum and maximum record sizes. This is not a
production firmware recommendation: host timing, whole-stack memory, and the
modeled failure behavior still need measurement on the actual controller and
NOR part.

## Prototype

The emulated flash is exactly 131,072 bytes: 32 erase blocks of 4,096 bytes.
Every program checks the NOR 1→0 rule.

Each journal update:

1. Scans the 32 block starts for valid committed records.
2. Rejects a revision that is not newer than the highest valid revision.
3. Selects the circular run immediately after the active record. A maximum
   record uses seven blocks, so it cannot overlap the seven-block active record.
4. Erases only the selected run.
5. Streams the payload in 256-byte chunks while computing CRC-32.
6. Writes a checked 32-byte header.
7. Writes the one-byte commit marker last.

A boot scan ignores an absent/torn commit, a malformed header, and a payload
with the wrong checksum. It selects the highest valid revision. Because the
replacement run never overlaps the active run, each tested interruption sees
the complete old value until the final commit byte, then the complete new value.

The CLI uses a file-backed emulator. The exhaustive crash and wear tests use an
in-memory emulator so that each byte boundary can be inspected deterministically.
Neither emulator claims to reproduce page-program, partial-erase, cache,
controller, or timing behavior of real NOR.

## Acceptance results

| Requirement | Result | Evidence |
| --- | --- | --- |
| Every write-boundary crash boots old or new, never mixed | Pass in emulator | The audit inspected all 6,177 boundaries of a 2 KiB update and all 53,281 boundaries of a 24 KiB update: 59,458 total. No boundary had no valid configuration or an unexpected revision. The commit boundary was the only boundary that selected the new value. CRC verification confirmed the final payload. |
| Reject corrupt and older revisions | Pass in emulator | A deterministic programmed-bit corruption in revision 42 made boot select intact revision 41. A proposed revision 40 returned `StaleRevision`. The CLI independently corrupted revision 101 and recovered complete revision 100. |
| At most 128 KiB flash | Pass | Both emulators enforce exactly 131,072 addressable bytes. The CLI creates a 131,072-byte image. |
| At most 16 KiB working memory | Unresolved; explicit-buffer design budget only | The core streams through 256-byte buffers and declares a conservative 1,024-byte design budget for its explicit buffers. It performs no heap allocation itself. This is not a measured or mechanically enforced whole-stack bound; it excludes the host emulator, caller-owned reader, compiler stack-frame overhead, and driver state. |
| Scan at most 32 erase blocks | Pass | Boot always reads exactly the 32 block starts. The test reports 32. |
| Boot within 50 ms | Pass only in the host emulators; hardware unresolved | Maximum-record in-memory scans varied from 228.875 µs to 1.563 ms. File-backed CLI scans varied from 247.708 µs to 4.253 ms across simulator, reviewer, and archive runs. These host figures do not establish controller timing. |
| 10,000 updates, erase imbalance at most 10% | Pass for fixed 2 KiB updates in the emulator | The 10,000-update test used minimum-size records and produced per-block erase counts of 312–313: 0.32% max-minus-min over mean. Mixed and maximum-size wear were not tested. |
| Avoid filesystem semantics, uncontrolled allocation, and unpredictable encoded size | Pass for journal core; BogKit rejected | The core depends only on `Read` and raw NOR traits, uses fixed buffers, and requires a declared 2–24 KiB encoded length. The host CLI alone uses a file. CBOR encoding is outside the prototype boundary. |

## Single-blob baseline

I evaluated the stated strategy before selecting any candidate. The preserved
test `baseline_single_blob_has_a_boundary_with_no_valid_configuration` writes a
valid revision 1 at block zero, then models power loss after the first byte of
the in-place erase. That byte destroys the only header and boot finds no valid
configuration. A checksum detects the damage but cannot recover the old value.

This establishes the required failure before introducing the journal.

## Ordered discovery and friction trail

1. I read the public root `README.md` first. Its transactional and crash-safe
   description initially sounded relevant, although all public examples were
   application databases.
2. I read `examples/starter/src/main.rs`. Its smallest example creates a
   directory under the host temporary directory and opens `Stream::new` with a
   filesystem path. Fold's crate documentation later described Fjall as
   “embedded,” meaning an in-process database rather than embedded firmware.
3. I also read the timeseries, search, and chat examples. They demonstrate
   durable materialized views, heap-owned strings and vectors, threads,
   networking, and host files. None demonstrates bounded raw storage.
4. I first tried to inspect `fold/src/stream.rs`; that path does not exist. I
   listed `fold/src` and found the implementation split between
   `fold/src/stream/mod.rs`, `unkeyed.rs`, and `keyed.rs`.
5. The source and `fold/Cargo.toml` resolved the fit question. `Stream::new`
   opens `fjall::SingleWriterTxDatabase` from a `Path`; `WriteTx` owns a
   growable `Vec`; serialization enables postcard `use-std`; checkpointing
   calls filesystem persistence.
6. I inspected `scripts/new-project.sh` rather than running it. It would add
   `fold`, `anny`, `ese`, and `serde` to every new example even though this
   trial needed none. I created a dependency-free crate manually.
7. I wrote and ran the single-blob failure reproducer. It confirmed that an
   in-place erase can remove the only valid configuration.
8. I chose a circular whole-record journal rather than adapting `fold`.
9. The first `cargo fmt --check -p flash-config-journal` reported formatting
   diffs. Running the formatter resolved them.
10. The first complete test run passed all five tests.
11. The first warnings-denied Clippy run rejected a test assertion whose value
    was compile-time constant. I moved the memory limit check to a compile-time
    assertion, then reran every check.
12. The final format, lint, test, and CLI runs all passed.

## Findings

### Baseline correctness defect — in-place single-blob update loses the only copy

- **Severity:** Critical
- **Confidence:** High
- **Scope:** Defect in the supplied single-slot baseline, not in BogKit.
- **Evidence:** After a valid revision 1, changing the first header byte to its
  torn-erase state makes the boot scan return no configuration.
- **Reproduction:** `cargo test -p flash-config-journal baseline_single_blob_has_a_boundary_with_no_valid_configuration`
- **Smallest plausible improvement:** Keep the active record untouched while
  writing and checking a replacement; publish the replacement with a final
  one-way commit marker.

### API friction — the documented scaffold adds every major crate

- **Severity:** Low generally; Medium under constrained builds
- **Confidence:** High
- **Evidence:** `scripts/new-project.sh` unconditionally adds `anny`, `ese`,
  `fold`, and `serde`. The flash journal needs none.
- **Reproduction:** Read `scripts/new-project.sh`.
- **Smallest plausible improvement:** Let the script accept a minimal preset or
  ask which components to include.

### Documentation gap — “embedded” is easy to read as embedded-device support

- **Severity:** Medium
- **Confidence:** High
- **Evidence:** Fold's crate documentation calls Fjall “embedded,” while public
  onboarding does not define the operating-system, filesystem, allocator, or
  `std` boundary near its persistence claims. The root README does not claim
  embedded-firmware support.
- **Reproduction:** Start with the root README, then compare it with
  `fold/Cargo.toml` and `fold/src/stream/unkeyed.rs`.
- **Smallest plausible improvement:** Say “in-process, filesystem-backed
  database for `std` targets” and list unsupported firmware constraints.

### Missing capability — no raw NOR or fixed-memory storage layer

- **Severity:** Critical for this use case
- **Confidence:** High
- **Evidence:** The public entry point takes a filesystem `Path`. The write
  transaction uses `Vec<u8>`. No raw read/program/erase trait, `no_std` feature,
  erase-block geometry, commit-byte primitive, wear accounting, or fixed
  recovery-I/O bound appears in the inspected public surface.
- **Reproduction:** Search `README.md`, `fold`, `examples`, and `scripts` for
  `no_std`, `flash`, `NOR`, and allocator guidance; then inspect the stream
  types.
- **Smallest plausible improvement:** Add an explicit “not intended for raw
  flash or `no_std` firmware” boundary now. A real capability would require a
  separate storage engine, not a small adapter.

### Poor product fit — Fold solves a different state problem

- **Severity:** Critical if selected; none if rejected
- **Confidence:** High
- **Evidence:** Fold maintains incremental views over data changes in an LSM
  store. This controller replaces one bounded opaque blob and needs a
  power-fail-safe publication protocol over 32 known erase blocks.
- **Reproduction:** Compare the root README’s Fold description and starter
  example with this trial’s acceptance matrix.
- **Smallest plausible improvement:** Add a use-case boundary to the README.
  Do not market the transactional API as a substitute for a raw-flash journal.

## Decision audit

### Consequential choices

- **No BogKit dependency.** A host filesystem transaction does not become a NOR
  transaction through a thin adapter.
- **Whole-record circular journal.** Updates replace the complete CBOR blob, so
  delta materialization would add complexity without saving the required
  publication step.
- **Variable contiguous runs.** A record occupies one to seven blocks. Moving
  to the run after the active record preserves the active copy and walks wear
  around all 32 blocks.
- **Payload first, header second, commit last.** No pre-commit state is
  bootable. The commit byte is the only publication boundary.
- **CRC-32 and monotonic `u64` revision.** They match the supplied checksum and
  ordering requirements. Signatures are explicitly out of scope.
- **Streaming input.** The core never buffers a 24 KiB configuration. It
  requires the caller to know the final encoded length.

### Rejected alternatives

- **Existing single slot:** rejected by the preserved torn-erase reproducer.
- **Two fixed slots:** atomic publication is simple, but repeatedly erasing the
  same small subset of 32 blocks fails the wear-distribution goal.
- **Fold/fjall:** rejected because it requires filesystem semantics and lacks
  raw-device, memory, scan, and wear bounds.
- **Encoding CBOR inside the journal:** rejected because it couples publication
  to allocator behavior and encoded-size prediction. The candidate accepts an
  already encoded, length-bounded stream.
- **Filesystem crate or extra checksum dependency:** unnecessary for the
  smallest reproducer; the emulator and CRC implementation use the standard
  library only.

### Unresolved uncertainty

- Actual NOR page-program and erase interruption behavior can be less tidy than
  the byte model. The hardware driver must define its guarantees.
- The measured host timings do not prove a 50 ms MCU boot.
- The 1,024-byte value is a source-level design budget for explicit core
  buffers, not a measured or mechanically enforced bound for the final
  compiler's whole stack frame, the input source, or the device driver.
- CRC-32 has collision risk and is not an authenticity check.
- Bad blocks, endurance limits, revision rollover, read-disturb, and post-boot
  background faults were not modeled.
- The file emulator calls host sync operations, but host filesystems do not
  reproduce NOR persistence.
- A production CBOR encoder must provide a bounded final length or a separate
  staging strategy. That encoder was a non-goal here.

## Verification log

Final quality and test command:

```console
cargo fmt -p flash-config-journal && cargo fmt --check -p flash-config-journal && cargo clippy -p flash-config-journal --all-targets -- -D warnings && cargo test -p flash-config-journal --all-targets -- --nocapture && cargo run -p flash-config-journal
```

Observed:

- Formatting check: passed.
- Clippy with warnings denied: passed.
- Tests: 5 passed, 0 failed, completed in 4.43 s.
- Crash boundaries: 6,177 at 2 KiB and 53,281 at 24 KiB.
- Wear: 312–313 erases per block after 10,000 fixed 2 KiB updates, 0.32%
  imbalance.
- In-memory maximum-record boot: 32 blocks in 228.875 µs in the simulator run
  and 1.563 ms in final archive verification.
- File-backed reopen: revision 101, 24,576 bytes, 32 blocks in 247.708 µs in
  the simulator run, 943.917 µs in the reviewer rerun, and 4.253 ms in final
  archive verification.
- Deterministic corruption: revision 101 rejected; complete revision 100
  recovered.
- File image: 131,072 bytes.

No commit, push, GitHub write, automation write, or external repository access
was performed.
