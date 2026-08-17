# Trial report: corruption-resynchronizing instrument stream decoder

## Outcome

**no_fit**

Fold, ESE, and ANNy are not a fit for the synchronous exact binary-framing path. The safe standalone decoder also cannot satisfy the full brief: unconditional immediate recovery from a plausible false header is incompatible with treating clean frame payloads as opaque.

The retained standard-library prototype is a minimal runnable incompatibility and conservative-recovery reproducer, not a production candidate. It passes clean framing, bounded memory, property stress, parser throughput, and chunk-independence checks. It explicitly fails the immediate-follower gate.

## Ordered discovery and friction

1. Public BogKit documentation identifies Fold as persistent incremental dataflow, ESE as text embedding, and ANNy as approximate vector search.
2. Fold's public `Stream` opens a path-backed fjall store and commits transactions. That machinery does not help exact byte framing and cannot sit in this no-disk consumption path.
3. ESE and ANNy are unrelated to opaque bytes and exact marker/length/CRC proof.
4. The modeled append/search baseline loses already-buffered valid data after some corrupt candidates.
5. The initial standalone candidate safely waited for an accepted-length containing candidate to end, but therefore could not emit a short valid follower immediately after `02 00 13`.
6. The round-one repair emitted the earliest later CRC-valid nested candidate before the containing candidate ended. It satisfied the immediate regression but made clean framing chunk-dependent.
7. The round-two clean counterexample is sequence 42 with opaque payload beginning with a complete valid encoded sequence 777 plus 32 bytes. Whole-chunk delivery emitted sequence 42; the unsafe one-byte path emitted sequence 777 and lost 42.
8. Strict RED/GREEN removed only the opportunistic nested emission. Whole-chunk and one-byte delivery now both emit sequence 42 with no diagnostics.
9. The exact false-header regression now waits: sequence 101 does not emit at its ETX on byte 16; sequences 101 and 102 both emit only after the plausible containing candidate fails at byte 30. Frames and diagnostics remain chunk-identical.
10. The full safe-policy damage corpus emits 0 damaged candidates but only 48,942/50,000 known-valid followers. The immediate gate is unmet, so the full-brief and candidate outcome is `no_fit`.

## Component decision audit

| Component | Considered role | Decision | Reason |
|---|---|---|---|
| Fold | Durable privacy-safe diagnostic aggregation | Not used | Useful only downstream of validation. Its path-backed transactions add disk/database machinery and do not solve framing or missing boundary information. |
| ESE | Payload/diagnostic semantic classification | Not used | Payloads are opaque, diagnostics cannot expose payload, and semantic text processing is out of scope. |
| ANNy | Boundary similarity search | Not used | Recovery requires exact transport proof, never approximate similarity. |

No BogKit component was forced into an unrelated hot path. A separate downstream Fold consumer remains possible but is outside this trial.

## Safe recovery contract

- Emit only a candidate whose own STX, exact declared length, ETX, and CRC pass.
- Treat the payload of an accepted-length candidate as opaque while that candidate is incomplete; never emit a nested valid-looking byte sequence early.
- For a valid containing candidate, emit only that outer frame even if its payload contains complete valid encoded frames.
- After a containing candidate reaches its declared end and fails, preserve the earliest nested STX strictly inside its declared bytes and wait for that nested candidate's own proof.
- A valid frame starting exactly at a failed candidate's declared boundary remains a normal follower; the failed candidate keeps its stable CRC/marker diagnostic.
- Reject over-limit headers from the three-byte prefix without allocating their claimed length.
- Diagnostics contain connection, offsets, safe header metadata, and error class only—never payload bytes.

## Why the immediate gate is unachievable under this wire contract

When a complete CRC-valid frame-shaped sequence appears inside an incomplete accepted-length candidate, bytes that have not arrived yet determine whether the containing candidate will validate. If it validates, the nested sequence is opaque payload and must not emit. If it fails, the nested sequence may be a recovery boundary. The decoder cannot know which future occurs at the nested ETX.

The unsafe round-one policy guessed “boundary” and corrupted the clean sequence-42 frame under one-byte delivery. The safe round-two policy waits and therefore cannot emit sequence 101 on byte 16 in the plausible-false-header case. No heuristic resolves missing information while preserving both contracts.

The smallest protocol-level improvement is an unambiguous boundary mechanism outside opaque payload—such as byte stuffing/escaping or trusted record segmentation. The smallest requirements-level improvement is to permit recovery only after an accepted-length containing candidate validates or fails.

## Acceptance results

| Criterion | Result | Evidence |
|---|---|---|
| 1. Full clean equality | Pass | All three schedules emitted 1,000,000/1,000,000 over 387,200,000 bytes; oracle/candidate digest `0x227BAFFB59534865`; trace `0xED1C4875E1055725`. |
| 2. Full damaged immediate recovery | **Fail** | Exact 25,000 events including 5,000 under-limit plausible headers; 0 damaged emitted, but only 48,942/50,000 followers emitted on every schedule. The 14-byte sequence-101 follower is deliberately deferred from byte 16 to byte 30. |
| 3. Edge and adversarial execution | Pass with incompatibility exposed | 19 tests cover both conflicting cases, every split, length edges, markers, CRC, adjacency, wrap/duplicates/decrease, and 64-stream isolation. The tests do not relabel the failed immediate requirement as a pass. |
| 4. <=64 MiB RSS and <=8,192 retained | Pass on current host | Fresh maximum RSS 6,144,000 bytes; full candidate retained <=4,095 clean and <=6,072 damaged; property max 8,175. |
| 5. >=80% baseline and >=50 MiB/s | Pass in parser microbenchmark | Slower baseline 126.388 MiB/s; candidate 135.002 MiB/s; ratio 106.816%. |
| 6. Deterministic schedules | Pass for resulting conservative policy | Targeted frames/diagnostics match across single, one-byte, and 50 seeded schedules; full outputs and losses match across the three named schedules. |
| 7. 100,000 arbitrary strings | Pass | Exactly 100,000 up to 32 KiB; no panic/hang; retained <=8,175; digest `0xF063F47065F8AF00`. |

Detailed commands, timings, follower counts, diagnostics, and RED/GREEN output are in `EVIDENCE.md`.

## Findings

### F1 — opportunistic nested emission corrupted clean opaque payload

- Category: correctness defect
- Severity: Important
- Confidence: High
- Attribution: standalone round-one prototype; not a BogKit core defect
- Status: Fixed in this handoff
- Reproduction: feed a clean sequence-42 frame whose payload starts with `encode_frame(777, "aa")` followed by 32 bytes. The rejected one-byte policy emitted sequence 777 and lost 42; whole-chunk delivery emitted 42.
- Smallest improvement: never emit a nested candidate while an accepted-length containing candidate is incomplete; retain the exact whole/one-byte regression.

### F2 — the wire contract lacks information required for unconditional immediate resynchronization

- Category: missing capability
- Severity: Important
- Confidence: High
- Attribution: wire-protocol and brief boundary; not the modeled baseline, BogKit, or a BogKit core defect
- Status: Open; causes the `no_fit` outcome
- Reproduction: compare the clean outer regression with `02 00 13 | encode_frame(101, "aa") | encode_frame(102, "bb")`. Emitting a nested candidate before the plausible container ends breaks the clean case; waiting prevents sequence 101 from emitting on byte 16 in the false-header case.
- Smallest improvement: relax immediate recovery until a plausible containing candidate validates or fails.

### F3 — append/search reset loses already-buffered valid data

- Category: correctness defect
- Severity: Important
- Confidence: High
- Attribution: modeled gateway baseline; not the standalone candidate and not a BogKit core defect
- Status: Demonstrated
- Reproduction: run the release `demo`; the modeled baseline clears a changed-marker frame and its valid follower, while the conservative candidate emits the follower.
- Smallest improvement: replace connection-wide reset with per-connection incremental validation and explicit recovery boundaries.

### F4 — BogKit components do not fit exact synchronous framing

- Category: poor product fit
- Severity: Important
- Confidence: High
- Attribution: BogKit product-fit finding; not a BogKit core defect
- Status: Retained no-fit decision
- Reproduction: compare the decoder's exact no-disk byte contract with Fold's persistent transaction stream, ESE's text embeddings, and ANNy's approximate vector index.
- Smallest improvement: keep framing standalone; use Fold only as an optional downstream consumer of already validated privacy-safe diagnostic events.

### F5 — root onboarding does not make the valid use-none path explicit

- Category: documentation gap
- Severity: Minor
- Confidence: High
- Attribution: BogKit documentation; not a BogKit core defect
- Status: Open
- Reproduction: start from the root README with a binary protocol parser problem. Component descriptions exist, but a newcomer must infer from examples that using none is correct.
- Smallest improvement: add a compact capability matrix with explicit no-fit examples such as binary framing, cryptographic validation, and synchronous network parsing.

Every retained finding uses exactly one allowed category and a Critical/Important/Minor severity. Evidence limits are not findings. The resolved round-one one-byte scanning cost is recorded in evidence rather than retained as a current performance finding.

## Evidence limits

- The full corpus is deterministic and synthetic; it does not establish real-instrument production readiness.
- RSS is a host measurement on an M4 Pro Mac, not an enforced 64 MiB container.
- The property runner is boundedness/no-panic stress, not an independent oracle or coverage-guided fuzz campaign.
- The fixed-chunk parser benchmark is not a gateway capacity claim and cannot offset the failed immediate-recovery gate.
- BogKit internals were not broadly audited; no BogKit core correctness defect is claimed.

## Dependencies and handoff decision

- Standard library only; no external dependency.
- Independent child workspace and lockfile under `simulation-output/prototype`.
- Generator seed `0xA17E2026`.
- Exact reproduction commands in `prototype/README.md`.

Decision: retain outcome **no_fit** for Fold, ESE, ANNy, and the standalone candidate against the full brief. Preserve the prototype only as an incompatibility and conservative-recovery reproducer. Do not adopt it in the gateway unless the wire contract or immediate-recovery requirement changes and the resulting design repeats all correctness, memory, and throughput gates.
