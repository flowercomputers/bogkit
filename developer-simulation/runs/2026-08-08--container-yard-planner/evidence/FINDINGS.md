# Categorized findings

## Correctness defect

No correctness defect in BogKit was established. This trial did not use a
BogKit component in the prototype, and no component internals were inspected.

Prototype finding: hazardous adjacency needs an unambiguous undirected model.
Severity: high for operational use. Confidence: high. Reproduction: provide a
stack that names a neighbor which does not name it back. Current behavior:
snapshot validation rejects it before planning. Smallest improvement beyond the
prototype: define and validate the production input contract centrally.

## Performance problem

No BogKit performance problem was established. The standalone release planner
and replay completed the synthetic 1,280-container workload in roughly 8-16 ms
on observed runs, far below 10 seconds for that fixture. Confidence: high for this
fixture, low for production distributions. Reproduction: run the named dense
acceptance test. Smallest next step: benchmark the supplied 30 snapshots on the
specified four-core laptop and preserve per-case timings.

## Temporary lab packaging

Finding: placing this standalone trial artifact below, but outside, the current
workspace required a temporary child `[workspace]`. This is not BogKit API
friction and not a feature candidate. Severity: none for product; low for lab
coordination. Confidence: high.
Reproduction: create a nested manifest outside the workspace member list and run
`cargo test`; Cargo reports that the package believes it is in a workspace.
Archive result: the coordinator removed the child `[workspace]` and lockfile,
used the nested daily workspace and single lockfile, and reran the final gates.

## Documentation gap

Finding: the public root README links only Fold's internal documentation; the
examples are the main public explanation for ESE and ANNy, and there is no
decision guide for non-search, non-incremental workloads. Severity: low.
Confidence: high from the permitted public surface. Reproduction: follow only
the README's documentation section. Smallest improvement: add a short "when not
to use these crates" table and direct links for each crate.

## Missing capability

Finding: the public surface contains no deterministic constrained-planning or
bounded-search primitive that directly addresses legal yard reshuffling.
Severity: medium for this use case, not necessarily a BogKit product defect.
Confidence: medium because only the required public README/examples were used;
internals were intentionally not searched. Reproduction: map each public crate
description to snapshot constraint search. Smallest improvement, only if this
becomes a target BogKit use case: publish a generic deterministic state-space
search example before designing a new crate.

## Poor product fit

Finding: Fold, ESE, and ANNy are poor fits for the prototype's core planning
loop. Severity: high if forced into the implementation because they add
complexity without proving legality or resolving dead ends. Confidence: high
for ESE/ANNy, medium-high for Fold based on its public examples. Reproduction:
compare the frozen one-run input and move-sequence output to each public crate's
documented role. Smallest improvement: keep the planner standalone; consider
Fold later only if durable incremental operational views become a separate
requirement.

## Prototype-specific risk

Finding: destination choice is deterministic bounded lookahead, not complete
backtracking and not a proof of feasibility. It can still reject a feasible
wave if an early legal placement causes a later dead end. Severity: high before
production evaluation. Confidence: high. Reproduction: use the preserved exact
three-stack witness: the heuristic moves light `X1` from `A` to nearer `B`, then
cannot move heavier `X2`; `X1 A→C`, `X2 A→B`, `P A→pickup_lane` is legal.
Smallest improvement: add a bounded beam/backtracking search with a shared
deadline, then validate it on the fixed 30 snapshots. Until then, describe this
only as a conservative proposal generator that can false-reject.

## Resolved publication defect

Round-1 finding: reused output locations retained stale `moves.json` after
review or malformed input, and writes published directly. Severity before fix:
blocker. Confidence: high. Resolution: each run invalidates the three owned
current-generation names before reading input; a fully written and synced hidden
temporary file is atomically renamed to exactly one canonical result. Focused
reuse and injected-write-failure regressions pass. Residual boundary: an
operating-system failure that prevents invalidating the old artifact is returned
as an error; the program cannot override filesystem permissions.

## Resolved verifier defect

Round-1 finding: transition replay ignored dishonest executable metadata and
explanation fields. Severity before fix: important. Confidence: high.
Resolution: verification now checks flags, counts, step/rank consistency,
canonical reasons/check lists, then replays transitions. The replay is separate
but shares static snapshot and hazardous definitions, so no fully independent
claim remains.
