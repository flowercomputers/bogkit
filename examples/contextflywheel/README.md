# ContextFlywheel

A Fold working-memory layer so a small local agent (Qwen 3.8 27B) stays correct on long missions. Evidence is append-only. Beliefs are versioned. Only the current version is injected.

**Category:** agent support.

## Run

```sh
cargo test -p contextflywheel
cargo run -p contextflywheel -- --data /Users/alhinai/Documents/Codex/2026-08-16/event-details-event-links-partiful-luma/work/qwen-live-runtime serve --bind 127.0.0.1:8787
```

Open http://127.0.0.1:8787/. Demo script: [JUDGES.md](JUDGES.md).

Without that ledger:

```sh
DATA="$(mktemp -d)/mission"
cargo run -p contextflywheel -- --data "$DATA" demo-seed
cargo run -p contextflywheel -- --data "$DATA" serve
```

## What this live ledger shows

Fixture README suspected the database. Profiler: `database_ms=20`, elapsed `120.040s`. After the fix: **7/7 OK**, elapsed `0.160s`. Same API: 1 superseded belief excluded; **10,956** vs **1,388** estimated tokens (4 characters/token). Not a five-run eval.

BogKit usage: Fold incremental views, BM25, ESE embeddings, ANNy HNSW, RRF in `--hybrid`.
