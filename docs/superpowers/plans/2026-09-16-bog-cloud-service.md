# Bog Cloud Management and REST Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create, discover, and use isolated records Bogs remotely through an authenticated API.

**Architecture:** A durable registry records creation intent and permissions before a supervisor starts trusted workers. A common service resolves caller permissions and resource identity before dispatching bounded operations over Unix sockets. The REST adapter maps these operations to the versioned contract.

**Tech Stack:** Rust 2024, Axum/Tokio, SQLite through a pinned Rust binding, OS-generated tokens, Unix HTTP client, existing records worker.

**Spec:** `docs/superpowers/specs/2026-09-16-personal-bog-cloud-design.md`

## Global Constraints

- One persistent host; no horizontal replication or cross-Bog transactions in v1.
- Stable external IDs are generated UUIDs; filesystem paths never derive from display names or request-supplied paths.
- Only template IDs in the operator's compiled allowlist may start a worker.
- REST and MCP invoke the same authorization and operation layer.
- The gateway binds to loopback by default; remote exposure requires explicit deployment configuration.
- Enforce bearer credentials even on a private network; use HTTPS for remote access.
- Never log authorization headers, raw tokens, or document bodies by default.

Prerequisite: storage plan tasks 1–2 pass. The API table and exact limits in the spec are binding.

---

## Task 3: Durable registry, idempotent creation intent, and credentials

**Files:** create `cloud/Cargo.toml`, `cloud/src/lib.rs`, `cloud/src/types.rs`, `cloud/src/config.rs`, `cloud/src/registry.rs`, `cloud/src/auth.rs`, `cloud/migrations/001_registry.sql`, `cloud/tests/registry.rs`, `cloud/tests/auth.rs`; update root manifest/lockfile.

**Interfaces:** `BogId` wraps UUID; `TemplateId` admits only `records-v1`; `Scope` is Read or Write; `Principal` is owner or an authenticated database token. Export `Registry::open(&Path) -> Result<Registry, CloudError>`, `Auth::authenticate(&str) -> Result<Principal, CloudError>`, and registry methods for create intent, status transition, token issue/revoke. All errors use `CloudError { code, message }`; adapters select HTTP/protocol representation.

- [ ] Add tests covering create retry after reopen, conflicting body for the same idempotency key, concurrent duplicate creation, unique normalized display names, unknown templates, token revocation after reopen, and malformed credentials. Use separate fresh temporary roots for every test.
- [ ] Run `cargo test -p bog-cloud --test registry --test auth`; confirm the missing transaction/auth behavior is what fails.
- [ ] Create the initial registry schema with foreign keys, transactions, and version tracking:

```sql
CREATE TABLE bogs (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
  template TEXT NOT NULL, template_version TEXT NOT NULL,
  desired_state TEXT NOT NULL, observed_state TEXT NOT NULL,
  generation INTEGER NOT NULL DEFAULT 0, failure_code TEXT,
  created_at TEXT NOT NULL
);
CREATE TABLE create_requests (
  owner_id TEXT NOT NULL, request_key TEXT NOT NULL,
  body_hash TEXT NOT NULL, bog_id TEXT NOT NULL REFERENCES bogs(id),
  PRIMARY KEY(owner_id, request_key)
);
CREATE TABLE tokens (
  id TEXT PRIMARY KEY, bog_id TEXT NOT NULL REFERENCES bogs(id),
  secret_hash BLOB NOT NULL UNIQUE, scope TEXT NOT NULL,
  revoked_at TEXT, created_at TEXT NOT NULL
);
```

Enable foreign keys and configure the registry for durable transactions. A new creation transaction inserts both the Bog and the idempotency row. Reopening must not recreate or reset the database. Whitelist states in application types and database checks. Normalize names consistently and bound names/idempotency keys to 128 bytes.

- [ ] Generate 32-byte random token secrets through an OS-backed RNG and encode them for transport. Store a cryptographic digest of the high-entropy token; do not use the schema fingerprint hash. Return plaintext only at issuance. Hash the provided bootstrap owner token at startup; require it outside test mode. Use constant-time digest comparison.
- [ ] Enforce read/write/owner capabilities in one authorization function, including access to schema, watch, and status. Failed cross-Bog access must not reveal its existence. Revocation must take effect without a service restart. Inspect logs with seeded sentinel token/document strings and assert neither appears.
- [ ] Run focused tests, inspect the scoped diff, and checkpoint the registry/auth task.

**Exit:** creation and credentials have durable semantics without needing to start any worker.

## Task 4: Worker supervisor and crash reconciliation

**Files:** create `cloud/src/supervisor.rs`, `cloud/src/worker_client.rs`, `cloud/src/reconcile.rs`, `cloud/tests/supervisor.rs`, `cloud/tests/recovery.rs`.

**Interfaces:** `Supervisor::ensure_running(BogId) -> Result<WorkerEndpoint, CloudError>`, `Supervisor::stop(BogId) -> Result<(), CloudError>`, and `Supervisor::reconcile() -> Result<RecoveryReport, CloudError>`. `WorkerEndpoint` is an internal Unix socket path plus template version/generation, never client-provided. The registry supplies desired state. WorkerClient exposes typed record/schema/view operations, not arbitrary URL forwarding. Add optional `instance_id` and private `startup_nonce` registry fields with a versioned migration before reconciliation uses them; these fields are never part of public registry responses.

- [ ] Add process tests for simultaneous activation, manager kill between persisted intent and spawn, manager kill after spawn before ready, worker crash, stale socket, locked store, wrong template version, and exhausted active-instance limits. Inject failures at named transitions to make crash tests deterministic.
- [ ] Run `cargo test -p bog-cloud --test supervisor --test recovery`; verify incomplete state transitions fail.
- [ ] Derive each instance directory from a validated UUID under the service root. Create service directories with restrictive permissions; refuse symlink escapes. Acquire a manager singleton lock so two managers cannot supervise the same root. Spawn only an absolute operator-configured binary with fixed argument positions, restricted environment, and no shell.
- [ ] Implement a per-instance activation mutex and global start semaphore. Persist creating before spawn, poll health/schema with a 15-second deadline, and verify template version before ready. Bound restart attempts to 3 with backoff; expose failure after exhaustion. For local development permit a configurable longer readiness deadline without an unbounded loop.
- [ ] Reconcile desired-running records at manager startup. A responsive stale worker is adopted only after identity/template/instance checks; PID alone is insufficient. If identity cannot be proved, fail closed with a diagnostic rather than signaling a possibly unrelated process. Extend the worker health identity response from the storage plan to include instance ID and a manager-issued startup nonce; store and verify the nonce securely.
- [ ] Stop by removing the instance from request dispatch, draining requests, signaling the verified child, and waiting for exit. A shutdown timeout is a failed stop with an explicit operator diagnostic, not permission to delete data. Cleanup must target only the verified instance socket.
- [ ] Run process tests on macOS and Linux CI. Assert no duplicate writers, orphaned test children, leaked sockets, or reset data. Review and checkpoint.

**Exit:** the manager recovers interrupted work and preserves stable IDs without exposing any public API yet.

## Task 5: Shared operations and REST routes

**Files:** create `cloud/src/service.rs`, `cloud/src/http.rs`, `cloud/src/error.rs`, `cloud/src/bin/bog-cloud.rs`, `cloud/tests/rest.rs`, `scripts/cloud/rest_acceptance.py`; update crate manifests.

**Interfaces:** export `CloudService::execute(principal: &Principal, operation: Operation) -> Result<OperationResult, CloudError>` as an async method. Define tagged `Operation` variants CreateBog, ListBogs, DescribeBog, GetRecord, UpsertRecord, DeleteRecord, ReadView, Batch, IssueToken, and RevokeToken in `types.rs`, with fields exactly matching the spec. Define matching typed result variants and adapters. `build_rest_router(Arc<CloudService>) -> axum::Router` mounts `/v1` routes; the service rechecks current token validity on every operation.

- [ ] Add HTTP tests before handlers. Cover every route in the spec, all authorization combinations, numeric-looking keys, missing records, unavailable workers, invalid template/view names, oversized/deep JSON, oversized batches, limit/offset boundaries, and invalid content types. Status mappings must be explicit: validation 400, missing/invalid credentials 401, insufficient scope 403, out-of-scope resource 404, conflicting idempotency/name 409, request size 413, capacity 429, worker unavailable 503.
- [ ] Run `cargo test -p bog-cloud --test rest`; confirm route/permission failures before implementation.
- [ ] Implement Operation validation and authorization before registry lookup or worker dispatch where possible. Validate against the allowlisted template contract; cap response size as well as request size. Return a generated request ID for correlated errors. Use a bounded Unix-socket HTTP connection pool and distinguish transport failures from a worker's legitimate 404.
- [ ] Implement REST using the shared service, plus explicit owner-only token issuance/revocation. Do not proxy arbitrary custom routes. Keep all responses free of internal paths/credentials. For optional watch streams, authorize the target on connection, bound subscriptions, include instance generation, and close on credential revocation; omit the public watch route until these checks pass.
- [ ] Create a standard-library Python acceptance runner using environment-provided base URL and token, with HTTP timeouts and no secret output. Its first core assertions must follow:

```python
# request(method, path, body, token, headers) is the runner's HTTP helper.
created = request('POST', '/v1/bogs',
                  {'name': run_name, 'template': 'records-v1'}, owner_token,
                  {'Idempotency-Key': run_id})
retried = request('POST', '/v1/bogs',
                  {'name': run_name, 'template': 'records-v1'}, owner_token,
                  {'Idempotency-Key': run_id})
assert created['id'] == retried['id']
# Poll status to ready with a bounded deadline before data operations.
```

Extend the runner to the final acceptance transcript, stopping before backup/MCP checks owned by the next plan. Use fresh IDs; never clean an unrelated database. Do not implement instance deletion just for a test convenience; test roots are removed by the process harness after all children exit.
- [ ] Run `cargo test -p bog-cloud` and the acceptance runner against a real loopback listener and fresh root. Restart worker and manager within the process harness and rerun read/auth assertions. Run Fold/server regression suites once after the integrated changes pass. Review and checkpoint.

**Exit:** a locally verified instance-management service; do not call it remotely verified yet.

## Task 6: First private remote deployment

**Files:** create `deploy/bog-cloud/bog-cloud.service`, `deploy/bog-cloud/config.example.toml`, `docs/bog-cloud-operations.md`, `docs/verification/bog-cloud-acceptance.md`.

**Interfaces:** one configured HTTPS base URL backed by a private gateway, persistent service root, protected owner credential source, and pinned worker binary. No provider-specific provisioning API is required.

- [ ] Inspect the chosen existing host and remote test machine read-only. Record OS, available disk, persistent directory, private connectivity, and existing HTTPS termination. Present exact deployment changes if they affect an existing service; obtain any still-missing authorization before applying those changes.
- [ ] Build release binaries for the target architecture, record commit and hashes, and install under versioned paths. Configure a non-root service account, restart-on-failure, restrictive filesystem permissions, and bounded logs. Keep gateway and worker binaries on matching versions.
- [ ] Configure HTTPS/private access without exposing worker sockets/ports. Verify the gateway only accepts the intended network path. Bootstrap credentials through a protected channel; keep secrets out of shell history and acceptance reports.
- [ ] From a genuinely separate machine, run `scripts/cloud/rest_acceptance.py` against the HTTPS endpoint. Confirm TLS validation, creation, scoped reads/writes, malformed-batch rejection, and cross-Bog isolation. Repeat reads after restarting the test service.
- [ ] Capture the redacted transcript and deployed commit. Mark M1 complete only if these actual remote checks pass. If no second machine/host is available, record local completion and the exact external dependency; continue independent MCP work without fabricating remote evidence.

**Exit:** remotely usable REST with disposable data. Important personal data waits for phase 3 restore/recovery verification.
