# Baseline-first discovery hypothesis

## Problem restatement before inspecting BogKit internals

The authoritative job is a bounded, offline, immutable-snapshot transformation. It must reject the whole batch on invalid input, rank each market with explicit bytewise keys, match quantities exactly, check invariants and limits, then replace one advisory output atomically. Neither arrival order nor long-lived mutable state belongs in the result.

My baseline is therefore a small safe-Rust command-line program using ordinary owned records, a deterministic ordered map keyed by `(lane_id, service_date)`, explicit sorting of both sides, checked integer arithmetic, a deliberately separate slow reference matcher, and a sibling temporary output followed by same-filesystem rename. This is the simplest architecture I expect to make auditable and reproducible.

## Initial testable hypothesis

1. A straightforward `BTreeMap<MarketKey, Market>` plus sorted `Vec<Order>` sides will eliminate the retry-winner defect because all priority and output order is explicit.
2. The brief's fill bound follows directly from the matcher exhausting at least one active order per fill, but the implementation must still reject an over-cap result rather than trusting that proof.
3. Full validation before matching and publication plus a sibling temporary file should preserve an existing proposal on all pre-rename failures.
4. The baseline should comfortably prove core correctness on representative and generated small cases. The stated 650,000-row, two-core Linux performance and memory gates require the supplied target machine, Ruby evaluator, fixtures, and harness; this macOS host can provide representative measurements only.

## Components to consider only after this hypothesis

- **Fold:** potentially relevant only if its transactional materialized views make grouping or atomic batch state simpler without weakening deterministic ordering or adding persistent scratch state. The README presents it as an incremental, durable stream engine; this batch is a one-shot immutable transform, so I expect poor product fit unless its API removes meaningful work.
- **ESE:** static text embeddings do not participate in price-time matching, validation, conservation, or publication. Expected no fit.
- **ANNy:** approximate nearest-neighbor search conflicts with the exact ranking and matching specification and has no expected role. Expected no fit.

## What would change the decision

I will inspect only the public APIs and tests needed to answer whether Fold offers a deterministic, bounded, one-shot grouping/materialization path with less code than the baseline. I would use it only if that path is directly supported and preserves the brief's failure boundary. ESE or ANNy would be used only if their actual public contracts reveal a direct exact-data-processing capability missing from the README, which is unlikely.

## Post-hypothesis component decision

After recording the hypothesis, I inspected the components' public crate entry points and the Fold stream, keying, aggregation, and table contracts needed to test it.

1. **Fold — considered, not used.** Its public contract opens an embedded fjall database, persists every sink, and makes incremental delta transactions crash-safe. It can group and materialize records, but it does not perform the specified two-sided sorted match or atomic canonical file publication. Adding it would create a database/scratch lifecycle while leaving validation, exact ordering, matching, invariants, and publication custom. That is more mechanism at this immutable advisory boundary, not less.
2. **ESE — considered, not used.** Its public API turns text into embedding vectors. No validation or matching requirement uses semantic similarity.
3. **ANNy — considered, not used.** Its public modules implement HNSW approximate nearest-neighbor search. Approximation is incompatible with the authoritative exact priority order.

The baseline hypothesis held. The fit decision is grounded **no fit for this prototype**, not a claim that the components are defective.

## Ordered discovery and friction

1. The public examples quickly established that BogKit is optimized for persistent incremental views and search, while the brief is an offline snapshot transform.
2. The exact matcher was small and directly testable with ordinary ordered collections.
3. Full-batch validation and atomic final-path publication dominated the operational design; none of the three components removed that work.
4. A first full-host measurement found that retaining a second complete expected-fill vector raised peak RSS to 652,476,416 bytes. A market-at-a-time validation refactor preserved all tests and reduced the clear-only high-water mark to 481,624,064 bytes.
5. The checkout contained no authoritative Ruby evaluator, Ruby timing harness, or set of 80 negative fixtures. Equivalent local evidence can exercise the design, but it cannot replace those acceptance artifacts.

## Skeptical-review repair audit

The initial external review supported the `no_fit` decision but found four prototype-boundary defects. Repairing them did not make any BogKit component more relevant:

1. Proposal/input alias rejection and existing-file identity checks belong to the file publisher, not an incremental database or search component.
2. Normalizing relative parents, retaining the directory handle, and distinguishing prepublication failure from post-rename durability uncertainty are ordinary file lifecycle responsibilities.
3. The 4 KiB validation guarantee required a bounded `fill_buf` loop; Fold, ESE, and ANNy do not own source-line buffering.
4. Requiring the SHA-256 and authoritative gross value at `run_files`/CLI is an evidence-boundary rule, not a persistent-state need.

All four now have permanent regressions in `freight-clearing/tests/reviewer_repairs.rs`. The workload-specific decision remains **no fit**.
