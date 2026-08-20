# Demo (90 seconds)

Open http://127.0.0.1:8787/ with the live ledger.

```sh
cargo run -p contextflywheel -- --data /Users/alhinai/Documents/Codex/2026-08-16/event-details-event-links-partiful-luma/work/qwen-live-runtime serve --bind 127.0.0.1:8787
```

1. **What the agent believes now** — retry/unit bugs, not the database.
2. **Belief changed** — Before → Evidence → Now → next action.
3. **Context sent to Qwen** — current beliefs in, superseded belief out, Fold / BM25 / ESE / ANNy named.
4. **7/7 tests passed** from the same ledger. Profiler: 20ms DB vs 120.040s elapsed, then 0.160s.
5. **Measured** — 10,956 estimated raw tokens vs 1,388 compiled. Not a five-run benchmark.

BogKit: Fold current beliefs, BM25 keywords, ESE+ANNy hybrid. The UI is a Time Machine over one ledger.
