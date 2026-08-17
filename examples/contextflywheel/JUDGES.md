# ContextFlywheel — 90-second judge script

**Category:** Agent context support  
**Thesis:** A small local agent stays correct on a long mission because BogKit Fold keeps current beliefs incremental, and only that compiled state is injected into the next Qwen prompt.

## Before the demo

From the bogkit repo root:

```sh
cargo test -p contextflywheel
cargo run -p contextflywheel -- --data /Users/alhinai/Documents/Codex/2026-08-16/event-details-event-links-partiful-luma/work/qwen-live-runtime serve --bind 127.0.0.1:8787
```

Open http://127.0.0.1:8787/

If the live ledger is unavailable, seed a local copy:

```sh
DATA="$(mktemp -d)/mission"
cargo run -p contextflywheel -- --data "$DATA" demo-seed
cargo run -p contextflywheel -- --data "$DATA" serve --bind 127.0.0.1:8787
```

## Talk track

1. Small local models rot on long missions because the transcript still contains beliefs that evidence already disproved.
2. ContextFlywheel is a Fold ledger: evidence is append-only, beliefs are versioned, only the current version is injectable.
3. Point at **What the agent believes now**. This run concluded the timeout was millisecond/second unit bugs and retry off-by-one, not the database.
4. Point at **BELIEF CHANGED**: Before → Evidence → Now → Behavioral Effect.
5. Click **Context sent to Qwen**. Protected Fold state, selected memories, excluded superseded beliefs, token budget. Retrieval is Fold views; hybrid CLI adds BM25 + ESE + ANNy RRF.
6. Point at the outcome row parsed from the same ledger: **7/7 tests passed** (`verified_outcome` at step 61). Profiler: `database_ms=20`, elapsed `120.040s` before the fix, `0.160s` after.
7. Point at **Measured from this ledger**: raw transcript **10,956** estimated tokens vs ContextFlywheel **1,388** selected (budget 8,000). **1** superseded belief excluded. Token method: four characters per token. Not a five-run eval.

## What BogKit is doing

- **Fold:** incremental current objective, current belief per key, evidence, failed approaches.
- **BM25:** exact errors, filenames, symbols.
- **ESE + ANNy:** paraphrased evidence, fused with BM25 via RRF in `--hybrid`.
- **Time Machine:** `history --step N` and this UI read the same ledger.
