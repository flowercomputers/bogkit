# bogkit CLI + bog-serve: local HTTP serving for fold pipelines

Implements phases 0–3 of `docs/bog-cli-plan.md`: scaffold a BogKit project,
serve any fold pipeline over HTTP with the API generated from the pipeline
itself, and an OpenAPI doc that cannot drift from behavior.

## New crates

### `cli/` — the `bogkit` CLI

- `bogkit new <name> [--kind server|embedded]` — scaffolds into `examples/`;
  embedded reproduces `scripts/new-project.sh` exactly; server (default) is a
  fold pipeline served by bog-serve
- `bogkit dev [-p name] [--fresh]` — `cargo run` plus conventions: stable data
  dir (`~/.bogkit/data/<name>`), `$BOG_DATA_DIR`, `$PORT` (default 7877),
  project inferred from cwd
- `bogkit api [--port]` — pretty-prints the running server's `/openapi.json`
- templates embedded in the binary (works standalone once published);
  `bog` name reserved for the future compiler

### `serve/` — `bog-serve`

Core idea: a fold program's API surface is exactly (input type at the front,
named terminal sinks at the back). Everything between is closures that never
appear in the API — so the whole HTTP layer is generated.

- `App` (unkeyed `Stream`) and `KeyedApp` (`KeyedStream`) builders; user
  `main` stays a plain fn
- writes: `POST /insert|/remove` (unkeyed), `PUT|GET|DELETE /docs/{key}`
  (keyed), `POST /batch` — every batch is one atomic `wtx`; every write
  returns a monotonic commit `seq`
- reads: `GET /views/{name}[/{key}]` dispatched through a `Views` trait
  implemented on fold's reader types (orphan-rule friendly, tuple-composed);
  all sink kinds covered: count, bag, table, stats, histogram, ranked,
  multimap, inverted index, bm25, hnsw
- search: `GET /views/{name}/search?q=&k=` (text) and
  `POST {vector, k}` (raw vector); `TextQuery` wrapper opts an HNSW view into
  text queries by registering the pipeline's encoder — encoder/index dim
  mismatch is a compile error
- live: `GET /watch` (SSE, one `{"seq"}` event per commit) and
  `GET /views/{name}/watch?desc&limit` (fresh view payload per commit — e.g.
  a live top-10)
- custom routes: `.get(path, |readers, req| ...)` — handler gets the same
  reader tuple as `rtx`, one consistent snapshot, results in the standard
  `{seq, data}` envelope
- docs: `/openapi.json` (validates against the OpenAPI 3.1 spec) and
  `/schema` (pipeline fingerprint) assembled from the same values the router
  dispatches with
- safety: schema fingerprint persisted beside the data dir; reopening with a
  changed pipeline refuses to start with a clear error
- errors: uniform `{"error": ...}` JSON with field-level serde detail
- concurrency: `RwLock` mirrors fold's model (`rtx: &self` / `wtx: &mut
  self`) — parallel snapshot readers, one writer, multi-client access to a
  previously single-process database

## fold changes (small, coordinated)

- all terminal readers expose `name()` (enables generic dispatch/docs)
- `Hnsw` shared state: `Rc<RefCell>` → `Arc<Mutex>` — pipelines with vector
  indexes are now `Send + Sync` (servable across threads); also removes a
  latent double-borrow panic
- `pub use fjall;` re-export (downstream layers name `Snapshot` without
  version skew)

## Examples / docs / CI

- `examples/search-server` — the search example served: same pipeline, plus
  generated CRUD/search routes and a custom `/search/hybrid` RRF endpoint
- README: bogkit CLI workflow, bog-serve + CLI sections, search-server entry
- `.github/workflows/cli.yml` — builds CLI, runs bog-serve tests, validates
  golden OpenAPI docs against the spec, scaffolds both project kinds and
  compiles them

## Tests (51 total)

- 30 HTTP integration tests driving every generated route in-process
- batch atomicity, forget-by-key removes from every index, error model,
  fingerprint mismatch (`should_panic`)
- concurrency hammer: 8 writers × 25 + 8 torn-snapshot readers + SSE
  subscriber; seqs unique and gapless, snapshots never torn
- golden OpenAPI snapshots (`UPDATE_GOLDEN=1` to re-record), spec-validated

## Known gaps (deferred, tracked in the plan doc)

- commit seq resets on restart (persistent seq arrives with the delta log)
- custom routes are read-only GETs and appear as stubs in the OpenAPI doc
- `KeyedRanked` has no `Views` impl yet
- crates.io dual-mode scaffolding blocked on fold publishing
