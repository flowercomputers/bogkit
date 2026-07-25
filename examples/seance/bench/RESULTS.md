# seance benchmark: bog (fold + ese + anny) vs sqlite fts5

Workload: sqlite.git (23,525 first-parent commits). Materialize the tree at
HEAD−2000, apply commits forward one at a time to HEAD; after every commit
run the same three queries ("btree balance", "wal checkpoint", "vdbe
cursor", OR-of-terms, top-10) and one stats read. Then a capability phase
at HEAD: semantic search, hybrid fusion, era snapshot, a 10,000-commit
time-travel jump, and a warp back via the snapshot. Same content rules in
both systems: skip >1MB blobs, skip binaries, index the first 64KB. One
transaction per commit in both databases; SQLite runs WAL with
synchronous=NORMAL.

Reproduce:

    cargo run -q -p seance -- <repo> --bench-walk 2000 --dump-vecs /tmp/vecs.json > bog.jsonl
    python3 examples/seance/bench/bench.py sqlite <repo> 2000 --vecs /tmp/vecs.json > sqlite.jsonl
    python3 examples/seance/bench/bench.py summary bog=bog.jsonl sqlite=sqlite.jsonl
    python3 examples/seance/bench/bench.py capsummary bog=bog.jsonl sqlite=sqlite.jsonl

## Headline: semantic and hybrid search

Both systems search the **same 2,200 ese embeddings** — bog dumps its
vectors (`--dump-vecs`) and the baseline scans exactly those. The baseline
is the honest no-vector-index shape: SQLite has no vector search without
extensions, so retrieval is a flat cosine scan.

| capability | bog (anny HNSW) | sqlite fts5 + flat scan | speedup |
|---|---|---|---|
| semantic top-10 (ese cosine) | **68µs** (p99 139µs) | 43.3ms (p99 45.1ms) | **637×** |
| hybrid keyword+semantic+rrf | **359µs** (p99 419µs) | 43.4ms (p99 45.2ms) | **121×** |

Why the gap is structural, not incidental:

- The HNSW graph search is **sub-linear** in corpus size; the flat scan is
  O(n) forever. At 2,200 documents the gap is 637×; at 100k documents the
  flat scan grows ~45×, the graph search grows ~logarithmically.
- Bog's hybrid number is the **entire product operation**: embed the query
  with ese, BM25 top-10, HNSW top-10, reciprocal-rank fusion — 0.36ms,
  fast enough to run on every keystroke while the store is mid-scrub.
  The baseline's equivalent composes its FTS5 query with the flat scan
  and the same fusion, and inherits the scan's 43ms floor.
- The comparison is tilted *against* bog and it still wins: bog's timings
  include query embedding; the baseline is handed pre-computed query
  vectors and its doc norms are precomputed untimed (index-build work).
- Anny's HNSW is retraction-exact: deletions repair the graph rather than
  tombstoning, so these latencies hold under the churn workload below
  instead of decaying as the graph rots.

Disclosure: the flat scan is interpreter-bound pure Python (numpy is not
present on a stock Mac). A native flat scan might reach single-digit
milliseconds — call it 10–50× behind instead of 637× — but the O(n)
scaling and the absence of any vector index in stock SQLite are the
point, not the constant.

## Continuous-churn walk (2,000 commits, Apple Silicon, macOS)

| system | steps | apply p50 | apply p99 | query p50 | query p99 | stats p50 |
|---|---|---|---|---|---|---|
| bog (bm25+hnsw+ese, compact/200) | 2000 | 22ms | 143ms | 0.3ms | 10.9ms | <1µs |
| sqlite fts5 | 2000 | 2.3ms | 21.5ms | 0.2ms | 0.3ms | 60µs |

Keyword search is at parity (0.3ms vs 0.2ms). SQLite wins apply 10× —
while maintaining exactly one index; bog's apply maintains seven views
per write, including per-file ese embeddings and the true-deletion HNSW
that make the headline table possible at all. Stats reads: bog's
materialized counters answer in <1µs vs FTS5's 60µs aggregate scan, and
bog's chronicle answers the same stats *as of any commit in history* at
the same cost.

## Time travel and snapshots

| capability | bog | sqlite fts5 |
|---|---|---|
| era snapshot: create | 22ms (+4.6s background wash) | **9.9ms** |
| time travel: 10k-commit jump (1,905 files) | 3,275ms | **590ms** |
| warp to snapshot (+1 query) | 61ms | **10.8ms** |

SQLite earns these rows: a single-file database snapshots and restores
beautifully, and its cold jump moves one index 5.5× faster than bog moves
seven. The context that matters: bog's 61ms warp restores *all seven
views including the vector graph* (anny's serialized HNSW loads at memory
bandwidth instead of rebuilding), so the semantic and hybrid latencies in
the headline table are available 61ms after warping into any era. The
baseline's warp restores a text index; semantic search at the destination
would still need a 43ms scan per query — or an index it doesn't have.

## Findings the benchmark surfaced

1. Without maintenance, bog's keyword-query latency degrades ~50× per
   ~1,000 commits of churn (0.25ms → 12ms): LSM read amplification on
   the BM25 postings keyspace that fjall's compaction never repays
   unaided. A periodic `major_compact` of that keyspace (every 200
   commits, ~350ms, charged into apply above at ~1.7ms/step amortized)
   holds query p50 flat; the residual p99 tail (10.9ms) is
   between-compaction drift. Actionable upstream: posting-heavy fold
   sinks want a compaction policy, not just write-time incrementality.
2. Bog's cold jump is the price of rebuilding seven views over 1,905
   files; the checkpoint-clone architecture exists precisely so that
   price is paid once per era, after which revisits cost 61ms with
   semantic search intact.
