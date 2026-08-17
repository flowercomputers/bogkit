# ContextFlywheel

ContextFlywheel is a versioned working-memory layer for autonomous agents. It keeps an append-only evidence ledger, derives the current belief for each topic, retrieves relevant records with BogKit BM25, and reconstructs what the agent knew at any step.

## Build and test

```sh
cargo test -p contextflywheel
cargo build -p contextflywheel
```

## Fast demo

```sh
DATA="$(mktemp -d)/mission"
cargo run -p contextflywheel -- --data "$DATA" demo-seed
cargo run -p contextflywheel -- --data "$DATA" context --query "timeout retry database"
cargo run -p contextflywheel -- --data "$DATA" timeline
cargo run -p contextflywheel -- --data "$DATA" history --step 5
cargo run -p contextflywheel -- --data "$DATA" context --hybrid --query "timeout retry database"
cargo run -p contextflywheel -- --data "$DATA" serve
```

## Real agent flow

```sh
contextflywheel --data .contextflywheel init \
  --objective "Fix the fixture timeout" \
  --constraint "Work only inside fixture/"

contextflywheel --data .contextflywheel context_record_evidence \
  --kind observation --text "Request timed out while loading an account"

# Use the printed observation ID as evidence.
contextflywheel --data .contextflywheel context_revise_belief \
  --key timeout-cause --statement "The database is slow" \
  --confidence 0.55 --evidence RECORD_ID
```

Belief revisions require evidence IDs and an exact expected current version. This prevents a stale agent turn from overwriting newer knowledge.

The shorter `record`, `belief`, `context`, `failure`, and `history` aliases are also accepted for interactive use.

## Claude Code hook

Build the binary, then copy or symlink `claude-plugin` into a Claude Code plugin installation. The hook uses `CONTEXTFLYWHEEL_HOME` when set and defaults to `.contextflywheel` in the working directory.

The controlled fixture intentionally fails until the agent changes `RETRY_BACKOFF_SECONDS` in `fixture/service.py` to fit within the deadline:

```sh
cd fixture
python3 profile.py
python3 -m unittest test_service.py  # expected to fail before the agent's fix
```

## Hybrid gate

Hybrid mode combines Fold BM25 with ESE embeddings and ANNy HNSW using reciprocal-rank fusion. Add `--rank` to request a second Qwen selection pass. The ranker selects at most 12 existing IDs with Supporting, Contradicting, Historical, or Procedural roles. Invalid output falls back to deterministic RRF and successful selections are cached.

```sh
contextflywheel --data .contextflywheel context_get --hybrid --rank --query "what should I investigate?"
```

The ranker accepts HTTPS endpoints and local HTTP endpoints only. It refuses a public plain-HTTP endpoint. Set `CONTEXTFLYWHEEL_MODE=hybrid` and `CONTEXTFLYWHEEL_RANK=1` for Claude hooks after an encrypted tunnel is established.

## Time Machine and evaluation

`serve` exposes a read-only timeline on `127.0.0.1:8787`. It reads the same Mission Ledger as the CLI. The evaluation harness under `evaluation/` runs isolated raw, deterministic, and hybrid trials without implicitly authorizing `--yolo` execution.
