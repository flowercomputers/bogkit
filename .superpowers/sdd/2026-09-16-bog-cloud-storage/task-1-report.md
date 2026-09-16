# Task 1 report: records template

## Result

Implemented the standalone `records-v1` JSON document template. It stores only JSON objects, preserves their JSON representation through Fold's Postcard persistence, exposes `docs` and `total`, validates record/key/request/batch/page limits, keeps batches atomic, and reopens persisted records.

## Changed files

- `cloud-records/Cargo.toml`
- `cloud-records/src/lib.rs`
- `cloud-records/src/document.rs`
- `cloud-records/src/template.rs`
- `cloud-records/tests/records.rs`
- `serve/src/lib.rs`: fallible keyed startup and opt-in literal string path keys
- `serve/src/http.rs`, `serve/src/views.rs`: carry the opt-in string-key behavior through document and point-view routes; legacy parsing remains the default
- workspace `Cargo.toml` and `Cargo.lock`: add `cloud-records`; the coordinated lockfile also includes the separately owned `cloud` crate

No worker, per-write checkpoint, or manager durability behavior from task 2 was implemented.

## Test-first record

Red: after dependency setup completed, `cargo test -p bog-cloud-records --test records` failed because `bog_cloud_records` did not exist. Later edge-case tests separately failed for encoded `limit`, quote-wrapped literal keys, and schema mismatch panics before their fixes.

Green:

- `cargo test -p bog-cloud-records`: 6 integration tests passed, plus library and doc tests.
- `cargo test --locked -p bog-serve -p fold`: passed with local Unix-socket permission. The first sandboxed run failed only because the sandbox denied Unix socket creation; the permitted rerun passed every server, Fold, and doc test.
- `git diff --check`: passed.

Coverage includes the required nested fixture; replacement, batch, and restart equality; empty objects and arrays; escaped strings; signed and unsigned integer extremes; scalar roots; depth 32/33; exact and excessive 256 KiB sizes; numeric, boolean-looking, null-looking, and quote-wrapped string keys; CRUD; count stability on replacement; atomic mixed/rejected batches; request, batch, key, list, and offset bounds; schema object representation; locked-store errors; and schema mismatch errors.

## Interfaces

- `JsonDocument::try_from_value(Value) -> Result<JsonDocument, DocumentError>`
- `JsonDocument::as_value(&self) -> &Value`
- `records_router(&Path) -> Result<axum::Router, TemplateError>`
- `TEMPLATE_ID: &str = "records-v1"`
- `KeyedApp::try_stream(...) -> Result<KeyedApp<...>, fold::fjall::Error>`
- `KeyedApp::raw_string_keys()` opts a keyed app into literal string URL keys; existing apps retain JSON-first parsing.

## Concerns and deviations

- The established count view response is `{ "value": n }` inside the standard `{seq,data}` envelope; tests preserve that existing contract.
- `records_router` converts schema-fingerprint initialization panics into `TemplateError::Initialize`. The shared server still invokes the process panic hook before the panic is caught; making all schema startup paths natively fallible would be a broader serve lifecycle change for a later task.
- The final workspace lockfile must be inspected as one coordinated artifact because another task added `cloud` and its dependencies concurrently. No existing package version should be changed solely by this task.
