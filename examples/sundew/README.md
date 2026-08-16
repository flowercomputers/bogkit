# Sundew

A sundew only sticks to what matters.

Alinery (the local multi-agent coding workspace) keeps tickets, phase artifacts,
human comments, and wiki pages as files. There is no query engine: the board is
a directory scan, ⌘K is substring, and the next session is told to “read
`artifacts/`.” Sundew is that missing engine, built on Bogkit.

**Files stay the source of truth.** Fold is the incremental index — the slot
Alinery’s own plan reserved for a later SQLite index. One upsert/remove resticks
BM25, the HNSW graph, the swamp, and the packed briefing. Retracting a stale
decision is a real delete, not a filter.

This crate is the hackathon prototype of that model. The same `Chunk` schema is
what a later Alinery sidecar would ingest from `.alinery/` and `docs/wiki/`.

```bash
cargo run -p sundew -- --probe "how should we store refresh tokens?"
cargo run -p sundew -- --script    # seed → comment → retract
cargo run -p sundew
# then open http://localhost:3000
```

`SUNDEW_PORT` overrides the port.

## Demo

1. Ask *how should we store refresh tokens?*
2. A stale wiki line and an in-memory-JWT design pack into the briefing.
3. Click **Human comment lands** — the comment jumps to #1 (comments are the product loop).
4. Click **Retract stale wiki** — Fold retracts BM25 and ANNy deletes the vector. The lie is gone.

## Category

agent support
