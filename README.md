<div align="center">
  <img src="examples/sundew/src/icon.png" width="88" alt="Sundew icon">
  <h1>Sundew</h1>
  <p><strong>Live, retractable memory for coding agents.</strong></p>
  <p>Compile models early. Choose the retrieval goal late.</p>
</div>

![Sundew showing an explain-rationale briefing](examples/sundew/assets/explain-rationale.png)

Alinery's filesystem is the database. **Sundew is the query engine.** It turns tickets, artifacts, human comments, and wiki pages into a continuously maintained briefing for the next coding agent—then lets a human correct or retract that memory while the system is running.

Built on Bog/Fold for **Bogathon 3** in the **agent support** category.

## Why Sundew

Coding agents rarely fail because a relevant file does not exist. They fail because the right decision is buried, a rejected idea still ranks highly, or the next session receives more context than it can use.

Sundew treats agent context as mutable data:

- **Parallel retrieval views:** BM25, ESE 512d, and Potion Code 256d are maintained from one keyed stream.
- **Goal-conditioned ranking:** the same question can favor implementation evidence or explanatory rationale without mixing incompatible vector spaces.
- **Live correction:** a human comment is indexed immediately; retracting stale knowledge removes it from every view.
- **Packed context:** results are fused, policy-weighted, and packed into a 1,800-token next-session seed.

## Live correction, not another prompt patch

The demo begins with a stale wiki decision and an artifact recommending in-memory refresh tokens.

1. **Land comment** inserts the human correction under a stable key.
2. Bog propagates that positive delta through BM25, both HNSW indexes, the source table, and aggregate counts.
3. **Retract wiki** emits a negative delta for the stale record. It becomes impossible to retrieve—not merely hidden in the interface.
4. Sundew reruns the active query against a consistent snapshot, rebuilds the token-budgeted briefing, and broadcasts it to connected browsers.

```text
human edit
    ↓
Fold KeyedStream
    ├── BM25
    ├── ESE → HNSW 512d
    ├── Potion Code → HNSW 256d
    ├── source table
    └── kind counts
             ↓
weighted reciprocal-rank fusion
             ↓
1,800-token agent briefing
```

The model's weights are not edited. Sundew updates the agent's **external, retrievable memory**, so the next retrieval sees the correction everywhere.

## One question, two retrieval goals

Embedding models encode different opinions about similarity. Sundew keeps their views separate and combines ranks with explicit, visible policy.

### Locate implementation

Potion Code receives more weight when the agent needs the executable rule or current implementation.

![Sundew using the locate-implementation retrieval goal](examples/sundew/assets/locate-implementation.png)

### Explain rationale

ESE receives more weight when the agent needs the decision history and the reason behind it.

The accompanying [Parallax experiment](examples/sundew/PARALLAX.md) evaluated this premise on an 879-file Alinery snapshot. Potion Code won 9 of 10 judged query-goal cases, the models chose different top results for 4 of 5 topics, and the faster ESE-to-Potion cascade was rejected because it discarded too many strong Potion results.

## Run it

Requires Rust with edition 2024 support. The first build downloads the embedding-model artifacts.

```bash
cargo run --locked -p sundew
```

Open [http://localhost:3000](http://localhost:3000), then:

1. Switch between **Locate implementation** and **Explain rationale**.
2. Click **Replay** to land the correction and retract the stale wiki.
3. Open **Full briefing** to inspect the packed context.
4. Use **Copy briefing** to copy the next-session seed.

CLI modes are available for a quick check:

```bash
cargo run --locked -p sundew -- --probe "how should we store refresh tokens?"
cargo run --locked -p sundew -- --script
```

Set `SUNDEW_PORT` to override port 3000.

## What is real today

The indexing, multi-model retrieval, retraction, rank fusion, token packing, and WebSocket updates are live. The hackathon UI currently seeds a small set of Alinery-shaped fixtures so the correction sequence is deterministic. A production Alinery sidecar would feed the same keyed record shape from `.alinery/` and `docs/wiki/` while keeping files as the source of truth.

## Built with BogKit

- **Fold** — incremental, transactional materialized views and retractions.
- **ESE** — compiled static embeddings for fast general retrieval.
- **ANNy** — retractable HNSW approximate-nearest-neighbor indexes.
- **Potion Code** — a second, code-oriented embedding view used by Parallax and Sundew.

## Team

Matthew Ball (`matthewrball`) · Dustin Dannenhauer (`dtdannen`)
