# Acceptance evidence

## Finishing criteria used for this trial

The prototype was considered complete when it could read and validate the two
input files, return only a fully replayed proposal or a no-moves review result,
compare against the stated baseline, demonstrate stable output, exercise every
named placement rule, run a bounded dense workload, and pass tests, formatting,
and strict linting. The production acceptance claim was deliberately held back
because the 30 stated evaluation snapshots and C# results were not supplied.

## Results by requested criterion

1. **Legal intermediate states:** Passed on the included synthetic outputs. The
   separate transition path replayed every emitted move in repeated micro-case
   runs and the dense case. Corruption tests proved it rejects a non-top lift and a weight
   inversion. A focused case forced the planner to avoid frozen, non-reefer,
   weight-invalid, and hazardous-neighbor destinations. This replay shares
   static snapshot and hazardous-conflict definitions with the planner model.
2. **27 feasible / 3 infeasible:** Not established. One feasible micro-geometry
   completed and was suffix-renamed/repeated 27 times for deterministic
   behavior. Three small synthetic infeasible cases returned review. The exact
   reviewer witness proves the conservative heuristic can reject a feasible
   one-pickup yard, safely but incorrectly.
3. **Baseline comparison:** One repeated micro-geometry used 3 nearest-slot
   relocations versus 2 lookahead relocations (33% for that geometry). The
   separate dense fixture used 42 versus 41. No diverse 27-case aggregate,
   production 20% improvement, or worst-case claim is made.
4. **Ten-second bound:** The dense 288-stack, 1,280-container, 40-pickup planner
   and replay took about 8-16 ms across observed release runs on this machine. A
   zero-duration planner test returned review. File read/parse precedes the
   planner timer, and validation/publication are not cooperatively bounded, so
   no hard end-to-end 10-second guarantee is claimed.
5. **Explanations:** Every relocation lists its immediate pickup reason and the
   source, hold, frozen, capacity, weight, reefer, and hazardous checks. Review
   results identify the first blocked pickup and sorted preventing conditions.
6. **Reproducibility:** Five repeated success runs, five root-object key-order
   permutations produced byte-for-byte identical canonical output. The output
   contains no timestamps, random values, or hash-map iteration order. Five
   repeated false-negative reviews were also byte-identical.
7. **Publication safety:** Passed focused regressions for success to review,
   review to success, success to malformed input, success to zero timeout,
   success to injected replay rejection, and injected pre-rename write failure.
   Success/review publication is atomic and exclusive; failures leave no stale
   canonical or partial temporary artifact.
8. **Full artifact verification:** A valid control passed. Eleven corruptions of
   status/executable flags, counts, replay flag, step, pickup rank, reason, and
   rule-check metadata were rejected. The reviewer's exact dishonest file now
   exits nonzero with `status must be executable`.

## Coverage boundaries

- No fixed production snapshots, supplied nearest-slot result files, or C#
  executable were present in the trial materials.
- The repeated micro-case intentionally isolates later-pickup burial. Its 27
  suffix variants are one geometry and one result, not a feasibility suite.
- The dense case covers the requested block dimensions and a normal occupancy,
  but not simultaneous clusters of every constraint.
- The exact three-stack witness proves the bounded proposal generator is not
  complete even within one pickup; it must not be used to certify infeasibility.
- Mid-plan changes, multi-block work, crane travel/collision, and dispatcher or
  terminal-system integration remain out of scope as requested.
