# Performance evidence

## Method

Both implementations were built once with the same Cargo release profile and invoked through the same binary over the same procedurally generated, shuffled 12,000,000-record corpus. Both modes are single-threaded. Each mode received one unmeasured warm-up, followed by three `/usr/bin/time -l` runs. The comparison uses the slower wall time from each series, as required. `maximum resident set size` is the macOS process peak in bytes.

The sandboxed `time -l` call could not query `kern.clockrate`, so the measured series was rerun outside the sandbox; no network or external input was used.

## Exact baseline

Command:

```console
/usr/bin/time -l /private/tmp/bogkit-reach-repair-target/release/reach-rollup-lab bench-exact
```

Every run reported 12,000,000 records, 5,760 buckets, 10,800,000 unique bucket occurrences, and digest `6efde7109d2ddba5`.

| Run | Wall (s) | Peak RSS (bytes) |
|---:|---:|---:|
| 1 | 1.00 | 640,663,552 |
| 2 | 0.99 | 647,315,456 |
| 3 | 1.00 | 625,754,112 |

Slower wall time: **1.00 s**.

## Candidate

Command:

```console
/usr/bin/time -l /private/tmp/bogkit-reach-repair-target/release/reach-rollup-lab bench-candidate
```

Every run reported 12,000,000 records, 5,760 buckets, a 4,124-byte maximum bucket state, and v2 complete-binding state digest `ecda69b5ba1467d1`.

| Run | Wall (s) | Peak RSS (bytes) |
|---:|---:|---:|
| 1 | 0.55 | 49,627,136 |
| 2 | 0.55 | 49,627,136 |
| 3 | 0.55 | 49,610,752 |

Slower wall time: **0.55 s**.

## Gate calculations

- Candidate peak RSS: 49,627,136 bytes = 47.33 MiB, below 128 MiB.
- Conservative memory ratio using the smallest measured exact peak and largest candidate peak: 625,754,112 / 49,627,136 = **12.61× lower**, above the required 4×.
- Slower-run wall ratio: 0.55 / 1.00 = **0.550×**, below the maximum 1.5×.
- No cross-language timing was used.

## Toolchain and machine

- `rustc 1.95.0 (59807616e 2026-04-14)`, LLVM 22.1.2, `aarch64-apple-darwin`.
- macOS 26.5.2 (25F84), Darwin 25.5.0, model `Mac16,11`.
- 14 physical / 14 logical cores, 64 GiB memory.

## Evidence limit

The available host is 14-core rather than the requested four-core machine. Both implementations are single-threaded, so extra cores did not accelerate either measured path, but this is not a literal four-core-host reproduction. The 128 MiB criterion is supported by measured process RSS, not by running inside a hard 128 MiB container limit.
