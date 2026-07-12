# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

BogKit is a Cargo workspace collecting the tooling behind "Bog style" databases: incrementally-maintained,
persistent dataflow with statically-typed pipelines. The core idea (in `fold`) is that queries are compiled
into a static tree of operators that eagerly update materialized views as data streams in/out, so reads are
just point lookups against already-computed state — nothing is recomputed from scratch.

Workspace members: `fold`, `anny`, `ese`, `examples/*` (resolver = "3", edition 2024).

## Commands

- Build everything: `cargo build`
- Run a specific example: `cargo run -p <name>` (e.g. `cargo run -p starter`, `cargo run -p search`)
- Run all tests: `cargo test`
- Run tests for one crate: `cargo test -p fold`
- Run a single test: `cargo test -p fold <test_name>`
- Lint: `cargo clippy`
- Benchmarks (criterion, `harness = false`): `cargo bench -p anny --bench hnsw`, `cargo bench -p ese --bench gooaq` (`gooaq` requires the `tests` feature: `cargo bench -p ese --bench gooaq --features tests`)
- Scaffold a new example project wired into the workspace: `./scripts/new-project.sh <project-name>` — creates `examples/<project-name>` with local path deps on `fold`, `anny`, `ese`

`fold`'s tests live under `fold/src/tests/` (not a top-level `tests/` dir) and are gated `#[cfg(test)]` from `lib.rs`.

## Crate architecture

### `fold` — the incremental dataflow engine

This is the crate to understand first; the other crates are consumed by it or by its examples.

- **Deltas, not values.** Everything flows through the pipeline as `(data, delta: isize)` pairs. A positive
  delta inserts `n` copies; a negative delta retracts them. Every operator and sink must honor retraction —
  pushing a record and later pushing the same record with the opposite delta must leave all state unchanged.
  This is *the* invariant that makes incremental maintenance work; when adding an operator, think through its
  retraction path before its insertion path.
- **Pipelines are trees of `Push` nodes, built inside-out.** Each operator (`Map`, `Filter`, `FlatMap`, ...)
  owns its downstream node(s) as a generic field, so a whole pipeline is one concrete, statically-dispatched
  type — no dyn dispatch, no runtime graph. Tuples of `Push` nodes (up to 16) implement `Push` by
  broadcasting each delta to every element, which is how a stream fans out into multiple parallel views
  (see `fold/src/pipeline/tuple.rs`).
- **The `Push` trait lifecycle** (`fold/src/pipeline/mod.rs`): `init` (resolve keyspaces once at startup) →
  `push` (one call per delta; stateful nodes buffer in memory here rather than touching the store) → `commit`
  (flush buffered state, emit downstream deltas — may run multiple times per transaction, since
  `Tx::rtx` triggers a flush for mid-transaction reads) → `abort` (discard buffered state on panic/rollback).
  Any new operator that wraps a downstream node must propagate `commit`/`abort`/`reader` even if it holds no
  state of its own.
- **Stateless vs. stateful operators.** `Map`/`Filter`/`FilterMap`/`FlatMap` forward transformed deltas
  immediately with no persisted state. `Distinct`/`Aggregate`/`TopK`/`Retain` persist per-element state in
  their own named keyspace and only emit downstream on `commit`, so repeated hot keys within one transaction
  collapse to a single downstream delta.
- **`Keyed<K, V>`** (via `KeyBy`/`Unkey`) and **`Scored<S, V>`** (via `ScoreBy`/`Unscore`) are the two
  "typed lane changes" in the pipeline: keyed sinks (`Table`, `Multimap`, `InvertedIndex`, `search::Bm25`,
  `search::Hnsw`) expect `Keyed` data; ranked/score sinks (`Ranked`, `Histogram`, `KeyedRanked`) expect
  `Scored` data.
- **Terminal sinks** (`fold/src/pipeline/terminal/`) are the pipeline's leaves — the only nodes that persist
  to the store. Three retraction disciplines depending on sink kind, all documented in
  `terminal/mod.rs`: *counting* sinks (`Count`, `Bag`, `Stats`, `Histogram`, `Ranked`, `KeyedRanked`)
  accumulate signed multiplicities so deltas cancel at any magnitude; *posting* sinks (`InvertedIndex`,
  `Multimap`, `search::Bm25`, `search::Hnsw`) are set-semantic per record — net-positive delta in a
  transaction inserts, net-negative deletes, no prior state read; `Table` is last-writer-wins within a
  transaction.
- **Storage**: state lives in an embedded [fjall](https://docs.rs/fjall) LSM store. Each named sink/stateful
  operator claims its own keyspace (`sink_{name}`) via `PipelineInitCtx::keyspace`; names must be unique
  pipeline-wide or `init` panics. Writes are transactional (`Stream::wtx`) and atomic across every sink;
  reads (`Stream::rtx`) see one consistent snapshot across the whole pipeline.
- **`Stream` vs. `KeyedStream`** (`fold/src/stream/`): `Stream` is the raw driver — you push deltas
  directly. `KeyedStream` fronts a stream with a primary-key table for upsert/delete-by-key semantics:
  `upsert` retracts a key's prior record before inserting the new one (so downstream state always reflects
  current rows, and re-upserting an unchanged value doesn't churn the graph); `remove` looks up and retracts
  by key alone, so callers never have to reproduce a record to delete it.

### `anny` (Approximate Nearest Neighbors)

Fast HNSW (hierarchical navigable small world) graph implementation, used by `fold`'s
`terminal::search::Hnsw` sink. See `anny/src/hnsw.rs`, `metric.rs` (distance functions, e.g. `Cosine`),
`traits.rs`. Has an optional `bench_compare` feature pulling in `instant-distance`/`hnsw_rs` for comparative
benchmarking.

### `ese` (Embedded Static Embeddings)

Compiles a tokenizer + embedding table into a static, dependency-free encoder: `encode`/`encode_single` turn
text into `[f32; DIMENSIONS]` vectors with no model runtime at request time. The embedding table and
tokenizer data are fetched and flattened into a perfect hash function at *build time* (`ese/build.rs`
downloads the model/tokenizer from HuggingFace and generates `lookup.rs`-consumed data), not runtime —
that's the core idea worth preserving in any future change here.

- Dimensionality and quantization are chosen via mutually-relevant Cargo features: `dim-{32,64,128,256,512,768,1024}`
  (default `dim-512`) and `quant-{8,16}`. `rayon` parallelizes `encode` for batches ≥16 inputs.
- `ese/api-py/` is a PyO3/maturin Python binding (crate `ese_core` aliased as `ese`) — build with
  `RUSTFLAGS="-Ctarget-cpu=native" maturin build --release` from within `ese/api-py`.
- `.cargo/config.toml` in `ese/` sets `-Ctarget-cpu=native`, relevant when benchmarking or building wheels.

### `examples/`

Runnable demonstrations of Bog-style databases, each a thin `main.rs` over `fold` (+ `anny`/`ese` as
needed). Read these before building something new — they're the idiomatic reference for how pipelines,
`Stream`/`KeyedStream`, and terminal sinks compose in practice:

- `starter` — minimal count + bag pipeline with insert/read/retract.
- `timeseries` — weather readings bucketed into hourly/daily aggregates, updated incrementally.
- `chat` — fold as source of truth, broadcasting updates to clients over a websocket
  (`cargo run -p chat`, then open http://localhost:3000).
- `search` — one document stream fanned out three ways: BM25 keyword search, HNSW semantic search over
  `ese` embeddings, and reciprocal-rank-fusion hybrid search. Good starting skeleton for agent-memory or
  document-search projects; note the pipeline's concrete type contains closures and can't be named, so
  stream-reading helpers there are macros rather than functions.

New example projects should be created via `./scripts/new-project.sh <name>` rather than by hand, so they're
correctly wired into the workspace `Cargo.toml` and get the standard `anny`/`ese`/`fold` path deps.
