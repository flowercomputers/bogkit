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

1. Ask is prefilled: *how should we store refresh tokens?*
2. The briefing opens on the stale session-lifecycle wiki and the in-memory JWT design — not the filesystem wiki.
3. Click **Replay** (or step **Human comment lands** then **Retract stale wiki**).
4. The correcting comment jumps to #1. Retract leaves a ghost: `retracted: session-lifecycle · BM25 + HNSW + table`.
5. **Copy briefing** puts the packed next-session seed on the clipboard.

## Category

agent support
