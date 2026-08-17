# Discovery log

## Baseline-first hypothesis (recorded before component inspection)

The existing problem is a set problem, so the first implementation boundary should be an exact Rust oracle: a map from `(tenant_id, unix_hour)` to `HashSet<[u8; 16]>`. It should stream the same records as the candidate, reject invalid/truncated input before publishing a report, and provide the trusted cardinality for every bucket. This preserves the conceptual simplicity of the Kotlin job while making its memory cost directly comparable on the same machine, input, executable, and release profile.

My initial hypothesis is that the exact baseline will be operationally correct but fail the 128 MiB container constraint by a large margin on 10.8 million distinct bucket occurrences. The candidate should therefore be a fixed-width, mergeable distinct-count sketch. A 4,096-register HyperLogLog-style state is the smallest familiar design likely to satisfy both the 4,160-byte serialization ceiling and the stated error bounds. Register-wise maximum is commutative, associative, and idempotent, which should make shard and duplicate behavior deterministic. This is a hypothesis, not a result; the full accuracy matrix and load corpus must decide it.

The baseline comes first in the test sequence and benchmark interface. The exact counter and shared record validation will be driven from failing tests before any approximate implementation is written. Candidate tests will then use literal small-set expectations and the exact baseline as a result oracle only where the true count is otherwise too large to list conveniently.

## What I learned from the public entry points

- The root README presents BogKit as three independent tools plus examples, not as a required all-in-one framework.
- Fold incrementally materializes changing streams into durable views, including keyed aggregation and retraction.
- ESE is a static text-embedding tool.
- ANNy is an HNSW approximate-nearest-neighbor index.
- The starter, chat, search, and timeseries examples emphasize durable mutable views and transactional updates. The requested input, by contrast, is one immutable offline export whose per-bucket state must have a strict portable byte bound.

## Initial component fit, before implementation inspection

- **Fold: possible but doubtful.** Keyed aggregation resembles the grouping problem, but persistence, change logs, and retractions are outside this prototype's immutable batch boundary. Its storage overhead and state encoding may also make the strict per-bucket-state and candidate-RSS evidence harder to isolate. I will inspect only the aggregation and serialization-facing public surface before deciding.
- **ESE: no apparent fit.** Installation IDs are already fixed-width identifiers; no text embedding is needed.
- **ANNy: no apparent fit.** This is cardinality estimation, not nearest-neighbor retrieval.

No component will be used merely to demonstrate adoption. A standard-library-only prototype is the default if the smallest relevant component surface does not improve correctness, boundedness, determinism, or evidence quality.

## Finishing criteria

Done means the isolated prototype has test-first evidence for record validation, exact counting, approximate state construction/merge/serialization, deterministic generation and reporting, atomic publication, and all specified corrupt/fault cases; then passes formatting, all-target tests, strict Clippy, release demo, the 2,880-observation accuracy matrix, the complete 12-million-record comparison, duplicate and 50-permutation checks, two-run byte determinism, state-size bounds, and same-profile time/RSS measurement. Generated corpora, targets, binaries, databases, and temporary outputs must be removed, leaving only compact source, tests, documentation, fixtures, and machine-readable evidence under `simulation-output/`.

## Ordered discovery so far

1. The exact set is the only acceptable result oracle and must remain in the prototype even if it misses the memory gate.
2. Mergeability must be over serialized bounded states, never by replaying raw IDs.
3. Byte-identical direct/sharded states and duplicate idempotence favor a lattice-like register maximum, not a floating accumulator.
4. Atomic publication is part of correctness, not a deployment detail.
5. BogKit adoption is optional; the public examples suggest Fold solves a neighboring mutable-view problem, while ESE and ANNy solve unrelated problems.

## Post-inspection component decision

The inspected Fold `Aggregate` can express positive-only HLL insertion for this immutable subset; its signed-delta API is not itself incompatible because this trial never sends retractions. The supported `no_fit` boundary is that the public surfaces do not demonstrate exporting independently produced bounded per-bucket states and merging them externally by register-wise maximum without raw IDs. Fold would also add persistent `fjall` storage and transaction machinery that this batch proof does not need.

`Aggregate` buffers cloned updates per key until commit. That source observation was not benchmarked in Fold, and smaller positive-only transactions may be viable, so it remains decision context rather than a finding or new API proposal. ESE and ANNy remain unrelated no-fits.

The skeptical review later exposed a standalone prototype defect: v1 protected nested sketch bytes but not the outer tenant/hour binding. The repaired v2 state file checksums its complete outer header, bucket count, every key, and all nested states before constructing a decoded rollup.
