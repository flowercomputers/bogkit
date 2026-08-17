# Trial 2 report: mergeable unique-installation reach rollups

## Outcome

**Outcome: `no_fit`.**

**Execution status: `DONE_WITH_CONCERNS`.** The standalone candidate passes the requested correctness, accuracy, merge, state-size, measured-memory, and runtime gates on the available host. No BogKit component improves the required portable external shard-merge boundary enough to justify adoption.

The remaining evidence concern is environmental: the available Mac has 14 cores rather than four, although both benchmark paths are single-threaded, and 128 MiB was checked by measured peak RSS rather than enforced as a hard container limit.

## Decision

The smallest useful prototype is a standard-library-only, 4,096-register HyperLogLog-style summary. Each bucket's nested state is 4,124 bytes and includes sketch version, precision, hash identity, checksum, and registers. The outer state file is version 2 and adds a checksum over its complete binding: magic, outer version, reserved bytes, bucket count, every tenant/hour key, and every nested state byte. Decoding verifies outer length and checksum before constructing or returning any rollup.

The candidate is suitable only for the brief's advisory immutable daily rollup. It is not exact and must not be used for billing, access, fraud, quotas, privacy, or contractual counts.

### Component decision

- **Fold:** positive-only `Aggregate` updates can express per-key HLL insertion for this immutable subset. Its aggregate contract is not incompatible. Fold does not, however, demonstrate the required portable export/import and register-wise merge of independently produced shard states without raw records. It also adds persistent `fjall` storage and transaction machinery that the self-contained batch proof does not need. Outcome: no fit.
- **ESE:** text embedding is unrelated to fixed-width installation-ID cardinality. Outcome: no fit.
- **ANNy:** nearest-neighbor search is unrelated to set cardinality. Outcome: no fit.

## Acceptance results

| Gate | Corrected result | Evidence |
|---|---:|---|
| Tuning matrix, 2,880 rows: median / p95 / worst absolute error | **0.858% / 2.671% / 5.626% — pass** | `evidence/accuracy-tuning.tsv` |
| Held-out matrix, 2,880 rows: median / p95 / worst | **0.858% / 2.708% / 6.124% — pass** | `evidence/accuracy-heldout.tsv` |
| Full load records / exact unique bucket occurrences | **12,000,000 / 10,800,000 — pass** | `evidence/summary.txt` |
| Full load canonical rows | **5,760 exactly once — pass** | `evidence/load-report.tsv` |
| Full-load median / p95 / worst error | **0.802% / 2.328% / 6.321%** | `evidence/summary.txt` |
| Direct vs eight-shard state | **byte-identical — pass** | corrected state digest `ecda69b5ba1467d1` |
| 50 seeded full-load shard merge orders | **all identical — pass** | summary + release test |
| Duplicates, 1,000 replays, and self-merge | **byte-identical — pass** | tests + summary |
| Two complete repeated state/report runs | **byte-identical — pass** | repeated flags; summary digest `ca4d7c625e0eec4d` |
| Outer key-to-state binding | **v2 checksum rejects valid-window tenant/hour mutations — pass** | `tests/state_file.rs` |
| Nested version/hash/length/checksum/register validation | **pass** | serialization tests |
| Maximum nested bucket state | **4,124 bytes — pass** | 36 bytes below 4,160-byte cap |
| Total nested bucket state | **23,754,240 bytes — pass** | below 23,961,600-byte cap |
| Candidate peak RSS | **49,627,136 bytes (47.33 MiB) — pass** | corrected three-run series |
| Conservative memory improvement | **12.61× — pass** | smallest exact peak / largest candidate peak |
| Slower-of-three wall ratio | **0.55 / 1.00 = 0.550× — pass** | below 1.5× ceiling |
| Invalid/truncated/trailing input and invalid publication | **rejected; prior report preserved — pass** | release tests |
| Sequential/prefix/one-byte/reverse/skew cases | **pass** | adversarial test + summary |

Bias remains visible rather than being folded into absolute error. The held-out matrix has 1,548 nonnegative observations with mean `+1.0418%` and 1,332 negative observations with mean `-1.1276%`. The load corpus has 2,895 nonnegative buckets with mean `+0.9668%` and 2,865 negative buckets with mean `-0.9513%`.

## Ordered discovery and decision audit

1. The exact Rust map of sets remained the result oracle. It is correct but measured 596.8–617.3 MiB peak RSS in the corrected three-run series.
2. Register-wise maximum was selected because it makes state union associative, commutative, and idempotent; duplicate insertion and self-merge preserve bytes.
3. Public entry points made ESE and ANNy immediate no-fits. Fold remained plausible enough to inspect its keyed aggregation, transaction, table, serialization, and storage surfaces.
4. Fold can perform positive-only HLL updates for this immutable subset. The supported no-fit is narrower: no demonstrated portable independently produced partial-state merge, plus unnecessary persistence and transaction machinery.
5. Source inspection shows `Aggregate` buffers cloned per-key updates until transaction commit. This was not benchmarked through Fold, a caller could use smaller positive-only transactions, and no batch-sizing or mergeable-state API conclusion is promoted from this single source. It is decision context, not a retained finding or product candidate.
6. The specified accuracy matrix exposed transition bias during development. That matrix is labeled tuning; a different fixed generator seed was then run without further estimator changes and is retained separately as held-out evidence.
7. The initial skeptical review found that v1 nested checksums did not bind outer tenant/hour keys. A test-first v2 repair now checks the complete file binding before decoding any row. Every affected full-scale state, digest, demo, and benchmark result was regenerated.
8. Full-scale 50-order proof uses temporary shard-state files so eight full rollups are not retained simultaneously. The harness removes its temporary directory on return.

## Findings

### F1 — v1 state files allowed valid-range key rebinding

- Category: **correctness defect**
- Severity: **Important**
- Confidence: **High**
- Attribution: standalone prototype defect; not a BogKit defect.
- Reproduction: serialize one bucket for tenant 0/hour 0 inside a window permitting tenant 1 and hour 1, mutate the unprotected v1 outer tenant or hour bytes, and decode. The initial reviewer observed successful decoding because only nested sketch bytes were checksummed. The retained regression performs both valid-window mutations against v2 and requires rejection.
- Smallest improvement: version and checksum the complete outer key-to-state binding, and verify it before constructing a rollup. **Implemented in v2 and covered by regression tests.**

### F2 — BogKit does not satisfy the portable external shard-merge boundary

- Category: **poor product fit**
- Severity: **Important**
- Confidence: **High**
- Attribution: scenario-specific BogKit fit; not a BogKit correctness defect.
- Reproduction: independently aggregate the eight immutable shards, then attempt to use the demonstrated Fold interfaces to export bounded portable per-bucket HLL states and merge them by register-wise maximum in another process without replaying raw IDs. The public examples and inspected surfaces do not demonstrate that boundary. Positive-only local Fold aggregation itself is possible.
- Smallest improvement: keep this prototype standalone and dependency-free. No new BogKit merge primitive or product candidate is proposed from this one trial.

### F3 — External partial-state merge capability is unclear at public entry points

- Category: **documentation gap**
- Severity: **Important**
- Confidence: **High**
- Attribution: BogKit documentation signal; not a BogKit correctness defect.
- Reproduction: start at the root README and starter/timeseries examples and determine whether independently produced bounded accumulator states can be exported, compatibility-checked, and merged without raw records. Source inspection is required and no supported recipe is presented.
- Smallest improvement: extend the existing capability-matrix documentation theme to state clearly whether append-only aggregation, retraction, external partial-state merge, deterministic portable serialization, and per-key state bounds are supported. Do not create a new one-source merge/batching candidate.

No Critical finding was observed. The only prototype correctness defect was F1 and is repaired. No BogKit correctness defect was established.

## Failure semantics verified after the repair

- Negative, overflowing, or out-of-day hours and out-of-range tenants fail before forwarding a record.
- Unknown record version, nonzero reserved bytes, truncation, and trailing bytes fail the run.
- State-file v2 rejects changed outer version, reserved metadata, bucket-count/length mismatch, file-checksum mismatch, valid-window tenant/hour bit mutations, duplicate/noncanonical keys, nested unknown version, wrong hash identity, nested checksum mismatch, impossible register values, and truncated nested payload.
- Complete-file integrity is verified before a rollup is constructed or returned.
- Candidate/oracle bucket mismatch prevents publication.
- Final report publication uses a validated temporary file and same-directory rename; invalid input and simulated interruption preserve the earlier complete report.
- Empty input and one bucket remain defined.

## Accuracy history and evidence limits

The specified matrix influenced the empty-register transition threshold and is therefore tuning evidence, not independent validation. The held-out matrix uses a different fixed seed in the same generator family; it passed without further change, but it is not a production-distribution guarantee or independently verifiable preregistration. The addressed tuning history is not a remaining finding.

Other limits:

- Re-run on the intended four-core container image with an enforced 128 MiB limit before production adoption.
- Validate real export ID distributions and tenant skew without changing the frozen regression cases.
- The corpus is generated procedurally and never retained. Reader failure semantics are tested separately; timing compares identical generation plus aggregation, not filesystem throughput.
- Outer and nested FNV checksums detect accidental corruption; they are not cryptographic authentication, privacy, or anonymity mechanisms.
- Same-process interruption and same-filesystem rename do not prove process-kill, directory-fsync, power-loss, or target-filesystem durability.
- The prototype intentionally excludes late events, deletion, sliding windows, online service behavior, and distributed coordination.

## Dependencies and reproducibility

- Standard library only; no third-party dependency and no network.
- Toolchain: Rust 1.95.0, LLVM 22.1.2, `aarch64-apple-darwin`.
- Hash seed: `0xbb67ae8584caa73b`.
- Load generator seed: `0x243f6a8885a308d3`.
- Tuning generator seed: `0x13198a2e03707344`.
- Held-out generator seed: `0x082efa98ec4e6c89`.
- State-file version: 2.
- Exact commands are retained in `README.md`, `TDD_LOG.md`, and `evidence/PERFORMANCE.md`.

## Corrected retained-evidence hashes

- `accuracy-tuning.tsv`: SHA-256 `76452306e95ef91b6bbf40faf48fc0047f3ff819353b81d4ad6d2bfb16eab204` (2,880 data rows).
- `accuracy-heldout.tsv`: SHA-256 `d93663b394eb5c1d7f02a4e5bd12ee57068fa9080120a1088a833adaf727d1ca` (2,880 data rows).
- `load-report.tsv`: SHA-256 `e57a1e1f2b94241ed9cf9a1401c6b2c8d2fba7a376e55716d8d4741e02537ed2` (5,760 data rows; unchanged because estimates did not change).
- `summary.txt`: SHA-256 `b6f2a6710edda01fc030c4ba06ec18ae6deba478ad1f3eba1b8af68bf1d70906`.
- `PERFORMANCE.md`: SHA-256 `105ecf633b6265a3b806a3b8b1194f30edbbb05bc829133a8d92bd4aab77b775`.
- Internal v2 full state digest: `ecda69b5ba1467d1`.
- Internal canonical report digest: `6219b8fa82ef6a79`.
- Internal repeated summary digest: `ca4d7c625e0eec4d`.

## Cleanup proof

The procedural 12-million-record corpus is never written. Successful full-scale runs remove temporary shard states. Final cleanup removes external Cargo targets and demo directories. Only source, tests, documentation, the isolated lockfile, and compact evidence remain under `simulation-output/`; no BogKit core, examples, Git, GitHub, or automation file is changed.
