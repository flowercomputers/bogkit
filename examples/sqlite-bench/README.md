# sqlite-bench: bog vs SQLite, directly and fairly

A single-binary benchmark comparing a bog pipeline (fold + ese + anny)
against SQLite (FTS5 + [sqlite-vec]) on a live document-search workload —
the "agent memory" shape: one stream of documents, upserted and removed
over time, queried by keyword, semantically, and hybrid.

```console
$ cargo run --release -p sqlite-bench                  # full run: 100k docs, ~minutes
$ cargo run --release -p sqlite-bench -- --scale 10000 # quicker
```

Markdown summary to stdout, machine-readable JSONL to `sqlite-bench.jsonl`
(`--out` to change), progress to stderr. Flags: `--scale`, `--queries`,
`--churn-ops`, `--batch`, `--churn-batch`, `--seed`, `--out`.

## Workload

1. **Ingest with scale checkpoints.** Insert topic-clustered synthetic
   documents in 1,000-doc transactions. At 1k / 10k / 100k docs, measure
   keyword, semantic, and hybrid top-10 plus a stats read (p50/p99 over
   `--queries` distinct queries), and semantic recall@10.
2. **Churn.** 4,000 mixed ops (50% edit, 25% insert, 25% delete) in
   10-op transactions, with the full query battery re-measured after each
   quarter — search latency and recall while the store is being rewritten.
3. **Costs.** Ingest throughput, per-batch percentiles, and on-disk size —
   the rows SQLite tends to win, reported with the same prominence.

## The contenders

**bog**: one `KeyedStream` maintaining four views per write — BM25
postings, ese embeddings feeding a true-deletion HNSW (anny), an id→text
table, and running stats. Queries read materialized state.

**SQLite**: the strongest stock-ecosystem shape we know how to build —
FTS5 for keyword, the native sqlite-vec `vec0` virtual table (cosine) for
vectors, docs table for lookups; WAL, `synchronous=NORMAL`, prepared
statements, one transaction per batch, all driven from Rust via rusqlite.

## Fairness rules

- **No interpreted baseline.** Both sides are native code in one process.
- **Embedding charged symmetrically.** ese embeds every document at write
  time and every query at query time on *both* sides.
- **Recall is reported, not assumed.** HNSW is approximate; every semantic
  latency comes with recall@10 against exact cosine ground truth computed
  untimed by the harness. sqlite-vec's brute-force scan is exact, so its
  1.000 doubles as a harness self-check.
- **One seed drives everything.** Same corpus, same queries, same churn
  schedule for both systems; runs are exactly reproducible.
- **Same tokenization ground.** The corpus is lowercase ASCII, where FTS5's
  unicode61 and fold's ASCII BM25 tokenizer agree. Queries are OR-of-terms
  in both. Both sides get two warmup queries per operation before timing.
- **Losses are results.** Ingest throughput, disk size, and any recall
  drift are in the output tables, not a footnote.

- **Equal memory budgets.** Both sides get a 1 GiB page/block cache
  (`PRAGMA cache_size` for SQLite, fjall `cache_size` for bog). The
  defaults — 2 MiB for SQLite, 32 MiB for fjall — are both far below what
  an operator would ship for a store this size, and bog's point-read-heavy
  BM25 postings fall off a cliff (~700× at 100k docs) when the hot set
  outgrows the cache.
- **Maintenance is charged to writes.** Bog major-compacts its BM25
  postings keyspace every 2,500 applied ops, inside `apply()`, so the
  harness bills it as write time — mirroring how FTS5 pays its segment
  merges inside its own inserts. Without this, LSM read amplification on
  the postings degrades bog keyword scans by orders of magnitude at 100k
  docs (the séance benchmark's finding #1, reproduced here).

Known asymmetries, disclosed: bog's HNSW lives in process memory backed by
persisted rows (a cold open rebuilds it; this benchmark measures a warm
store on both sides), and sqlite-vec's KNN is exact brute force — it pays
O(n) per query but never loses recall. A vector index alternative for
SQLite would trade back along the same curve.

[sqlite-vec]: https://github.com/asg017/sqlite-vec
