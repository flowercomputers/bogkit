# ContextFlywheel

Incremental working memory so a small local agent (Qwen 3.8 27B) stays correct on long missions.

**Category:** agent support (not a search demo). No credentials. No API keys.

BogKit is the retrieval engine: Fold views over an append-only ledger, BM25 for keyword hits, ESE embeddings plus ANNy HNSW for semantics, fused with reciprocal-rank fusion (RRF). Beliefs are versioned; stale ones are excluded from the context packed for the next agent turn.

## 60-second run (from repo root)

```sh
cargo test -p contextflywheel
cargo build -p contextflywheel
```

**Live Time Machine (this machine, real Qwen ledger):**

```sh
cargo run -p contextflywheel -- --data /Users/alhinai/Documents/Codex/2026-08-16/event-details-event-links-partiful-luma/work/qwen-live-runtime serve
```

Open [http://127.0.0.1:8787/](http://127.0.0.1:8787/).

**PR / other machines:** that path is local. Seed a disposable ledger instead:

```sh
DATA="$(mktemp -d)/mission"
cargo run -p contextflywheel -- --data "$DATA" demo-seed
cargo run -p contextflywheel -- --data "$DATA" serve
```

Then open [http://127.0.0.1:8787/](http://127.0.0.1:8787/). `demo-seed` requires an empty `--data` directory.

Optional CLI checks on the same `$DATA`:

```sh
cargo run -p contextflywheel -- --data "$DATA" context --query "timeout retry database"
cargo run -p contextflywheel -- --data "$DATA" timeline
cargo run -p contextflywheel -- --data "$DATA" history --step 5
cargo run -p contextflywheel -- --data "$DATA" context --hybrid --query "timeout retry database"
```

## What the judge should see

On the live ledger: **Current understanding**, **BELIEF CHANGED**, **Context sent to Qwen**, and a **7/7** outcome parsed from the mission. Click-through script: [`JUDGES.md`](JUDGES.md).

Numbers below are from `qwen-live-runtime/ledger.jsonl` and `GET /api/mission` on that ledger, not from `demo-seed`. Baseline: 7 tests, 1 failure + 2 errors. Profiler: `database_ms=20`, `simulated_elapsed_seconds=120.040`. After the fix: `Ran 7 tests, OK` and `0.160`. Mission API: `verified_outcome` 7/7 ok at step 61; 1 superseded belief excluded; raw **10,956** vs compiled **1,388** estimated tokens (4 characters/token, budget 8,000); `last_step` 72. `cargo test -p contextflywheel` is 8/8.

## 90-second talk track

A 27B local model cannot keep a whole mission in its window. ContextFlywheel writes every observation, tool result, and failed approach to a ledger, then compiles *current* beliefs plus Fold retrieval (BM25 + ESE + ANNy HNSW, RRF in `--hybrid`) into a budgeted prompt. The fixture README suspected the database; profiler evidence (20ms DB, 120s backoff) and the versioned belief on the ledger name the real bugs (ms/s units, off-by-one attempts, stuck reservation). Qwen fixed `checkout.py`; the same ledger shows 7/7 and 0.160s elapsed. The UI Time Machine reads that file — not a second source of truth.

## Agent CLI (optional)

```sh
cargo run -p contextflywheel -- --data .contextflywheel init \
  --objective "Fix the fixture timeout" \
  --constraint "Work only inside fixture/"

cargo run -p contextflywheel -- --data .contextflywheel record \
  --kind observation --text "Request timed out while loading an account"
```

Use the printed record `id` as `--evidence`. `belief` revisions take evidence IDs and an optional `--expected_version` so a stale turn cannot overwrite a newer belief. Aliases: `record`, `belief`, `context`, `failure`, `history`.

`serve` binds `127.0.0.1:8787` by default. `--hybrid` is BM25 + embeddings + HNSW + RRF; `--rank` is an optional second pass that still falls back to deterministic RRF. Evaluation lives in `evaluation/` and does not launch an agent by itself.
