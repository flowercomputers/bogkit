# Evidence ledger — safe-policy incompatibility handoff

## Scope and environment

- Trial checkout: `/private/tmp/bogkit-sim-2026-08-17-trial1.KyfY2d` at detached BogKit revision `20f2ca5`.
- Every retained write is under `simulation-output/`.
- Prototype: independent Rust child workspace in `simulation-output/prototype`, standard library only.
- Build target used during repair: `/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target` (removed before handoff).
- Toolchain: `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`.
- Host: Mac mini `Mac16,11`, Apple M4 Pro, 14 cores, 64 GB RAM, arm64 Darwin 25.5.0.
- Deterministic seed: `0xA17E2026`.
- No network or external crate was used.

## Protocol and independent oracle

Wire format: `02 | payload_length:u16 BE | sequence:u32 BE | payload | CRC32(sequence || payload) | 03`; exact wire length is `payload_length + 12`.

CRC-32/ISO-HDLC uses reflected polynomial `0xEDB88320`, initial `0xFFFFFFFF`, final XOR `0xFFFFFFFF`.

- `"123456789" -> 0xCBF43926` passes.
- Sequence `0x01020304` with empty payload has CRC `0xB63CFBCD`; exact frame `02 00 00 01 02 03 04 B6 3C FB CD 03` passes.
- The whole-frame oracle has a separately written branch-form CRC loop and rejects trailing bytes, marker damage, and CRC damage.

## Review verification and strict RED/GREEN record

The second repair began by reading the complete available skeptical review and the receiving-review, test-driven-development, good-test, and verification instructions. Commands used the external target shown above.

### Clean outer frame with valid-looking nested payload

Permanent test: `candidate_never_emits_valid_nested_payload_before_outer_is_proved`.

Fixture: a clean outer frame for sequence 42 whose opaque payload is `encode_frame(777, "aa")` followed by 32 bytes. The outer payload is 46 bytes and its wire length is 58 bytes.

RED command:

```sh
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target cargo test candidate_never_emits_valid_nested_payload_before_outer_is_proved -- --nocapture
```

Observed failure: whole-chunk delivery emitted the valid outer sequence 42 at end offset 57. One-byte delivery emitted only nested sequence 777 at end offset 20; its frame record differed from the expected outer record.

Minimal GREEN change: remove opportunistic nested-frame emission while an accepted-length containing candidate is incomplete. The decoder now waits until the containing candidate either proves valid or reaches its declared end and fails.

GREEN: whole-chunk and one-byte delivery both emit only sequence 42 with byte-identical frame records and no diagnostics.

### Plausible false header and two short followers

Permanent test: `candidate_defers_nested_followers_until_plausible_header_fails`.

Fixture:

```text
02 00 13 | encode_frame(101, "aa") | encode_frame(102, "bb")
```

Each follower is 14 bytes; the 31-byte false containing candidate does not reach its declared end until byte index 30. After the safe-policy implementation, the old immediate expectation correctly failed:

```text
left: [30]
right: [16, 30]
```

The retained safe-policy assertion requires no emission at sequence 101's ETX, byte index 16. After the false candidate fails, both sequences 101 and 102 are emitted on the call receiving byte index 30. Aggregate frames and the single privacy-safe `Noise` diagnostic at offset 0 are identical under single-chunk and one-byte delivery.

This is not an implementation gap hidden by the test: the clean outer case proves that emitting at byte 16 can corrupt a valid containing frame. Opaque-payload correctness and unconditional immediate nested recovery are incompatible without additional wire information.

### Other retained recovery coverage

- `candidate_preserves_nested_start_after_plausible_header_fails` uses `02 00 13` followed by two ordinary 192-byte valid frames and verifies chunk-identical recovery after the false candidate actually fails.
- Every garbage-class event in the damaged workload uses the under-limit plausible header `02 00 13`; the required four damage totals remain unchanged.
- Frame and diagnostic trace digests are separate and compared across whole-frame, one-byte, and seeded irregular schedules.

## Final static and automated gates

```sh
cargo fmt --all --check
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target cargo test --all-targets
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target cargo clippy --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
CARGO_TARGET_DIR=/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target cargo build --release
```

Results:

- Formatting: exit 0, no diff.
- Tests: 19 library tests passed, 0 failed; binary target passed with 0 tests.
- Strict Clippy: exit 0, no warnings.
- Release build: succeeded.

Covered behavior includes exact CRC/wire layout, every split, empty/1/8,180 payload acceptance, 8,181/65,535 rejection, embedded markers, adjacent frames, wrapping/duplicate/decreasing sequences, bad CRC/marker, changed payload with old CRC, 64-stream isolation, one malicious one-byte stream, 50 schedules, both incompatible-prefix regressions, scaled clean/damaged workloads, property bounds, and identical-byte parser benchmarking.

## Minimized baseline comparison

```sh
/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target/release/instrument-decoder-trial demo
```

Observed:

- Modeled append/search baseline: 0 frames, 1 reset, peak retained 39 bytes.
- Standalone candidate: emitted follower sequence 32, one `Marker` diagnostic for damaged sequence 31, retained 0 bytes.

This is a modeled gateway-baseline finding, not a BogKit finding.

## Full clean and damaged corpus plus current-build RSS

Command, executed outside the restricted sandbox so macOS could provide RSS:

```sh
/usr/bin/time -l /private/tmp/bogkit-sim-2026-08-17-trial1-round2-target/release/instrument-decoder-trial full
```

Configuration: 64 connections; exactly 1,000,000 valid frames; exactly 950,000 x 192-byte plus 50,000 x 4,096-byte frames; exactly 387,200,000 clean bytes. Damage total remained exactly 25,000: 10,000 CRC flips, 5,000 over-limit headers, 5,000 changed markers, and 5,000 garbage bursts. All 5,000 garbage bursts used the under-limit `02 00 13` plausible header and nested follower STX.

### Clean schedules — pass

| Schedule | Frames | Tuple digest | Trace digest | Diagnostics | Max retained | Peak buffered | Time |
|---|---:|---|---|---:|---:|---:|---:|
| whole-frame | 1,000,000/1,000,000 | `0x227BAFFB59534865` | `0xED1C4875E1055725` | 0 | 0 | 4,096 | 6.416454 s |
| all-one-byte | 1,000,000/1,000,000 | same | same | 0 | 4,095 | 4,096 | 11.409391 s |
| seeded irregular 1..4,096 | 1,000,000/1,000,000 | same | same | 0 | 4,095 | 4,096 | 6.377592 s |

Oracle and candidate tuple digests were identical on every clean schedule. Removing the unsafe nested scan reduced the one-byte clean time from the round-one 283.566192 seconds to 11.409391 seconds. The old performance finding is resolved and is not retained as a current finding.

### Damaged schedules — immediate-recovery gate fails

Every schedule emitted 0 damaged candidates, but only 48,942 of 50,000 known-valid followers. The same 48,942 were observed on the final feed call for their own scheduled frame. Candidate trace digest `0x6E17FB57A361D391` and diagnostic digest `0x0952BE767C57CA87` were identical across schedules.

| Schedule | Followers emitted / expected | Candidate max retained / peak | Modeled baseline resets | Modeled baseline max retained | Time |
|---|---:|---:|---:|---:|---:|
| whole-frame | 48,942 / 50,000 | 5,990 / 6,182 | 4,928 | 8,367 | 0.215143 s |
| all-one-byte | 48,942 / 50,000 | 6,072 / 6,073 | 3,246 | 58,418 | 8.705880 s |
| irregular | 48,942 / 50,000 | 5,990 / 6,182 | 4,938 | 35,698 | 0.219024 s |

Candidate followers emitted by preceding injected class were `[19,544, 9,776, 9,774, 9,848]`, versus expected `[20,000, 10,000, 10,000, 10,000]`, on every schedule. Total known-valid follower loss was 1,058.

Candidate diagnostic counts, identical across schedules: 9,778 CRC, 5,168 over-limit, 4,850 marker, and 10,588 noise; 30,384 total. A damage class does not promise a one-to-one diagnostic class because payload marker bytes and long-lived connection state can establish an earlier conservative recovery candidate. No diagnostic contains payload bytes.

These generator results do not rescue the exact immediate gate: the permanent 14-byte regression proves sequence 101 cannot safely emit on byte 16. Conversely, the full generator shows that conservative recovery after corruption can lose later valid frames in long-lived streams. The brief's immediate-follower acceptance criterion is unmet.

### RSS

Current safe-policy build maximum resident set size: **6,144,000 bytes** (about 5.86 MiB), below 64 MiB. `time -l` also reported peak memory footprint 5,505,336 bytes, 0 swaps, and 0 block input/output operations.

## Required arbitrary-byte property-style run

```sh
/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target/release/instrument-decoder-trial property 100000
```

Result: exactly 100,000 deterministic arbitrary strings, lengths 0 through 32,768; largest 32,768; no panic or hang; peak buffered 8,192; maximum retained 8,175; elapsed 15.249824 seconds; digest `0xF063F47065F8AF00`.

This is boundedness and panic/hang stress evidence. It is not an independent framing oracle or a coverage-guided fuzzing campaign.

## Parser-only benchmark

The runner prebuilt one 10,000-frame, 3,872,000-byte corpus before timing. Each parser received exactly those stored bytes in fixed 4,096-byte chunks for ten passes per measured run, after one warm-up. Generation and frame encoding were outside measured time.

```sh
/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target/release/instrument-decoder-trial bench 100000
```

| Parser | Run 1 | Run 2 | Run 3 | Slower of three |
|---|---:|---:|---:|---:|
| modeled baseline | 126.388 MiB/s | 126.589 MiB/s | 126.464 MiB/s | 126.388 MiB/s |
| standalone candidate | 135.806 MiB/s | 135.072 MiB/s | 135.002 MiB/s | 135.002 MiB/s |

Slower-run ratio: 1.068161 (106.816%). Both emitted 100,000 frames, zero diagnostics/resets, and digest `0x4BF9177355C9C665`. The throughput gates pass for this clean fixed-chunk microbenchmark.

## Evidence limits

- The full corpus is deterministic and synthetic; it does not establish real-instrument production readiness.
- Host RSS is measured on this 64 GB M4 Pro Mac, not under an enforced 64 MiB container.
- The parser benchmark is a clean per-connection stored-byte microbenchmark, not a full gateway capacity test.
- The property runner checks determinism/counts/bounds and absence of panic/hang; it does not supply an independent oracle for arbitrary bytes.
- The wire format has no escape or external boundary signal that can prove whether a nested valid byte sequence is payload or a transport frame before a plausible containing frame ends.
- No BogKit component internals were exercised beyond the public surfaces needed for the fit decision.

## Cleanup verification

- Removed `/private/tmp/bogkit-sim-2026-08-17-trial1-round2-target` after the final gates.
- No `target` directory, generated corpus, database, runtime output, or binary remains under `simulation-output/`.
- `git diff -- . ':(exclude)simulation-output'` is empty; no tracked BogKit core, example, Git, GitHub, or automation file changed.
- The retained handoff contains only the brief, small Markdown evidence, Rust source/tests, `Cargo.toml`, and `Cargo.lock`.
