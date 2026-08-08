# Trial report: fastq-barcode-spill

Run date: 2026-08-05 (America/New_York)

Sanitized checkout: `/private/tmp/bogkit-2026-08-05-fastq-CjPkDm`

Starting commit: `80fd3c9a023e877fff2e5d127accca386d437af0`

Trial directory: `trial-fastq-barcode-spill`

## Outcome

The reviewed prototype is a standalone, dependency-free Rust CLI. No BogKit component
fits this workload. Fold maintains durable views for later reads; ANNy and ESE are approximate
search and embedding tools. This problem is instead a bounded, one-pass byte router with no
database read path. Using any of those components would add persistence and state without
solving parsing, Hamming classification, or file-descriptor pressure.

The prototype meets the requested acceptance checks on this test host for the supplied and
generated record shape. The final corrected run streamed 1,000,000 pairs through a pipe
across 384 sample destinations with an even four-way mix of exact matches, unique one-base
corrections, ambiguous ties, and unmatched reads. It completed in 1.93 seconds, used 63,750,144
bytes maximum resident memory (60.8 MiB), and kept at most 24 FASTQ output files open. Two full
runs produced the same output-tree checksum.

The timing and memory result is host- and fixture-specific. The parser currently has no maximum
FASTQ line length, so the 128 MiB claim is not extended to adversarially huge individual records.

## What was built

- `src/main.rs`: validated eight-line paired FASTQ reader for the documented identifier subset,
  absolute line-number errors, paired-ID validation, sequence/quality length validation,
  fixed-length hash-map lookup for exact and unique-Hamming-1
  lookup, bounded LRU append writers, per-destination buffers, counters, and final manifest.
- `baseline.py`: a minimal model of the stated existing Python behavior: exact matching and one
  simultaneously open handle per sample.
- `fixtures/`: small clean, mixed, truncated, unequal-length, and mismatched-ID fixtures, plus a
  deterministic workload generator.
- `tests/integration.py`: baseline comparison, seeded mutation, tie, unmatched, malformed,
  privacy, writer-bound, count, and repeat-checksum checks.
- `scripts/measure_open_files.py`: an independent `lsof` observer for the open-output bound.
- `scripts/verify.sh`: the focused local verification and demo.

All implementation and evidence instructions are confined to this trial directory. No BogKit
core file or public example was changed. No dependency download, GitHub use, commit, or push was
performed.

## Acceptance result

| Requirement | Result | Evidence |
| --- | --- | --- |
| Clean output byte-identical to baseline | Pass | All four sample files and `unmatched.fastq` compared equal byte-for-byte; hashes recorded below. |
| Seeded one-substitution correct | Pass | Seed `20260805` produced a one-base mutation routed to the expected sample; integration test also verifies output bytes. |
| Hamming-distance-one ties ambiguous | Pass | `GAAAAAAAAA` is one base from each of two whitelist entries and is written to `ambiguous.fastq`. |
| Counts cover complete pairs | Pass | Mixed million-pair manifest classifications sum to 1,000,000; sample counts sum to the 500,000 exact plus corrected pairs. |
| Truncation error, no manifest | Pass | Exit 1, `line 8`, and no `manifest.json`. |
| Unequal sequence/quality error, no manifest | Pass | Exit 1, `line 4`, and no `manifest.json`. |
| Mismatched pair IDs error, no manifest | Pass | Exit 1, `line 5`, and no `manifest.json`. |
| At most 24 sample files open | Pass | Internal maximum 24; corrected `lsof` sampling observed 24 across 45 successful polls. |
| 1M pairs, 384 samples, under 30s | Pass on this host | Final corrected run: 1.93 seconds real time, streamed through a pipe. |
| Under 128 MiB | Pass on this host and fixture | Final corrected run: 63,750,144-byte maximum resident set (60.8 MiB). |
| Repeated output and manifest checksums identical | Pass | Both corrected 387-file output trees hashed to `88e0f8be6e3f5622b19d4cf530c1af20a2f69ce8ece18c3bf347a466e22cd644`. |
| No read/sample data in logs | Pass for exercised errors | Integration asserts that IDs, sequence, quality, and a sample name do not occur in malformed-run stderr. Normal stdout contains counts only. |
| Completion manifest only after validation | Pass | Malformed runs leave partial FASTQ outputs but no completion manifest; valid runs atomically rename the completed manifest after writer flush and count checks. |
| Output filename uniqueness | Pass after review fix | ASCII-case-folded sample aliases and `Ambiguous`/`Unmatched` reserved aliases fail before output creation. |
| Supported identifier validation | Pass after review fix | Empty/control-byte identifiers and contradictory slash/CASAVA roles fail; ordinary CASAVA and CRLF inputs pass. |

## Ordered discovery and friction trail

1. Read the public root `README.md`. Its normal path is `./scripts/new-project.sh`, which creates
   an example crate and adds Fold, ANNy, ESE, and Serde dependencies.
2. Read public examples in the README's order: `starter`, `timeseries`, `chat`, then `search`.
   `starter` shows durable count/bag views; `timeseries` keyed aggregates; `chat` a durable source
   of truth with snapshot broadcasting; `search` BM25/HNSW indexes. None has a role in a
   single-pass FASTQ byte router.
3. Read `scripts/new-project.sh`. It always creates under `examples/` with all three local BogKit
   dependencies. The brief requires a unique top-level directory and says not to change public
   examples, so the trial was created manually and opted out of the parent Cargo workspace.
4. Searched this sanitized checkout for `fastq`, `barcode`, `demultip`, `fixture generator`, and
   `python cli`; there was no existing FASTQ baseline artifact. Built the smallest reference
   baseline described by the brief before selecting a BogKit component.
5. Reproduced the trial-created baseline model's file-descriptor failure at 384 samples with a
   64-file process limit. This illustrates the brief's stated design but does not verify the
   unavailable production baseline and is not a BogKit defect.
6. The first clean fixture accidentally ended with an extra blank line. The baseline treated it
   as the start of a truncated pair. Removed the blank line and added exact line-count fixtures.
   This was a trial-fixture defect caught before making comparison claims.
7. The first parser version reported record-relative lines for malformed pairs after the first.
   Absolute-line tests exposed the risk; the parser now carries the pair's starting line into all
   validations. This was a prototype defect fixed before final measurement.
8. A naive correction path scanned all whitelist entries for every non-exact barcode. It was
   functionally correct but an unnecessary scaling risk. Replaced it with a precomputed map of
   every A/C/G/T/N one-base neighbor, where collisions are marked ambiguous.
9. Raw bounded LRU writers would reopen a file on nearly every pair under round-robin sample
   input. Added 64 KiB per-destination buffers before the 24-entry LRU; the final million-pair
   mixed run needed 1,952 file-open events rather than one per pair while preserving order.
10. The first piped benchmark tried to create and consume the barcode map concurrently, so the
    consumer exited before input. Added the generator's `--emit-only` mode and made map creation
    an explicit prior command. This was benchmark-fixture friction, not a prototype result.
11. Sandboxed `/usr/bin/time -l` could report elapsed time but could not read the macOS kernel RSS
    counter (`sysctl kern.clockrate: Operation not permitted`). Re-ran the same bounded command
    with permission to read resource counters. This was test-environment friction.
12. Ran the mixed million-pair measurement twice, reconciled manifest counts, and hashed every
    output filename and byte. The tree checksums matched.
13. Skeptical review reproduced all headline results but found that `Alpha`/`alpha` and
    `Ambiguous`/`ambiguous.fastq` could alias on the host filesystem, allowing a complete manifest
    to describe mixed output classes. The loader now rejects ASCII-case-folded filename
    collisions before creating the output directory, with exact integration regressions.
14. Review also showed accepted empty/control-byte identifiers and contradictory slash/CASAVA
    roles, plus an `lsof` observer that false-passed when every observation failed. The parser now
    rejects those identifiers without echoing them, and the observer requires a successful,
    positive sample. A forced-failure observer regression and the real 45-poll check both pass.
15. The corrected suite, real descriptor measurement, and two million-pair runs were rerun. The
    final resource observation and repeat checksum are the values reported here.

## Commands and observed results

Commands below were run from `trial-fastq-barcode-spill` unless stated otherwise.

### Focused Rust checks

```console
$ cargo test
running 5 tests
test tests::exact_unique_one_error_and_tie_are_distinct ... ok
test tests::malformed_inputs_report_expected_line ... ok
test tests::pair_reader_preserves_original_bytes ... ok
test result: ok. 5 passed; 0 failed

$ cargo fmt --check
# exit 0, no output

$ cargo clippy --all-targets -- -D warnings
Finished `dev` profile ...

$ cargo build --release
Finished `release` profile ...

$ python3 tests/integration.py target/release/fastq-barcode-spill
integration checks passed
```

The integration command exercises the Python comparison, seeded correction, barcode-level
ambiguity (including same-sample barcode ties), unmatched routing, malformed content and
identifier cases, case-folded filename collisions, no-manifest behavior, privacy assertions, a
30-sample writer cap of 3, and repeat checksums.

### Demo

```console
$ target/release/fastq-barcode-spill --barcodes fixtures/barcodes.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/demo-final \
    < fixtures/mixed.fastq
processed 4 read pairs: exact 1, corrected 1, ambiguous 1, unmatched 1; max open output writers 3
```

The manifest reported `total_pairs: 4`, one pair in each classification, two pairs for the
`gamma` destination, `max_open_writers: 3`, and `complete: true`.

### Baseline behavior and clean comparison

The descriptor-limit reproduction was:

```console
$ python3 fixtures/generate.py --samples 384 --pairs 0 \
    --barcodes /private/tmp/fastq-spill-evidence-cUbEIn/barcodes-384.tsv
$ ulimit -n 64
$ python3 baseline.py \
    --barcodes /private/tmp/fastq-spill-evidence-cUbEIn/barcodes-384.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/baseline-384
error: could not open all output files
# exit 1
```

Clean comparison commands:

```console
$ python3 baseline.py --barcodes fixtures/barcodes.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/baseline-clean-final \
    < fixtures/clean.fastq
$ target/release/fastq-barcode-spill --barcodes fixtures/barcodes.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/rust-clean-final \
    < fixtures/clean.fastq
processed 2 read pairs: exact 2, corrected 0, ambiguous 0, unmatched 0; max open output writers 2
```

| Compared file | Equal | SHA-256 |
| --- | --- | --- |
| `alpha.fastq` | yes | `c3c28d466dfecbddcf0e6abca023b5608bd21968ef055529e35a95eb88d3f98a` |
| `beta.fastq` | yes | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `gamma.fastq` | yes | `f53385669a88200f5cff6b896154e366f64b8e0c714d2b35d59b2ab547363493` |
| `delta.fastq` | yes | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `unmatched.fastq` | yes | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |

The baseline has no ambiguity output or completion manifest, so those Rust-only artifacts are
not part of the clean byte comparison.

### Malformed input

```console
$ target/release/fastq-barcode-spill --barcodes fixtures/barcodes.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/bad-truncated \
    < fixtures/truncated.fastq
error: line 8: truncated interleaved FASTQ pair
# exit 1

$ target/release/fastq-barcode-spill --barcodes fixtures/barcodes.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/bad-unequal \
    < fixtures/unequal.fastq
error: line 4: sequence and quality lengths differ
# exit 1

$ target/release/fastq-barcode-spill --barcodes fixtures/barcodes.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/bad-mismatched \
    < fixtures/mismatched.fastq
error: line 5: paired read identifiers do not match
# exit 1

$ find /private/tmp/fastq-spill-evidence-cUbEIn/bad-truncated \
    /private/tmp/fastq-spill-evidence-cUbEIn/bad-unequal \
    /private/tmp/fastq-spill-evidence-cUbEIn/bad-mismatched \
    -name manifest.json -print
# no output
```

### Independent output-handle observation

```console
$ python3 scripts/measure_open_files.py
observed_max_open_fastq_files=24 manifest_max_open_writers=24 polls=45 successful_polls=45
```

This used a throttled 200,000-pair non-seekable pipe and counted only `.fastq` descriptors under
that run's output directory. The code also structurally refuses a `--max-open` value above 24.

### Full numeric workload

The generated whitelist has 384 entries. Two entries are Hamming distance two to provide a known
tie midpoint; every other entry is at least distance three from all others. The mixed input cycles
exact, unique one-substitution, tie, and unmatched cases.

```console
$ python3 fixtures/generate.py --samples 384 --pairs 0 --well-spaced \
    --barcodes /private/tmp/fastq-spill-evidence-cUbEIn/mixed-barcodes.tsv

$ python3 fixtures/generate.py --samples 384 --pairs 1000000 --well-spaced \
    --mixed --emit-only \
    --barcodes /private/tmp/fastq-spill-evidence-cUbEIn/mixed-barcodes.tsv |
  /usr/bin/time -l target/release/fastq-barcode-spill \
    --barcodes /private/tmp/fastq-spill-evidence-cUbEIn/mixed-barcodes.tsv \
    --out /private/tmp/fastq-spill-evidence-cUbEIn/million-mixed-a
processed 1000000 read pairs: exact 250000, corrected 250000, ambiguous 250000, unmatched 250000; max open output writers 24
        1.93 real         0.44 user         0.10 sys
            63750144  maximum resident set size
```

`/usr/bin/time` wrapped only the Rust consumer. Its real time includes any wait for the separate
Python producer, so the 1.93-second result is conservative for this generated pipe. The output
tree occupied 198,692 KiB on disk.

The same generator and CLI command was repeated to `million-mixed-b`. A streaming SHA-256 over
each sorted filename and file contents produced:

```text
million-mixed-a: 387 files, 88e0f8be6e3f5622b19d4cf530c1af20a2f69ce8ece18c3bf347a466e22cd644
million-mixed-b: 387 files, 88e0f8be6e3f5622b19d4cf530c1af20a2f69ce8ece18c3bf347a466e22cd644
```

Each manifest reported 1,000,000 classified pairs, 500,000 sample-routed pairs, 1,952 output-file
open events, and a maximum of 24 open writers.

## Findings by source

| Source | Finding | Severity | Confidence | Reproduction | Smallest improvement |
| --- | --- | --- | --- | --- | --- |
| Modeled Python baseline | The trial-created all-open model fails under a lower descriptor limit at 384 samples. This illustrates the stated design; it does not verify the unavailable production artifact. | High | High for the model | Set `ulimit -n 64` and launch the local model with the 384-sample map. | Use a bounded writer pool; buffering destinations avoids reopen-per-record cost. |
| Modeled Python baseline | The trial-created exact-only model sends a one-substitution sequencing error to unmatched. | High | High for the model | The seeded integration pair differs by one base from exactly one whitelist barcode. | Precompute exact and unique one-error lookup tables. |
| BogKit fit | Fold, ANNy, and ESE do not address this streaming transformation. This is a valid no-fit, not a defect. | Informational | High | Compare the public examples' durable view/search paths with the no-read-path brief. | No component change. Keep the CLI standalone. |
| BogKit onboarding | `new-project.sh` always creates under `examples/` and adds all three local components, even when none fit. | Low | High | Read or run the script with a disposable name. It has one fixed template. | Document a standalone/no-component route or add opt-in dependency flags. |
| Trial prototype | The measured acceptance workload passes, including all four classification paths. | Informational | High | Run `scripts/verify.sh`, the workload command, and the open-file observer. | None for the one-day boundary. |
| Trial prototype | A single pathological FASTQ line can grow memory beyond the measured fixture envelope because line length is not capped. | Medium | High | Feed an extremely long line and observe allocation before validation. Not run because it would not alter the stated fixture result. | Add a bounded line reader and a documented maximum record length. |
| Trial prototype | Malformed input leaves partial FASTQ files, deliberately without a completion manifest. Consumers must use the manifest as the completion sentinel. | Low | High | Run any malformed fixture and inspect its output directory. | Document the sentinel contract; optionally clean failed run directories when the caller explicitly permits deletion. |
| Trial prototype | Output files are flushed before manifest publication but are not individually `fsync`ed, so power-loss durability is not proven. | Medium | High | Requires filesystem fault/power-loss injection; not run. | Sync each touched output and the output directory before publishing the manifest if crash durability is required. |
| Trial prototype | Sample names are deliberately limited to safe ASCII filename characters and 120 bytes. Truly arbitrary sample labels are not accepted. | Low | High | Put whitespace or a path separator in column two of the map. | Separate opaque sample labels from sanitized output IDs if such labels are required. |
| Trial prototype | Case-insensitive output aliases could mix samples or the reserved ambiguity stream while the manifest claimed separation. Fixed after review. | High | High | Use `Alpha` and `alpha`, or sample `Ambiguous`, on this host. | Reject ASCII-case-folded collisions before output creation; retain regressions. |
| Trial prototype | Empty/control-byte IDs and contradictory slash/CASAVA roles were accepted. Fixed after review. | Medium | High | Use the archived integration cases. | Validate the supported identifier token and reconcile both role conventions. |
| Evidence script | A failing `lsof` command produced zero observations but exited successfully. Fixed after review. | Medium | High | Set `LSOF_COMMAND=/usr/bin/false`; the corrected script exits nonzero. | Require at least one successful, positive observation. |
| Trial fixtures | Extra trailing blank line and map-creation race initially invalidated test commands; both were fixed before final evidence. | Informational | High | Described in the ordered trail; current integration and piped commands pass. | Retain exact fixtures and `--emit-only` separation. |
| Test environment | Sandboxed `time -l` could not read kernel RSS counters. This was not a product failure. | Informational | High | Run the first timing command without resource-counter permission on this host. | Grant read access to the timer counters or use another process RSS observer. |

## Decision audit

1. **No BogKit runtime dependency.** The deciding evidence was the public examples plus the
   baseline's actual failure mode. No component provides FASTQ framing or bounded fan-out.
2. **Dependency-free Rust.** The standard library covers buffered stdin, file append, maps, JSON
   emission, and atomic rename. This avoids downloads and keeps the trial reproducible.
3. **Precomputed correction index.** Each whitelist barcode contributes every one-base A/C/G/T/N
   neighbor. First insert is unique; any second barcode marks the neighbor ambiguous, even if both
   barcodes name the same sample. Exact lookup takes precedence. This directly encodes the
   barcode-level tie rule.
4. **64 KiB destination buffers plus 24-entry LRU.** LRU alone bounds handles but performs poorly
   for round-robin 384-sample data. Buffers reduce churn while using roughly 24 MiB at 384
   destinations, inside the measured memory budget.
5. **Sequential, single-threaded processing.** It naturally preserves within-sample order and was
   already far under the timing target. Parallel classification would add ordering machinery.
6. **New-or-empty output directory.** Refusing existing contents prevents accidental append or
   truncation and makes repeated checks unambiguous. Case-folded output aliases are rejected
   before the directory is created.
7. **Completion manifest last.** All parser checks, writer flushes, and count invariants finish
   before a temporary manifest is synced and renamed to `manifest.json`.
8. **Original bytes retained.** Lines include their input line endings in output buffers, while
   validation uses newline-stripped slices. A CRLF unit test confirms preservation.

## Rejected choices

- **Fold as a spool or counter store:** rejected because it introduces durable state and a second
  representation of data without eliminating output FASTQ files; an uncompressed input spool is
  explicitly out of scope.
- **ANNy or ESE for matching:** rejected because ten-base Hamming distance is exact discrete
  matching, not semantic or approximate vector search.
- **All sample files open:** reproduces the baseline failure.
- **LRU append with no destination buffering:** bounded but risks nearly one open/close per pair
  for round-robin samples.
- **One uncompressed temporary input copy:** violates the single-pass constraint.
- **Compressed outputs or a shared container:** would change baseline bytes and is a non-goal.
- **Third-party argument, JSON, LRU, or FASTQ crates:** unnecessary for the one-day prototype and
  could require uncached downloads.
- **Deleting partial outputs automatically on malformed input:** not required by acceptance and
  is a consequential behavior; the manifest is the explicit success boundary instead.

## Unresolved uncertainty and narrowed claims

- The largest run was the required 1,000,000-pair numeric workload, not the maximum stated
  10,000,000-pair stream. No ten-million-pair time, disk, or RSS claim is made.
- The 128 MiB result applies to the generated 40-base read 1 and 40-base read 2 records. Without a
  line-length cap, it does not cover adversarially large individual FASTQ records.
- Corrected `lsof` sampling can miss short transients, but its 45 successful polls and observed
  maximum agree with the
  writer-pool invariant: opening entry 25 always flushes and drops the least-recently-used entry
  first.
- The maximum applies to FASTQ output files. Standard input, stdout/stderr, and short-lived map or
  manifest descriptors are outside the "sample files open" requirement.
- The manifest's atomic rename and file flush behavior was tested normally, not under disk-full,
  I/O-fault, process-kill, or power-loss injection.
- Optional FASTQ conventions beyond the exercised `/1` and `/2` and matching Illumina whitespace
  role forms may need broader production fixtures. Conflicting dual roles are rejected.
- Filenames expose safe sample names by design, as the existing per-sample convention implies.
  The program does not print them, but filesystem metadata and the manifest contain them.

## Coordinator rerun

The disposable `/private/tmp/fastq-spill-evidence-cUbEIn` outputs may not survive. The durable
reproduction entry points are in this trial directory:

```console
./scripts/verify.sh
python3 scripts/measure_open_files.py
```

For the full numeric run, use the exact two generator commands and timed pipeline in the README.
On a sandboxed macOS runner, `/usr/bin/time -l` may need permission to read kernel resource
counters; the functional command itself does not.
