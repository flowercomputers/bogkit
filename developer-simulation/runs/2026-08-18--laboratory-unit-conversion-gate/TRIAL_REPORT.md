# Trial 1 repaired report: exact laboratory-unit conversion admission gate

## Stable handoff

- **Repair round:** 1 of 5, addressing the complete initial skeptical review.
- **Fit decision:** `no_fit` for Fold, ESE, and ANNy; unchanged by repair.
- **Prototype:** `simulation-output/lab-unit-gate/`
- **Baseline-first discovery and repair trail:** `simulation-output/DISCOVERY.md`
- **Runnable instructions:** `simulation-output/lab-unit-gate/README.md`
- **Frozen source identity:** `simulation-output/TREE_MANIFEST.sha256`
- **BogKit HEAD:** `20f2ca50d5d06f51edfe8b8570c0fb48caf9eb81`
- **Tracked repository files changed:** none.
- **Admission decision:** not proven. The repaired prototype closes the review's publication and evidence-labeling defects, but measured peak memory still exceeds 256 MiB and authoritative Java/signed-fixture/target-Linux evidence remains unavailable.

## Repair verdict

Every Trial 1 reviewer finding was addressed without adding a BogKit component:

| Review item | Category | Prior severity | Current severity | Repaired evidence |
|---|---|---|---|---|
| T1-I1 output could overwrite immutable input | correctness defect | Important | None | Direct observation, resolved reference, existing hard-link identity, and sibling-temporary alias regressions now reject before temporary creation and preserve input bytes. |
| T1-I2 relative output failed after publication | correctness defect | Important | None | Bare relative CLI output now succeeds; the directory handle is opened before temporary creation and retained through rename/sync. A post-rename failpoint returns a distinct published-but-durability-uncertain state. |
| T1-I3 benchmark false-passed hard/missing gates | correctness defect | Important | None | Output now separates `run_status:"complete"` from `acceptance:"unverified"` or `"failed"`; no acceptance `pass` exists. A known over-limit unit regression is retained. |
| T1-I4 absent authority was called a product finding | documentation gap | Important | None | Missing Java, signed fixtures, reason codes, and target runner are now only Evidence limits. |
| T1-M1 60 files duplicated one parser class | documentation gap | Minor | None | The 60 padding files were removed and replaced with one named unknown-field fixture with one exact expected outcome. |

The remaining open finding is the already-disclosed million-row memory failure, normalized in the Findings section.

## Repaired publication boundary

Before any sibling temporary file is removed or created, the runner derives both final and temporary paths and compares each with all four inputs:

- ordinary path equality;
- resolved canonical equality, including `..` components and symlinks when resolvable;
- existing Unix file identity using device and inode, covering hard links.

Aliases return `OUTPUT_ALIASES_INPUT` or `OUTPUT_TEMPORARY_ALIASES_INPUT`, publication state `NotPublished`, and leave input bytes unchanged. The adjacent temporary-path check also prevents stale-temp cleanup from deleting an immutable input.

The output parent is normalized from an empty relative parent to `.`, opened before temporary creation, and retained. The temporary report is written, flushed, file-synced, and renamed. The retained directory handle is then synced.

Prepublication errors remain ordinary `RunError` values with publication state `NotPublished`; the prior report is preserved by the retained duplicate-ID, read/write, alias, row failpoint, and pre-rename failpoint tests. A directory-sync failure after successful rename cannot make that claim. It is returned as `POST_RENAME_DURABILITY_UNCERTAIN` with `PublishedDurabilityUncertain`; the post-rename regression proves the new final report is already visible.

## Test-driven repair audit

The repair followed observed RED/GREEN cycles:

1. direct observation output alias returned success and replaced its input; the new regression failed, then passed after preflight alias rejection;
2. resolved reference alias and an existing hard-link identity returned success; retained regressions now pass with unchanged input bytes;
3. an input at the sibling temporary path was deleted/replaced by cleanup; the adjacent regression now passes with the input untouched;
4. the relative-path CLI regression failed with `DIRECTORY_SYNC_ERROR` after the report appeared; it now succeeds after `.` normalization and early directory open;
5. the new post-rename state API first failed to compile, then the failpoint returned success; it now returns the distinct durability-uncertain error and state after publication;
6. the benchmark integration test failed because `run_status` and `acceptance` were absent and `status:"pass"` remained; it now emits complete/unverified on this host and no `status` field;
7. the known-over-limit test first lacked the assessment function, then reproduced `pass`; it now returns `failed` for any internally measured peak over 268,435,456 bytes and `unverified` when authority is incomplete;
8. the single named unknown-field fixture regression first failed because the meaningful file was absent; the 60 same-class files were removed and the exact one-file regression passed.

No production code path was added before its corresponding reproducer failed.

## Complete verification

All commands ran from `simulation-output/lab-unit-gate/`.

```text
cargo test --offline --locked
```

Result: **20 tests passed, 0 failed**, plus doc tests. Coverage includes:

- exact decimal grammar, checked rational transforms, positive/negative half-even boundaries, and 20,000 independent slow-reference comparisons;
- 1,024-byte bounded line buffering, invalid UTF-8, structural/reference faults, forced read/write errors, duplicate IDs, and one named retained unknown-field fixture;
- direct/resolved/existing-identity/temporary alias probes with byte preservation;
- relative output success and explicit prepublication versus post-rename state;
- injected exits at 1 percent, 50 percent, pre-rename, and post-rename/pre-directory-sync;
- fixed-seed generation, five shuffles, three repeats, canonical digest checks, and CLI benchmark semantics.

```text
cargo fmt --all -- --check
```

Result: pass; no formatting difference.

```text
cargo clippy --offline --locked --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic
```

Result: pass; warnings, all, and pedantic lints denied.

```text
cargo build --offline --locked --release
```

Result: pass.

Static production review found no `f32`, `f64`, floating-point parse, unsafe block, or subprocess use. Both production roots retain `#![forbid(unsafe_code)]`.

## Release demo

```text
target/release/lab-unit-gate generate /private/tmp/lab-unit-gate-repair1.yHyxPR 9500 250 250 42
target/release/lab-unit-gate bench /private/tmp/lab-unit-gate-repair1.yHyxPR /private/tmp/lab-unit-gate-repair1.yHyxPR/report.ndjson
```

Result:

```json
{"acceptance":"unverified","converted":9750,"elapsed_ms":32,"missing_gates":["signed_fixture_parity","java_comparison","declared_two_core_linux"],"output_digest":"256be30a4b0845bb13d4e999587042cdd60f9c6512f2ffc1e8da8a54379b71a0","peak_rss_bytes":null,"rejected":250,"run_status":"complete","total":10000}
```

The run completed successfully; acceptance remains unverified rather than passing.

## Full one-million-row evidence

Available host: `Darwin arm64`, Rust/Cargo 1.95.0. It is not the declared two-core Linux target.

Seed 42 generated exactly 250 units, 300 analytes, 12,000 mappings, 950,000 ordinary conversions, 25,000 boundary conversions, and 25,000 expected rejections. Observation digest remained `f0474b988a85d6548a0f9df3900fcf55c9a022435a4eb3abd5fd25ac7fd0e473`.

```text
/usr/bin/time -l target/release/lab-unit-gate bench /private/tmp/lab-unit-gate-repair1.yHyxPR /private/tmp/lab-unit-gate-repair1.yHyxPR/report.ndjson
```

Machine-readable output:

```json
{"acceptance":"unverified","converted":975000,"elapsed_ms":1186,"missing_gates":["signed_fixture_parity","java_comparison","declared_two_core_linux"],"output_digest":"b9c8adf790c0717818cdaaeb83d5bbb612aa3780eb60e4eba3d97732c1446eb5","peak_rss_bytes":null,"rejected":25000,"run_status":"complete","total":1000000}
```

External macOS evidence: 1.19 seconds real and 291,176,448 bytes maximum resident size (about 277.7 MiB). This exceeds 268,435,456 bytes by 22,740,992 bytes, about 8.5 percent. Because the program cannot observe the external macOS measurement, its acceptance remains `unverified`; it no longer claims `pass`. On Linux, an internally observed over-limit `VmHWM` produces `acceptance:"failed"`.

Input observations were 140,975,000 bytes. The final report was 93,065,000 bytes, about 66.0 percent of input. Output digest remained `b9c8adf790c0717818cdaaeb83d5bbb612aa3780eb60e4eba3d97732c1446eb5`.

## Full-shape shuffle evidence

The repaired release binary ran five full one-million-row shuffles with seeds 11, 22, 33, 44, and 55, followed by three further full seed-11 repeats. Internal elapsed times were 1310, 1189, 1100, 1087, 1095, 1097, 1111, and 1099 milliseconds.

All eight runs produced exactly:

- 975,000 conversions;
- 25,000 rejections;
- 1,000,000 report rows;
- 93,065,000 report bytes;
- SHA-256 `b9c8adf790c0717818cdaaeb83d5bbb612aa3780eb60e4eba3d97732c1446eb5`;
- `run_status:"complete"` and `acceptance:"unverified"`, never acceptance `pass`.

This closes the local full-shape determinism gap. It does not establish Java parity or target qualification.

## Benchmark state semantics

- `run_status:"complete"`: parsing, evaluation, and atomic rename completed.
- `acceptance:"failed"`: at least one applicable internally measured hard gate failed; currently this is emitted for peak RSS above 256 MiB on Linux.
- `acceptance:"unverified"`: no measured hard failure was observed, but one or more required authorities are absent.
- `missing_gates`: explicitly names signed-fixture parity, Java comparison, and declared two-core Linux execution.

The program has no acceptance `pass` state while these gates are missing.

## Evidence limits

The following were not supplied and are not prototype or BogKit findings:

- frozen Java evaluator and authoritative byte output;
- signed fixtures and authoritative rejection-code spellings;
- the supplied 60-file negative corpus;
- the declared offline two-core Linux machine/harness;
- the same-machine 100,000-row Java timing.

The one retained negative text fixture is explicitly a candidate-authored unknown-field example, not a substitute for the absent corpus. Other negative cases are created narrowly by tests with exact expected codes.

Memory/timing figures are single-host macOS observations. The deterministic failpoints are returned errors, not operating-system kills, disk-full, permission changes, or power-loss qualification. The post-rename regression establishes lifecycle labeling, not filesystem durability under power loss.

## Component consideration and fit

| Component | Evidence considered | Decision |
|---|---|---|
| Fold | Persistent Fjall-backed incremental views, transactional changes, and consistent snapshots. | `no_fit`: an immutable exact batch still owns arithmetic, bounded sorting, duplicate detection, input identity, and file publication. |
| ESE | Floating-point text embeddings. | `no_fit`: unrelated to exact numeric conversion and mapping. |
| ANNy | Approximate HNSW vector search. | `no_fit`: incompatible with exact fail-closed mapping keys. |

No BogKit component was used or modified. None of the repaired issues is a BogKit defect.

## Decision audit

| Decision | Evidence | Residual uncertainty |
|---|---|---|
| Reject final and temporary input aliases before publication | Direct, `..`-resolved, hard-link, and adjacent temporary regressions preserve source bytes. | Adversarial filesystem changes racing after preflight are outside this offline immutable-input prototype. |
| Retain parent directory handle before rename | Relative CLI output passes; post-rename state is explicit. | Power-loss durability remains unqualified. |
| Separate completion from acceptance | Integration and over-limit tests prevent acceptance `pass`. | macOS peak remains externally measured and therefore machine acceptance stays unverified. |
| Keep one meaningful fixture | One exact unknown-field case remains; generated tests own other candidate-authored cases. | Supplied negative corpus remains absent. |
| Keep in-memory canonical sort | Smallest transparent implementation and stable output bytes. | Peak memory still fails the gate. |
| Preserve `no_fit` | Repairs concern application publication/evidence semantics, not component capability. | No adoption conclusion without authoritative evidence. |

## Findings

### F-01 — million-row peak memory exceeds the admission gate

- **Category:** performance problem
- **Prior severity:** Important
- **Current severity:** Important
- **Confidence:** high
- **Reproduction:** build release, generate `950000 25000 25000 42`, then run the README's full benchmark under `/usr/bin/time -l` on macOS or the supplied harness when available.
- **Evidence:** repaired seed-42 binary used 291,176,448 bytes maximum resident memory for exactly 1,000,000 observations; the reviewer independently reproduced the same class at 291,667,968 bytes.
- **Retained reproduction pointer:** `simulation-output/lab-unit-gate/README.md`, “Full synthetic shape,” and `simulation-output/lab-unit-gate/src/lib.rs::run`.
- **Smallest improvement:** write bounded sorted report runs and merge them by observation ID while detecting adjacent duplicates, replacing both the full row vector and ID hash set. This remains database-free and should fit within the 512 MiB temporary-storage allowance.

This is the only retained current finding. It uses exactly one allowed category.

## Cleanup and freeze proof

The final handoff retains source, tests, Cargo manifest/lockfile, README, discovery/report text, one small negative fixture, and `TREE_MANIFEST.sha256`. Build `target/`, generated corpora/reports, runtime databases, temporary output, binaries, and the external repair evidence directory were removed after verification.

`git diff --exit-code` passes, so tracked BogKit files are unchanged. Final Git status contains only untracked `BRIEF.md` and `simulation-output/`. The checksum manifest freezes every retained output file other than itself for the scoped re-review.
