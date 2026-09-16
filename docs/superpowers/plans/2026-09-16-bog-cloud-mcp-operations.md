# Bog Cloud MCP and Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the service usable through an actual MCP client and dependable enough for ongoing personal use.

**Architecture:** MCP is a protocol adapter over the same CloudService as REST. Operator backup/restore pauses a worker and copies a closed, versioned store to a verified archive. Deployment evidence combines real remote use, restoration, recovery, and bounded-load tests.

**Tech Stack:** Rust 2024, official Rust MCP SDK, Axum, existing CloudService/registry/supervisor, cryptographic archive checksums, Python acceptance harness, Linux service deployment.

**Spec:** `docs/superpowers/specs/2026-09-16-personal-bog-cloud-design.md`

## Global Constraints

- REST and MCP invoke the same authorization and operation layer.
- A batch is atomic within one Bog; no transaction spans databases.
- Notifications request a refresh; they are not a durable change log.
- Runtime user data, tokens, registry files, and backups never enter Git.
- Never log authorization headers, raw tokens, or document bodies by default.
- Important-data readiness requires a successful independent restore test.

Prerequisite: management/REST tasks 3–5 pass. Task 6 may be waiting for an external host; that does not prevent local MCP/restore development.

---

## Task 7: MCP adapter and protocol acceptance

**Files:** create `cloud-mcp/Cargo.toml`, `cloud-mcp/src/lib.rs`, `cloud-mcp/src/tools.rs`, `cloud-mcp/src/transport.rs`, `cloud-mcp/src/bin/bog-cloud-server.rs`, `cloud-mcp/tests/protocol.rs`, `cloud-mcp/tests/authorization.rs`, `docs/bog-cloud-mcp.md`; update root manifest/lockfile.

**Interfaces:** `build_mcp_router(Arc<CloudService>) -> axum::Router` serves `/mcp`. Tool handlers construct the same typed Operation values as REST and consume OperationResult. The combined server composes `build_rest_router` and `build_mcp_router` using shared config and service ownership. Keep transport/session state out of the record engine.

- [ ] Check primary SDK documentation and pin a compatible released Rust SDK. Record the version and negotiated protocol in the MCP guide. Verify whether the chosen SDK's session store and middleware preserve per-request credentials; do not assume a session ID establishes identity.
- [ ] Write a protocol test client using the SDK's client facilities. Start a real listener, initialize, send initialized notification, list tools, and call them. A minimum wire-level discovery request is:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
```

Assert the list contains create_bog, list_bogs, describe_bog, get_record, upsert_record, delete_record, read_view, and batch. Assert it does not contain token issuance, arbitrary fetch/exec, instance erase, or search_view for records-v1.

- [ ] Run `cargo test -p bog-cloud-mcp --test protocol --test authorization`; confirm failures reflect missing protocol/tool behavior.
- [ ] Define typed input/output schemas. Map each handler directly to CloudService::execute; derive actor identity from current request authorization, never a tool's supplied arguments. Return structured results and stable tool errors; distinguish malformed JSON-RPC from a valid tool call whose database operation failed. Bound response content and omit credentials/internal paths.
- [ ] Mark read tools read-only. Mark replacements/deletions as potentially destructive; creation retry semantics depend on the required idempotency key. Do not claim every batch is idempotent. Validate Origin against the operator allowlist when present. Require the same bearer token policies as REST; reject mixed-identity session reuse.
- [ ] Test creation retry, CRUD, count view updates, batch failure, cross-Bog access, read-only credentials, expired/revoked credentials, unknown tools, malformed arguments, invalid Origin, excessive result sizes, and existing-session revocation. Confirm sessions cannot bypass the current registry's token status.
- [ ] Run the protocol and authorization suites plus REST tests. Review and checkpoint the adapter task.

**Exit:** a standards-based MCP endpoint passing real transport tests. An HTTP JSON response alone is not sufficient evidence.

## Task 8: One real MCP client and clear connection documentation

**Files:** update `docs/bog-cloud-mcp.md`, `docs/verification/bog-cloud-acceptance.md`; create `cloud-mcp/examples/mcp_acceptance.rs` as a runnable SDK client example.

**Interfaces:** one endpoint URL and supported client's protected bearer configuration; no credentials committed in sample configuration. Configuration snippets use environment variable names/placeholders such as `BOG_CLOUD_TOKEN`, never real secrets.

- [ ] Document tested client, SDK/protocol version, and exact supported connection method based on current official client documentation. State the private static-token boundary. Do not advertise one-click OAuth installation or compatibility with untested clients.
- [ ] Connect the user's selected existing MCP client to the private endpoint using its secure configuration mechanism. If connection requires changing user settings, show the exact bounded change first; do not overwrite unrelated servers.
- [ ] Through the real client, create a disposable database, write nested JSON, read it, replace it, inspect total, batch, and delete a record. Check results independently through REST to prove both interfaces see the same data.
- [ ] Revoke a test token and verify the existing client session loses access. Keep the operator's owner token intact.
- [ ] Save redacted tool results and mark M2 only after actual client interaction. If client integration is unavailable, retain protocol test evidence but label the actual-client gate pending.

**Exit:** the natural-language agent workflow is demonstrated, not just the server protocol.

## Task 9: Backup, independent restore, and recovery operations

**Files:** create `cloud/src/backup.rs`, `cloud/src/restore.rs`, `cloud/src/bin/bog-cloud-admin.rs`, `cloud/tests/backup_restore.rs`, `cloud/tests/archive_validation.rs`; update `cloud/src/registry.rs`, `cloud/src/supervisor.rs`, `docs/bog-cloud-operations.md`.

**Interfaces:** operator-only administrative actions `backup <bog-id>` and `restore <archive-id> --name <name>`. The admin client uses a local Unix control socket owned by the running manager, invoking methods on that same registry/supervisor; it must not start a second manager or mutate a live registry from another process. Archive IDs resolve under an operator-configured directory. Manifest fields: format_version=1, template_id, template_version, source_bog_id, build_commit, created_at, and per-file SHA-256 digests/sizes. Restored instances always receive a new UUID and newly issued tokens.

- [ ] Add tests: write several records and mutate them, back up, restore to a fresh instance, compare complete records and total; kill during staging; fail disk copy; corrupt a byte; reject incompatible template; refuse overwriting an existing destination. Assert the source returns to its previous desired state after both successful and failed backups.
- [ ] Run `cargo test -p bog-cloud --test backup_restore --test archive_validation`; confirm missing recovery/archive behavior fails.
- [ ] Implement maintenance transition, request draining, graceful worker stop, closed-store copy plus adjacent schema metadata, hash verification, atomic completion marker, and restart in unconditional cleanup logic. Failed/incomplete archives stay outside the completed-backup catalogue. The stopped store is the consistency boundary; do not copy a running store and call it consistent.
- [ ] Implement extraction into a newly allocated staging directory. Validate every archive entry before writing: reject absolute/traversal paths, links, duplicates, files outside the root, unsupported formats, and expanded content beyond configured storage limits. Verify digests before creating a registry restore intent. Never import source credentials or discovery sockets/PIDs.
- [ ] Register a new restoring instance, start the exact template version, compare records/views, and transition to ready only on success. A failed restore remains failed and cannot replace the source. Add recovery handling for manager restart during each backup/restore stage.
- [ ] Run the complete suite, then perform an independent restore on the deployment host into a new instance. Measure downtime and archive size with the fixture size recorded. Checkpoint code and append redacted evidence.

**Exit:** important-data recovery is demonstrated; backup file existence alone does not pass.

## Task 10: Limits, deployment hardening, and release acceptance

**Files:** create `cloud/tests/limits.rs`, `scripts/cloud/recovery_acceptance.py`; update `cloud/src/config.rs`, `cloud/src/http.rs`, `cloud/src/supervisor.rs`, `deploy/bog-cloud/bog-cloud.service`, `.github/workflows/cli.yml`, `docs/bog-cloud-operations.md`, `docs/verification/bog-cloud-acceptance.md`.

**Interfaces:** operator-configurable bounds from the spec, stable overload errors, structured request logs, readiness reflecting registry availability and supervisor state. Individual failed Bogs remain visible without making healthy Bogs inaccessible.

- [ ] Add tests at each exact limit and one above it: 256-byte key, 256-KiB document, depth 32, 1-MiB body, 100 operations, 1,000 list limit, 10,000 offset, 8 active instances, 2 concurrent starts. Account for UTF-8 bytes rather than character count. Include output bounds and long-lived watcher limits if watch is enabled.
- [ ] Run `cargo test -p bog-cloud --test limits`; verify missing enforcement fails. Implement bounded requests, worker starts, connection pools, timeouts, and log retention. Reserve sufficient disk before provisioning and backup; surface disk-full errors without claiming a successful write or backup. Choose the disk reserve from the actual host inventory and record it in deployment configuration.
- [ ] Exercise interrupted provisioning, duplicate manager startup, worker crash, invalid schema/template, forced restart, corrupt backup, credential revocation, and disk-full simulation using temporary test filesystems/failure seams. Run unsafe resource-exhaustion simulations only in isolated test roots, never by filling the user's host disk.
- [ ] Extend CI path filters to all new crates and lockfiles. Run Linux process/socket tests, formatting, scoped Clippy, and `cargo test --locked -p fold -p bog-serve -p bog-cloud-records -p bog-cloud -p bog-cloud-mcp`. Keep existing CLI scaffolding coverage. Pin the target Rust toolchain only after verifying all selected dependencies compile together.
- [ ] Build and deploy the tested commit. Run the roadmap's full remote/MCP/restore acceptance transcript. Verify restart-on-failure, startup after host service restart, intended network binding, and persistence on the configured volume. Record exact scope and avoid availability/scale claims beyond measured tests.
- [ ] Document routine start/stop/status, token rotation/revocation, backup schedule, restore, failed-start diagnosis, capacity changes, and rollback. Template upgrades use a separate new instance and validated record transfer, preserving the source. Scheduling actual recurring backups is an explicit deployment action, not implied by the presence of an example timer.
- [ ] Mark M3 complete only when every final acceptance gate passes. Review the final changes and hand off commands, endpoint, deployed version, tested client, limitations, and evidence location without secrets.

**Exit:** a bounded personal cloud database service with REST, MCP, recovery, and operational ownership. Public multi-user hosting and custom code builds remain separate projects.
