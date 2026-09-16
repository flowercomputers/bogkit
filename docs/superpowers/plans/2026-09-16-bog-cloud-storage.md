# Bog Cloud Storage and Worker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Provide an independently runnable records worker with correct JSON persistence, atomic views, and explicit durability/shutdown semantics.

**Architecture:** Add an application-specific JSON wrapper and records template above Fold. Add opt-in lifecycle and acknowledgement controls to bog-serve, preserving default behavior. The worker is a trusted compiled binary owned by the later manager.

**Tech Stack:** Rust 2024, Fold, bog-serve, Serde, Schemars, Axum, Tokio, Postcard.

**Spec:** `docs/superpowers/specs/2026-09-16-personal-bog-cloud-design.md`

## Global Constraints

- Preserve upstream behavior by default; new durability and lifecycle options are opt-in for existing apps.
- Do not rewrite unrelated Fold operators or the existing CLI scaffolder.
- Preserve every acknowledged write against process crash; v1 durable mode checkpoints before success is returned.
- A batch is atomic within one Bog; no transaction spans databases.
- Workers listen on Unix sockets owned by the service account; no public worker ports.

---

## Task 1: JSON storage contract and records template

**Files:** create `cloud-records/Cargo.toml`, `cloud-records/src/lib.rs`, `cloud-records/src/document.rs`, `cloud-records/src/template.rs`, `cloud-records/tests/records.rs`; modify workspace `Cargo.toml` and `Cargo.lock`.

**Interfaces:** export `JsonDocument` (private `serde_json::Value`, object-only checked constructor `try_from_value(Value) -> Result<JsonDocument, DocumentError>` and `as_value(&self) -> &Value`); export `records_router(path: &Path) -> Result<axum::Router, TemplateError>`. Template ID constant is `records-v1`. No registry dependency.

- [ ] Add the new crate and a storage regression test that writes `JsonDocument`, closes/reopens the same temporary store, and retrieves nested values. The meaningful fixture is:

```rust
let document = serde_json::json!({
    "title": "moss 🌿", "done": false, "count": 42,
    "nothing": null, "nested": {"tags": ["a", "b"], "ratio": 1.25}
});
```

Assert exact equality after initial get, replacement, batch, and reopening; include empty objects, arrays inside objects, escaped strings, rejected scalar roots, depth 33, and oversized records. Verify a numeric-looking string key `123` round-trips over HTTP; the existing generic parser tries JSON first and requires explicit handling here.

- [ ] Run `cargo test -p bog-cloud-records --test records`; confirm the storage regression fails before adding the codec (dependency setup must complete before counting a failure).
- [ ] Implement the codec branch explicitly:

```rust
if serializer.is_human_readable() {
    self.as_value().serialize(serializer)
} else {
    serializer.serialize_str(&self.as_value().to_string())
}
```

For deserialization use `Value::deserialize` in human-readable mode and `String::deserialize` followed by `serde_json::from_str` otherwise. Feed both results through the object/depth/size validator. Implement `JsonSchema` using the JSON-object schema, independent of its disk encoding. Test integer extremes supported by serde_json; reject unsupported values rather than coercing them.

- [ ] Compose `KeyedApp<String, JsonDocument, _>` with `terminal::Table::new("docs")` and `terminal::Count::new("total")`. Add template-specific key extraction if required; keep legacy key behavior unchanged. Translate startup errors into typed errors rather than silently deleting data.
- [ ] Use the existing in-process HTTP test pattern (`serve/tests/common/mod.rs`) to verify PUT/GET/DELETE, unchanged count on replacement, mixed batch, whole-batch rejection, bounds, and schema representation. Assert a failed batch leaves both docs and total unchanged.
- [ ] Run `cargo test -p bog-cloud-records` and `cargo test --locked -p bog-serve -p fold`; inspect the scoped diff and checkpoint the JSON/template task.

**Exit:** a reliable JSON document database can run without any cloud manager. The template supports only docs and total, as advertised.

## Task 2: Durable acknowledgement and controlled worker lifetime

**Files:** modify `serve/src/lib.rs`, `serve/src/http.rs`; create `serve/src/lifecycle.rs` if the new lifecycle cannot stay focused in lib; create `serve/tests/durability.rs`, `serve/tests/shutdown.rs`; create `cloud-records/src/bin/bog-records-worker.rs`, `cloud-records/tests/worker.rs`; update affected manifests and lockfile.

**Interfaces:** add an opt-in `Durability::CheckpointBeforeAck` builder setting to both App and KeyedApp. Add `try_into_service() -> Result<ServedApp, ServeError>` with `ServedApp::router()` and `ServedApp::shutdown()` handles so external listeners do not discard lifecycle ownership as `into_router()` currently does. Exact generic erasure remains internal. Existing `into_router()` and `run()` retain compatibility. The worker implements the spec's three CLI flags and signal behavior.

- [ ] Add integration tests for every mutation family: insert/remove/batch, keyed put/delete/batch, successful custom POST, and failed custom POST. Instrument a test-only checkpoint seam to verify success is withheld until checkpoint completes, and a checkpoint failure does not return 2xx. This seam must not be publicly configurable.
- [ ] Run `cargo test -p bog-serve --test durability` and confirm the missing acknowledgement behavior fails.
- [ ] Centralize commit/sequence/notification/checkpoint ordering under the write lock. Publish notification sequences in commit order; do not release the lock and then permit an older writer to notify after a newer one. For failed custom operations do not advance the sequence or checkpoint a nonexistent mutation. Map persistence failures to an unavailable/degraded state and return a structured error; commit outcome may be ambiguous and must not be represented as a rollback.
- [ ] Add a dedicated path for fallible checkpoint propagation from Fold if required; retain existing convenience methods for callers. Test this path with controlled failure injection. Declare the default mode explicitly in docs; do not infer fsync behavior from process-kill tests.
- [ ] Implement the worker's Unix listener, explicit template version, health/schema readiness, and graceful signal handling. Stop accepting new work, drain in-flight requests with a bounded timeout, close long-lived watchers, checkpoint, drop store ownership, and clean up only the worker's own socket. A force-killed worker must recover on restart.
- [ ] Add process tests with a temporary directory/socket: write nested records, force kill, restart/read; send SIGTERM while requests run; reopen after exit; attempt a second worker on the same store and observe failure without data loss. Test that unrelated sockets/directories are untouched.
- [ ] Run `cargo test -p bog-cloud-records --test worker`, `cargo test -p bog-serve --test durability --test shutdown`, then the complete Fold/server/template suites. Record checkpoint latency separately from correctness; checkpoint-before-ack is deliberately conservative.
- [ ] Review the default-compatibility diff and checkpoint this task.

**Exit:** the manager can start, verify, stop, and recover a worker without guessing whether storage is safe to reuse. Acknowledged mutations pass the defined process-recovery tests; power-loss guarantees remain based on documented storage semantics, not the kill test alone.
