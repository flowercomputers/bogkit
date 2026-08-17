# Corruption-resynchronizing instrument stream decoder

## Developer role and Rust experience

You maintain the device-ingestion side of a laboratory automation gateway. You have nine years of production C and C++, have completed the Rust Book, and have written two small internal Rust command-line tools but no production protocol parser in Rust. You have no prior knowledge of the candidate library being evaluated; begin from this problem statement rather than assuming that any library component should be used.

## Existing system and baseline implementation

The gateway maintains long-lived TCP connections to laboratory instruments. Each connection carries a proprietary framed binary stream. The current C++ parser appends bytes to a per-connection vector, searches for an end marker, and then validates the declared length and CRC. It is easy to operate and fast on clean traffic, but an absent end marker can grow the vector, and a corrupt length or marker often forces the gateway to discard the entire connection buffer and reconnect.

For this trial, model each wire frame as:

`STX (0x02) | payload_length (u16 big-endian) | sequence (u32 big-endian) | payload | CRC32(sequence || payload) | ETX (0x03)`

The wire length is therefore `payload_length + 12` bytes. Use CRC-32/ISO-HDLC over `sequence || payload`: reflected polynomial `0xEDB88320`, initial value `0xFFFFFFFF`, final XOR `0xFFFFFFFF`, with `"123456789"` checking to `0xCBF43926`. A frame is valid only when both its exact declared wire length and markers agree and its CRC passes.

The runnable baseline is a small Rust translation of the existing append-and-search approach. It is not expected to reproduce unrelated gateway behavior.

## Concrete pain

One noisy device can cause memory growth, repeated reconnects, and loss of valid frames that follow a damaged frame. Operators cannot tell whether a sequence gap came from the instrument, transport damage, or parser recovery. The replacement needs a bounded incremental decoder that consumes arbitrary read chunks, emits only proved-valid frames, records privacy-safe diagnostics, and resumes at the earliest defensible next frame boundary.

## Realistic workload and data shape

Use a deterministic generator for 64 interleaved connection streams:

- 1,000,000 valid frames total.
- 950,000 frames are exactly 192 wire bytes, with 180-byte payloads.
- 50,000 frames are exactly 4,096 wire bytes, with 4,084-byte payloads.
- Valid wire data is therefore exactly 387,200,000 bytes before damage is added.
- The harness supplies chunks from 1 through 4,096 bytes, including an all-one-byte schedule, a seeded irregular schedule, and boundaries immediately before and after every header field, CRC, and marker.
- Inject exactly 25,000 damaged candidates: 10,000 CRC bit flips, 5,000 declared lengths above the accepted limit, 5,000 missing or changed end markers, and 5,000 garbage bursts containing misleading marker bytes. Every injected case is followed by at least two known-valid frames.

Payloads are opaque bytes and may contain either marker value. Sequence numbers are connection-local unsigned values and may wrap; the parser reports them but does not decide whether a gap is operationally acceptable.

## Operational constraints

- Accept wire frames no larger than 8,192 bytes; reject larger declared lengths before allocating their claimed size.
- Keep total peak resident memory at or below 64 MiB for the 64-stream harness.
- In-progress accepted-frame storage is bounded by `64 × 8,192 = 524,288` bytes, plus fixed parser, output-channel, and runtime overhead.
- Make no blocking network, disk, logging, or clock calls in the byte-consumption path.
- Never panic, abort, read out of bounds, or loop without consuming input on any byte sequence.
- Preserve per-connection order. State from one connection must not affect another.
- Diagnostics may contain connection ID, byte offset, declared length, sequence when safely decoded, and error class, but not payload bytes.
- The same bytes and chunk schedule must produce byte-identical frame metadata and diagnostics on repeat runs.

## Measurable acceptance criteria

1. On the clean corpus, the candidate emits exactly the same 1,000,000 `(connection, sequence, payload digest)` tuples as an independent whole-frame oracle, under every chunk schedule.
2. On the damaged corpus, it emits no damaged candidate as valid and emits every known-valid frame that begins after the declared recovery boundary. For each of the 25,000 injected cases, it emits the immediately following known-valid frame as soon as that frame's end marker has arrived; it may not skip that frame while searching for a later boundary.
3. The all-one-byte schedule, maximum-size frames, empty payload, payload-embedded markers, adjacent frames, and sequence wrap all pass without special harness exceptions.
4. Peak resident memory is at most 64 MiB. Retained per-stream input after each call never exceeds 8,192 bytes.
5. Clean-input throughput is at least 80% of the Rust baseline on the same machine and build profile, and at least 50 MiB/s. Report the slower of three runs after one warm-up.
6. Fifty deterministic chunk schedules produce the same output digest as the single-chunk oracle.
7. A property or fuzz-style test processes at least 100,000 generated arbitrary byte strings, each capped at 32 KiB, with no panic, hang, or retained-buffer violation.

## Explicit non-goals

- No TCP server, reconnect policy, TLS, authentication, device discovery, or production deployment.
- No payload schema decoding or business interpretation.
- No correction, reordering, or synthesis of sequence numbers.
- No disk persistence or replay log.
- No zero-copy requirement if bounded copying satisfies the measured gates.
- No attempt to recover bytes from inside a frame whose length and CRC have not both been validated; conservative loss is preferable to fabricating a frame.

## Smallest self-contained prototype boundary

Build one Rust workspace containing: the deterministic stream generator, a whole-frame oracle used only for expected results, the append-and-search baseline, the incremental candidate decoder, a 64-stream interleaving harness, diagnostics with stable error classes, and tests or a CLI that runs the clean, damaged, chunk-boundary, memory, and throughput checks. The candidate decoder API need only accept `(connection_id, byte_chunk)` and return zero or more validated frame records and diagnostics. Do not integrate it with the actual gateway.

## Baseline comparison

Run the baseline and candidate on identical generated bytes and chunk schedules. Report clean-output equality, valid frames retained after each corruption class, reconnect-or-buffer-reset count, peak resident memory, throughput, and maximum retained bytes per connection. The candidate is acceptable only if it preserves clean correctness, meets the recovery and memory gates, and does not fall below the stated throughput floor. Do not claim improvement from asymptotic reasoning alone; include measurements.

## Fault and adversarial cases

- A header split at every possible byte boundary.
- `STX` and `ETX` values repeated throughout the payload.
- Declared payload lengths of 0, 1, 8,180, 8,181, and 65,535.
- Correct markers with wrong CRC; correct CRC bytes attached to a changed payload.
- Missing end marker followed immediately by a valid frame.
- Garbage containing plausible headers, nested start markers, and long marker-free runs.
- A connection ending after each individual header, payload, CRC, or marker byte.
- One malicious stream receiving only one byte at a time while the other 63 carry clean maximum-size frames.
- Duplicate, decreasing, and wrapping sequence values; these must be surfaced without changing framing decisions.
- Arbitrary-byte property cases and a regression seed for every discovered panic, hang, false frame, or recovery miss.

## Evidence expected

Provide the exact build and run commands, toolchain and machine description, generator seed, CRC definition and vectors, the output digest for each chunk schedule, a table by corruption class, property-test count and elapsed time, peak-memory method and result, three timed baseline and candidate runs, and the maximum observed retained bytes per stream. Include at least one minimized adversarial example showing how the old baseline loses or over-retains data and how the candidate behaves. State any unmet criterion plainly; a clean no-fit conclusion is valid evidence.
