# Mergeable unique-installation reach rollups

## Developer role and Rust experience

You are an analytics-infrastructure engineer for a business software company. You have eight years of production Kotlin and SQL experience and four months of Rust experience from batch utilities. You have no prior knowledge of the candidate library being evaluated; treat adoption as optional and start with the existing analytics problem.

## Existing system and baseline implementation

An advisory product-analytics dashboard reports unique active installations for each tenant and UTC hour. The nightly Kotlin job reads an immutable daily export and stores every 128-bit installation ID in an exact hash set keyed by `(tenant_id, hour)`. The result is trusted for capacity planning and product analysis, but never for billing, access control, fraud action, or customer-facing contractual counts.

The exact set-based job is conceptually simple and supplies ground truth, but its peak memory now exceeds the batch container limit. Operations currently raises the limit or splits a day into manual tenant ranges. The prototype must retain an exact Rust set-based baseline as the oracle while evaluating a bounded, mergeable approximate summary.

## Concrete pain

The daily export is growing faster than container memory. Manual partitioning makes runtimes unpredictable and complicates retries, while summing exact per-shard counts double-counts installations seen in more than one shard. The team needs to know whether a compact mergeable summary can meet an explicit error contract, remain deterministic, and reduce memory enough to justify replacing the current advisory rollup.

## Realistic workload and data shape

Generate one deterministic 24-hour export with 240 tenants and therefore `240 × 24 = 5,760` `(tenant, hour)` buckets. For each tenant, assign:

- 20 hourly buckets with exactly 1,000 distinct installation IDs: 4,800 buckets and 4,800,000 unique occurrences.
- 3 hourly buckets with exactly 5,000 distinct installation IDs: 720 buckets and 3,600,000 unique occurrences.
- 1 hourly bucket with exactly 10,000 distinct installation IDs: 240 buckets and 2,400,000 unique occurrences.
- 1,200,000 additional records that duplicate an ID already present in the same bucket.

The export therefore contains exactly 10,800,000 unique bucket occurrences and 12,000,000 records. Partition it round-robin into eight immutable source shards of exactly 1,500,000 records each. A record is `(tenant_id: u32, unix_hour: i64, installation_id: [u8; 16])`. Input order within each shard is deliberately shuffled.

Also generate an accuracy matrix for 20 independent seeds. Each seed has 24 buckets at each true cardinality in `{100, 500, 1,000, 5,000, 10,000, 50,000}`, plus 25% within-bucket duplicates. This gives 144 measured buckets per seed and 2,880 error observations overall.

## Operational constraints

- Process the immutable export offline on one four-core machine with a 128 MiB peak resident-memory limit for the candidate.
- A serialized approximate state for one bucket, including version and hash-seed identity, must be at most 4,160 bytes. Across 5,760 buckets this caps serialized bucket state at 23,961,600 bytes, about 22.85 MiB.
- The eight shards must be aggregatable independently and mergeable without raw installation IDs.
- Duplicate records must not change a bucket estimate or serialized state.
- Hashing, serialization format, estimator calculation, and report ordering must be explicit and reproducible.
- The candidate may use parallelism, but its final state and report must not depend on thread scheduling, shard order, input order, locale, clock, or network.
- Invalid records fail the affected run before a complete report is published. Preserve any earlier complete report by writing the new report to a temporary path and renaming only after validation.

## Measurable acceptance criteria

1. Across the 2,880 accuracy observations, absolute relative error has a median at or below 1.5%, a 95th percentile at or below 3.5%, and a worst case at or below 8.0%. Report positive and negative error separately so bias is visible.
2. On the 12,000,000-record load corpus, every estimate is compared with the exact Rust baseline and all 5,760 buckets appear exactly once in canonical `(tenant_id, unix_hour)` order.
3. Direct aggregation and eight-shard aggregation produce byte-identical serialized state for every bucket. The same equality holds for 50 seeded permutations of shard merge order.
4. Adding any number of within-bucket duplicates leaves serialized state byte-identical. Merging a state with itself is byte-identical to the original state.
5. Each serialized bucket state is at most 4,160 bytes and contains enough version and hash identity information to reject incompatible merges.
6. Candidate peak resident memory is at most 128 MiB and at least four times lower than the exact Rust baseline on the 12,000,000-record corpus, measured with the same build profile and harness.
7. Candidate wall time is no more than 1.5 times the exact Rust baseline. Report the slower of three runs after one warm-up rather than only the best run.
8. Two complete repeated runs with the same seed produce byte-identical state files, canonical report bytes, and summary digest.

## Explicit non-goals

- No billing, quota enforcement, access decision, fraud action, or exact-count claim.
- No online query service, live database connection, dashboard UI, or distributed coordinator.
- No late-event correction, deletion, sliding window, or partial-day mutation; the source is one immutable daily export.
- No privacy or anonymity guarantee merely from hashing or approximation.
- No attempt to estimate unions across different hours in the published report.
- No requirement to preserve compatibility with the current Kotlin job’s internal serialization.

## Smallest self-contained prototype boundary

Build one Rust workspace containing: the deterministic load and accuracy generators, a streaming record reader, an exact hash-set baseline, one approximate per-bucket summary, deterministic state serialization, shard merge logic, canonical report publication, and a harness for correctness, error, state-size, memory, time, and failure tests. The output needs only per-bucket exact count, estimate, signed and absolute relative error, serialized size, and a run summary. Do not build a service or integrate a production datastore.

## Baseline comparison

Use the Rust exact hash-set implementation as the result oracle and performance baseline on identical records. Compare per-bucket counts, error distribution, peak resident memory, wall time, total serialized state, and behavior under shard-order permutations and duplicates. Separately note the current Kotlin job’s operational pain, but do not use cross-language timing as proof of an improvement. Adoption is justified only if the accuracy, merge, state-size, memory, determinism, and runtime gates all pass.

## Fault and adversarial cases

- All records in a shard belong to one tenant and hour; other shards remain normally distributed.
- IDs are sequential, share a 120-bit prefix, are all-zero except for one changing byte, or arrive in reverse order.
- Every record in a bucket is duplicated 1,000 times.
- The same logical records arrive in 50 shuffled input orders and 50 shard merge orders.
- Empty input; one bucket; the minimum and maximum declared tenant IDs; negative, overflowing, or out-of-day hours.
- Truncated records, trailing bytes, unknown format version, and incompatible hash-seed identity during merge.
- A process exit after temporary report creation but before rename; the prior complete report must remain intact.
- State bytes with a changed version, impossible register value, checksum mismatch if a checksum is used, or truncated payload must be rejected without a partial report.
- Accuracy seeds that expose unusually high positive or negative error must be retained as named regression cases, not discarded as outliers.

## Evidence expected

Provide exact build and run commands, toolchain and machine description, all generator and hash seeds, proof of the workload counts, the 2,880-row accuracy artifact or a machine-readable equivalent, percentile and bias summaries, per-bucket state-size maximum, direct-versus-merged byte digests, results for all 50 merge permutations, duplicate-idempotence results, peak-memory method and values for both implementations, and three timed runs for each after warm-up. Show that malformed and interrupted-publication cases leave no new complete report and preserve the previous one. State any failed gate and whether the result is no fit; statistical success on a handpicked example is not sufficient.
