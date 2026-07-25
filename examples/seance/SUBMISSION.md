## Hackathon submission

### category

pick **one**:

- [ ] agent support
- [x] performance
- [ ] novel interface / gaming

### team

- **project name:** séance
- **team / author name(s):** JT Hodge
- **contact (optional):** j.taylor.hodge@gmail.com

### what you built

Séance is time-travel search for git repositories. It shows a repository
at any commit in its history. You move a slider. The search indexes
follow. You can search each version of the repository in three ways:
keyword (BM25), semantic (ese vectors), and hybrid (reciprocal-rank
fusion). A header shows live statistics for the version on screen.

We measured séance against SQLite FTS5 plus a flat vector scan on one
workload: sqlite.git, 23,525 commits, a 2,000-commit walk, the same
queries, the same content rules, the same ese vectors. The full method
and tables are in `examples/seance/bench/RESULTS.md`.

**Headline results:**

| operation | séance (bog) | sqlite fts5 + flat scan | factor |
|---|---|---|---|
| semantic top-10 | **68µs** | 43.3ms | **637×** |
| hybrid keyword+semantic+rrf | **359µs** | 43.4ms | **121×** |
| keyword top-10 | 0.3ms | 0.2ms | parity |
| stats read (also as-of any commit) | **<1µs** | 60µs | **60×** |
| warp into a visited era, all views | 61ms | 10.8ms (text index only) | see tradeoffs |

**How each part of bogkit causes these gains:**

- **fold** holds seven materialized views correct through one delta
  stream: BM25 postings, the HNSW graph, a document table, counts,
  per-extension aggregates, line statistics, and a temporal index.
  Retraction is the mechanism that makes time travel exact: a move of
  the cursor applies one diff, forward or backward, and every view
  rolls with it. One commit step costs 22ms (p50). No view is ever
  rebuilt. Fold's `KeyedRanked` sink powers the "chronicle": its
  order-preserving score encoding makes "this file, this stat, as of
  commit T" a single bounded seek — 1–3µs across all 23,525 commits,
  from a 16MB store, with zero writes to scrub.
- **ese** makes embeddings a pure function of file content. This lets
  séance store the vector inside each fold record, so retraction never
  computes an embedding again. Batch encoding with the rayon feature
  embeds a full 2,220-file tree in ~95ms. The 68µs semantic query
  above includes the query embedding.
- **anny** gives true deletion: a removed vector repairs the graph
  instead of leaving a tombstone. Recall does not decay across the
  2,000-commit churn walk. This PR adds graph serialization to anny
  (`write_to`/`read_from`, bit-faithful) and a fast-load path to
  fold's Hnsw sink. A warp then restores the graph at memory
  bandwidth instead of re-inserting every vector — that is the
  difference between a 1.5s warp and a 61ms warp.
- **fold's persistent stores + APFS clonefile** give checkpoint-clone
  time travel: a full snapshot of all seven views costs one CoW clone
  (10–40ms at any store size). Visited eras become immutable masters.
  A return visit clones a master and opens it in tens of
  milliseconds, semantic search included.

**Where SQLite wins, and why (tradeoffs we measured and accept):**

- Apply: 2.3ms vs 22ms p50, 10× for SQLite. SQLite maintains one
  index. Bog maintains seven views and computes an embedding for each
  changed file on every write.
- Cold 10,000-commit jump: 590ms vs 3,275ms, 5.5× for SQLite. Same
  cause. The checkpoint-clone design exists for this reason: séance
  pays this cost once per era, then returns in 61ms.
- Snapshot create and warp: 9.9ms and 10.8ms for SQLite. A
  single-file database copies very well — this surprised us and we
  report it. But the SQLite warp restores a text index only. The bog
  warp restores semantic search too. A semantic query at the SQLite
  destination still costs 43ms.
- The rule the numbers show: bog pays at write time and wins where
  reads multiply.

### how to run

```bash
# from repo root
cargo run -p seance -- /path/to/any/git/repo
# then open http://localhost:3333
```

- First start of a large repository: a few seconds to materialize HEAD,
  plus a background chronicle build (~11s for sqlite.git; the app is
  usable during the build). Later starts resume from a checkpoint in
  ~40ms.
- Set `SEANCE_PORT` to change the port.
- Correctness check: `cargo run -p seance -- <repo> --probe "query"`
  walks HEAD → first commit → HEAD and fails if any view diverges.
- Benchmarks: `cargo run -p seance -- <repo> --bench-walk 2000
  --dump-vecs /tmp/vecs.json`, then
  `python3 examples/seance/bench/bench.py sqlite <repo> 2000 --vecs
  /tmp/vecs.json`, then the `summary` and `capsummary` subcommands.
  No Python dependencies.
- macOS/APFS gives O(1) checkpoint clones. Other filesystems fall back
  to full copies.

### demo / notes

- Benchmark report: `examples/seance/bench/RESULTS.md`.
- Demo path: search at HEAD; drag the slider (live playback, ~20ms per
  commit, results update as history replays); fling to a distant era
  (teleport); dwell (the era becomes a checkpoint); return (61ms warp,
  semantic search intact). The header updates in microseconds from the
  chronicle during every jump.
- Findings for the bogkit team, found by the probe and the benchmark:
  1. `Bm25` corrupts term frequencies when one transaction retracts
     and re-inserts the same document. Séance works around this with
     separate retract and insert transactions (`goto`, main.rs).
  2. Posting keyspaces need periodic compaction: query latency grows
     ~50× per 1,000 commits of churn without it, and stays flat with
     a `major_compact` every 200 commits.
  3. anny graph serialization (this PR) removes the only O(n) cost on
     store open.

### checklist

- [x] i forked bog-kit and built my project in this fork
- [x] my project is runnable from this pr (crate name and run command above)
- [x] i selected exactly one category
- [x] this pr is my official hackathon submission acknowledgment
