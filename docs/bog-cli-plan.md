# BogKit CLI + local dev server — development plan

> **Status (Aug 2026):** phases 0–3 are built and tested, plus typed custom
> routes (`.get`/`.post` with schemas captured at registration; POST runs
> in fold's `try_wtx`, so `Err` rolls the transaction back). Deferred:
> the crates.io dual-mode switch (blocked on fold publishing),
> `KeyedRanked` view coverage, persistent commit seqs (waiting on the
> delta log), and `Last-Event-ID` resume (unnecessary under /watch's
> level semantics — documented instead).

Goal: a `bogkit` CLI that scaffolds and runs BogKit projects locally, in two
flavors that both stay first-class. (The name `bog` is reserved for a future
tool that is an actual compiler.)

- **embedded** — a plain Rust binary using fold directly (what
  `scripts/new-project.sh` produces today)
- **server** — a fold program served over HTTP by a new `bog-serve` crate,
  with an auto-generated API and OpenAPI doc

The server flavor also lifts a hard embedded limitation: fjall's single-writer
database means exactly one process can open a fold store, so the serve process
becomes the shared access point — any number of clients, across devices and
sandboxes, each reading a consistent snapshot. `rtx` already takes `&self`
(pinned snapshot per call) while `wtx` takes `&mut self`, so reads can run
genuinely in parallel behind an RwLock; writes stay serialized, which is
fold's model anyway.

Cloud deployment is explicitly out of scope for this plan. Everything here
must be independently useful with zero cloud behind it: `bogkit new` +
`bogkit dev` should give any bogkit project an instant local HTTP API. The cloud story
later hangs off the same contract (`/openapi.json`, `/healthz`, `/schema`,
`$PORT`, `$BOG_DATA_DIR`), so nothing here is throwaway.

## New workspace members

```
cli/        binary crate `bogkit` — scaffolding + dev runner
serve/      library crate `bog-serve` — the HTTP layer over fold
```

Templates live inside the CLI binary (`include_str!`-style embedding, not
files copied from the repo), so `bogkit new` works from any directory once
the CLI itself is installed — required for the post-crates.io world.

## Command surface (v0)

```
bogkit new <name> [--kind server|embedded]   scaffold a project (default: server)
bogkit dev [-p <name>]                       run a project locally
bogkit api [-p <name>]                       fetch & pretty-print /openapi.json from the running server
```

- `bogkit new`, run inside this workspace, keeps today's behavior: creates
  `examples/<name>` with path deps (`examples/*` is already a workspace
  member, so no manifest edits needed). Run elsewhere, it creates a
  standalone cargo project with crates.io deps — gated until fold/anny/ese
  publish (see "crates.io transition").
- `bogkit dev` is `cargo run` plus conventions: picks a stable per-project
  data dir (`~/.bogkit/data/<name>` — persistent by default, `--fresh` to wipe),
  sets `$BOG_DATA_DIR`/`$PORT`, and for server projects prints the URL and
  route table on startup.
- `scripts/new-project.sh` stays until the CLI reaches parity, then becomes
  a one-line shim calling `bogkit new --kind embedded` for a release or two.

Name validation, collision checks, and the `--kind embedded` template body
are ports of the existing script — same rules, same output.

## `bog-serve` architecture

The API surface of any fold program is exactly (input type at the front,
named terminal sinks at the back); the closures in between never appear in
the API. That makes generation tractable:

### App builder

```rust
bog_serve::App::stream(data_dir, pipeline).run()      // Stream<D>
bog_serve::App::keyed(data_dir, pipeline).run()       // KeyedStream<K, V>
```

`run()` generalizes the chat example's plumbing: one plain thread owns the
fold stream (single-writer), an mpsc feeds it writes, a `tokio::watch`
publishes commit notifications, axum serves. No async database code.

### Write routes (from the stream flavor + input type)

Input types require `Serialize + DeserializeOwned + schemars::JsonSchema`.

- `Stream<D>`: `POST /insert`, `POST /remove` (full record — fold's actual
  retraction contract), `POST /batch`
- `KeyedStream<K, V>`: `PUT /docs/{key}`, `DELETE /docs/{key}`, `POST /batch`
- `/batch` maps to a single `wtx` — atomicity across all views is a feature,
  documented in the generated OpenAPI description.
- Every write response carries a monotonic commit `seq`.

### Read routes (from an `ApiSurface` trait)

A trait implemented per terminal sink, composed over tuples the same way
readers already mirror the pipeline structure:

```rust
trait ApiSurface {
    fn routes(&self) -> ...;            // axum routes, closed over reader access
    fn openapi(&self) -> ...;           // path + schema fragments
}
```

Sink names become paths:

| Sink | Routes |
|---|---|
| `Count` | `GET /views/{name}` → `{ value }` |
| `Bag<T>` | `GET /views/{name}?limit&offset` → `[[T, multiplicity]]` |
| `Table<K,V>` | `GET /views/{name}`, `GET /views/{name}/{key}` |
| `Multimap<K,V>` | `GET /views/{name}/{key}` → `[V]` |
| `Bm25` | `GET /views/{name}/search?q&k` → scored hits |
| `Hnsw` | `POST /views/{name}/search` (vector body; `?q=` text form only if a query encoder is registered) |
| `Histogram` / `Stats` / `Ranked` | corresponding `GET`s |

The HNSW text-query hole is real and handled explicitly: the text→vector map
lives in a user closure upstream where the server can't see it, so text
search requires opt-in registration (e.g. `.query_encoder(ese::encode_single)`
on the sink or serve config). Otherwise the route accepts raw vectors only.

### Platform plumbing (the future cloud contract)

- `GET /openapi.json` — assembled at startup from the same `ApiSurface`
  values that build the router, so doc and behavior cannot drift
- `GET /healthz`
- `GET /schema` — fingerprint: hash of (input type JSON schema + sink
  names/types). Locally: detects data-dir/pipeline mismatch at startup with
  a clear error instead of a fjall surprise. Later: the deploy-safety check.
- `GET /watch` — SSE, events carry commit `seq`, resumable via
  `Last-Event-ID`. v1 emits per-commit notification events; per-view
  payloads (`/views/{name}/watch`) are phase 3.
- Bearer-token middleware, **off by default locally**, enabled by env var.

### Escape hatch for custom routes

Non-negotiable (the search example's RRF hybrid endpoint is exactly this):

```rust
App::keyed(dir, pipeline)
    .route("/search/hybrid", get(|readers, params| { ... }))
    .run()
```

Custom handlers get snapshot access via a closure where the concrete reader
tuple type is inferred — the same trick the examples' macros use, but
captured by the builder's generics so users never name the type. If custom
logic can't coexist with generated routes, power users abandon the crate and
the OpenAPI guarantee is lost.

### Required fold changes (small, coordinated)

1. Sink name accessors — sinks store their name; expose it (`fn name()`) so
   `ApiSurface` can build paths and the fingerprint.
2. Possibly a marker/metadata trait on terminal readers so tuple composition
   of `ApiSurface` doesn't need one impl per tuple arity per sink
   combination. To be settled in a short design spike (phase 1, first task).
3. Nothing else: `wtx`/`rtx` and the single-writer model are used as-is.

Land these before the crates.io publish if possible — cheaper than a
point release right after.

## crates.io transition (~2 weeks out)

- Until publish: templates emit path deps; `bogkit new` only supports
  in-workspace mode (matching today's script). Standalone mode exists behind
  a flag but errors with a friendly "fold isn't on crates.io yet".
- On publish: templates carry both dep forms; the CLI picks path deps when
  cwd is inside this workspace, versioned deps otherwise. In-workspace mode
  stays supported forever (hackathon flow, contributor flow).
- `bog-serve` and the `bogkit` CLI should publish in the same wave as fold, so
  a standalone server project resolves entirely from crates.io.
- Version pinning: templates pin the minor version of fold/anny/ese/bog-serve
  that the CLI was built against.
- Watch item: ese is a heavy dependency (embedded model). First build of a
  standalone project will be slow; `bogkit new` should say so, and the embedded
  template should keep ese optional/commented like the current script keeps
  it merely available.

## Phases

### Phase 0 — CLI skeleton + embedded parity (small)

`bogkit new --kind embedded` reproduces the script exactly (same validation,
same manifest, same starter main.rs); `bogkit dev` runs it. clap, no config
file yet.

**Exit:** `bogkit new foo --kind embedded && bogkit dev -p foo` works from the
workspace root; CI job scaffolds and `cargo check`s the result.

### Phase 1 — `bog-serve` MVP on `Stream` (the meat)

Design spike on the `ApiSurface`/reader-metadata question first (with fold
changes from it landed), then: App builder + ingest thread, write routes +
`/batch`, `ApiSurface` for `Count`/`Bag`/`Table`, `/openapi.json`,
`/healthz`, `/schema`, commit seqs. Server template (a small
Count+Bag starter, served) and `bogkit new --kind server` + `bogkit dev`.

**Exit:** `bogkit new foo && bogkit dev` gives a working HTTP API over the starter
pipeline; `/openapi.json` validates against the OpenAPI spec; integration
test spawns the server and exercises every generated route; batch atomicity
covered by a test that reads mid-batch state and observes all-or-nothing.

### Phase 2 — keyed streams, search, custom routes

`App::keyed` with upsert/remove-by-key routes; `ApiSurface` for `Bm25` and
`Hnsw` (+ `query_encoder`); `/watch` SSE (commit events); the custom-route
escape hatch. **Dogfood milestone:** port `examples/search` to `bog-serve` —
generated BM25/HNSW/table routes plus a custom `/search/hybrid` — kept in
the repo as the reference server example.

**Exit:** the ported search example serves hybrid search over HTTP with a
correct OpenAPI doc, custom route included; forget-by-key over HTTP
demonstrably removes from every index.

### Phase 3 — ironclad

- Golden OpenAPI snapshots per template + the search port (drift = CI fail)
- Error model: consistent JSON error shape, correct status codes,
  deserialization failures reported with the offending field
- Concurrency: hammer test — many writers + readers + one SSE subscriber;
  seqs strictly monotonic, snapshots never torn
- `/schema` mismatch on startup produces the clear error, not corruption
- Remaining sinks (`Histogram`, `Stats`, `Ranked`, `Multimap`,
  `InvertedIndex`) covered by `ApiSurface`
- Per-view `/views/{name}/watch` payload streaming
- Docs: rustdoc for `bog-serve`, README section replacing the
  new-project.sh instructions
- crates.io switch flipped when fold publishes

**Exit:** the bar for starting cloud work — a stranger can `cargo install
bogkit`, `bogkit new`, `bogkit dev`, and drive the whole API from curl with
only the OpenAPI doc for guidance.

## Deferred (noted so they don't creep in)

Auth beyond a static local token, MCP surface generation, msgpack/CBOR
negotiation, delta-log export, remote builds, `bogkit deploy` — all
cloud-phase.

## Open questions

1. Tuple-arity blowup in `ApiSurface` composition — macro-generate impls up
   to arity N (fold presumably already does this for `Push` on tuples), or
   a different composition shape? Phase 1 spike decides.
2. `Bag` route semantics for non-trivially-large bags — pagination is in the
   table above, but is offset-pagination over an LSM iterator acceptable, or
   do we need cursor tokens? Fine to ship offset first.
3. Does `bogkit dev` watch-and-rebuild (`cargo watch` style)? Nice, not
   phase 0–3 critical.
4. Server template default port and data-dir conventions — proposed:
   `PORT=7877`, `~/.bogkit/data/<name>`; confirm before phase 1 lands.
