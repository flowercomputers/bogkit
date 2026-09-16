# Personal Bog Cloud design

Status: proposed execution design; planning only. No cloud resources have been created.

## Goal and success criteria

From a remote machine, create a persistent Bog through a web API, use it as a small record database backed primarily by Fold, and repeat the workflow through MCP. Changes to records must update Fold views atomically. A second database must have independent data and credentials. Service restart must preserve both databases and their endpoints.

The first useful release is a private, single-owner service on one persistent Linux host. macOS supports local development. Remote means an actual second machine, not a second local process. A local-only result does not complete the remote milestone.

## Verified starting point

Base: `sam/vibe-bog-serve`, commit `5a0fb83de9a9633a74f4e6c559ef52a2fd432206`.
Working branch: `codex/personal-bog-cloud`.

The September 16 investigation passed `cargo test --locked -p bog-serve -p fold` with socket access. A temporary HTTP probe verified typed keyed CRUD, derived counts, batch rejection without partial writes, schema discovery, and process-kill/restart persistence. Direct `serde_json::Value` storage failed on read through Postcard. No remote provisioning, MCP, power-failure tolerance, or backup recovery was verified.

Existing implementation anchors:
- `serve/src/lib.rs`: App/KeyedApp, router integration, TCP/Unix sockets, daemon lifecycle.
- `serve/src/http.rs`: transactions, generated routes, RwLock access, notifications.
- `serve/src/openapi.rs`: generated schemas and structural fingerprint.
- `fold/src/stream/keyed.rs`: primary records and atomic propagation to views.
- `fold/src/stream/unkeyed.rs`: transactions and checkpoint.
- `cli/src/new.rs`: local source generation; not a hosted provisioner.

## Architecture and alternatives

Chosen: a small Rust management service, one trusted compiled worker process and data directory per Bog, with HTTP and MCP sharing one authorized operation service. Workers listen on Unix sockets. An authenticated gateway is the only remotely reachable application port. Fold remains the record and view engine.

A single process containing every database would simplify initial routing but couples crashes and complicates per-instance restart and resource accounting. Arbitrary user-supplied Rust builds would preserve maximum flexibility but add a build service and untrusted-code execution. Both are excluded from the first release.

Worker separation is a failure/lifecycle boundary, not protection against malicious code running under the same OS identity. Only operator-built, allowlisted templates are executable.

The manager has a durable registry under a configured service root. Use SQLite for registry transactions, unique names, provisioning state, and idempotency records; this is management metadata only. User documents and derived views live in Fold. This avoids making recovery of the manager depend on the workers it must recover. No separate network database is required.

## Project-wide requirements

- Preserve upstream behavior by default; new durability and lifecycle options are opt-in for existing apps.
- Do not rewrite unrelated Fold operators or the existing CLI scaffolder.
- One persistent host; no horizontal replication or cross-Bog transactions in v1.
- Stable external IDs are generated UUIDs; filesystem paths never derive from display names or request-supplied paths.
- Only template IDs in the operator's compiled allowlist may start a worker.
- REST and MCP invoke the same authorization and operation layer.
- Workers listen on Unix sockets owned by the service account; no public worker ports.
- The gateway binds to loopback by default; remote exposure requires explicit deployment configuration.
- Enforce bearer credentials even on a private network; use HTTPS for remote access.
- Runtime user data, tokens, registry files, and backups never enter Git.
- Never log authorization headers, raw tokens, or document bodies by default.
- Preserve every acknowledged write against process crash; v1 durable mode checkpoints before success is returned.
- A snapshot read is consistent within one request; separate requests need not share a snapshot.
- A batch is atomic within one Bog; no transaction spans databases.
- Notifications request a refresh; they are not a durable change log.
- Important-data readiness requires a successful independent restore test.

## Components and file ownership

New workspace crates:

| Directory / crate | Responsibility |
|---|---|
| `cloud-records/` / `bog-cloud-records` | JSON storage wrapper, versioned records template, worker binary |
| `cloud/` / `bog-cloud` | registry, authorization, worker supervision, shared operations, REST gateway |
| `cloud-mcp/` / `bog-cloud-mcp` | MCP transport and tools calling the shared operations |

The gateway binary composes `bog-cloud` and `bog-cloud-mcp`; keep its entry point in `cloud-mcp/src/bin/bog-cloud-server.rs` to avoid a dependency cycle. A REST-only `cloud/src/bin/bog-cloud.rs` supports the earlier milestone. Configuration and gateway route construction are shared, not duplicated.

Existing changes are limited to workspace membership, the new opt-in serve durability/lifecycle contract, tests, CI, and docs. New crates remain unpublished initially.

## Records template: `records-v1`

Primary key: a nonempty UTF-8 string, maximum 256 bytes; reject control characters and slash. Clients URL-encode keys. The edge and worker must agree that numeric-looking IDs such as `123` remain strings; override the existing JSON-first key parser for this template if necessary.

Record value: a JSON object, maximum encoded size 256 KiB; maximum nesting depth 32. Preserve JSON values across write, read, replacement, batch, and restart. Store JSON with an explicit codec: the wrapper serializes as JSON to human-readable HTTP serializers and as encoded JSON text to Postcard. Its schema advertises an object, not the internal string representation.

Views: `docs` (key/value listing) and `total` (record count). Listing order is implementation-defined; do not promise alphabetical or numeric key order. A future template may add searchable text and configured summaries. Ordinary records do not secretly gain arbitrary query/filter support.

Initial limits: request body 1 MiB, 100 batch operations, list limit 100 by default / 1,000 maximum, offset maximum 10,000, 8 simultaneously active Bogs, and 2 concurrent starts. These are configurable operator limits, not measured capacity claims. Refuse excess work with a stable error rather than silently truncating it.

## Public REST contract

All routes below are under `/v1`. Database IDs are UUIDs. Responses include `request_id`; errors have `{ "error": { "code": "...", "message": "..." }, "request_id": "..." }`.

| Method / path | Input / result |
|---|---|
| `POST /bogs` | `{name, template:"records-v1"}` plus `Idempotency-Key`; 202 returns `{id,name,template,status:"creating",api_url}` |
| `GET /bogs` | Owner-only list of registry records |
| `GET /bogs/{id}` | State: creating, ready, stopped, failed, restoring, or maintenance; sanitized failure reason |
| `POST /bogs/{id}/tokens` | Owner-only; scope read or write; returns secret once and stable token ID |
| `DELETE /bogs/{id}/tokens/{token_id}` | Owner-only revoke; 204 |
| `GET /bogs/{id}/schema` | Actual template input/views and explicit template version |
| `PUT /bogs/{id}/docs/{key}` | Full JSON-object replacement; `{seq,replaced}` |
| `GET /bogs/{id}/docs/{key}` | `{seq,data}`; missing record 404 |
| `DELETE /bogs/{id}/docs/{key}` | `{seq,removed}` |
| `POST /bogs/{id}/batch` | Upsert/remove operations; one atomic commit |
| `GET /bogs/{id}/views/{name}` | Bounded `limit` and `offset`; template views only |
| `GET /bogs/{id}/watch` | Optional refresh notifications with restart-aware instance generation |

Owner credentials authorize management and all records. Per-Bog read tokens authorize only that Bog's reads; write tokens additionally authorize its writes. Invalid/missing authentication is 401; authenticated insufficient scope is 403; a resource outside a token's database scope is 404. A known but unavailable Bog returns 503 with `Retry-After`. The manager does not forward arbitrary caller URLs, paths, headers, or HTTP methods.

Creation idempotency is scoped to owner identity and a canonical body hash. Reusing a key with the same body returns the original ID; a changed body is 409. Persist creation intent before starting the worker. Resource responses never contain another instance's credential or internal socket path.

Generate tokens with an OS cryptographic RNG, keep only cryptographic hashes, and compare without timing-dependent equality. Provision the bootstrap owner credential via a protected file/environment mechanism, not a CLI argument or committed file. No account registration, billing, or user login UI in v1.

## Worker and recovery contract

`bog-records-worker --data-dir <absolute> --socket <absolute> --template-version records-v1` starts the trusted template. The manager also supplies an instance UUID and a private startup nonce through a protected configuration source. It reports identity, health, and schema over the socket; the nonce must never appear in public API responses or logs. It must support graceful SIGTERM/SIGINT, draining accepted writes and checkpointing before exit, and durable acknowledgement on every mutation route.

The manager owns all starts and stops. Startup reconciliation uses store locks plus health/schema verification, never PID alone. One instance cannot acquire another instance's directory. Persist desired state and recover failed/interrupted starts using bounded retries. Do not mark ready until health and template version match. When stopping, block new requests, drain, checkpoint, and wait for process exit before releasing storage or restarting.

Use desired state `running|stopped` and observed state separately. Creation -> creating -> ready or failed. Boot reconciliation re-establishes desired running instances, preserving IDs, keys, and data. Do not enable idle shutdown until race-free activation and retry behavior are verified.

## MCP contract

Use the official Rust MCP SDK after checking its then-current released API and dependency compatibility; pin the selected release in Cargo.lock. The investigated baseline is MCP 2025-11-25 Streamable HTTP. Implement actual initialization, tool discovery, structured results, and protocol errors rather than treating REST JSON as MCP.

Mount `/mcp` at the same authenticated gateway. Private v1 clients must support a configured bearer credential. A broader one-click installation flow requires a subsequent OAuth/discovery implementation and client compatibility tests; do not label static-token support as universal MCP authorization.

Tools:
- `create_bog(name, template, idempotency_key)` -> creation result.
- `list_bogs()` and `describe_bog(bog_id)` -> authorized instances/schema/status.
- `get_record(bog_id,key)`, `upsert_record(bog_id,key,data)`, `delete_record(bog_id,key)`.
- `read_view(bog_id,view,limit,offset)` and `batch(bog_id,operations)`.
- `search_view` is advertised only after a search-capable template exists.

Do not expose credential minting, arbitrary code execution, filesystem access, instance erasure, or indefinite watch operations as v1 tools. An MCP session is not an authorization credential. Apply permissions on every tool call; revoke tokens for established sessions as well as new ones. Validate Origin for HTTP MCP and set bounded outputs. Tool annotations describe behavior but do not enforce permissions.

## Backup, restore, and upgrade contract

Initial backup is an operator command, not an MCP tool. Put the instance in maintenance, drain writes, gracefully stop it, and copy the closed store plus required schema metadata and a versioned manifest. Restart in a finally-style cleanup path. Stage and hash the archive before publishing it as complete; a failed copy is never a valid backup.

Restore to a new instance ID and directory, issue new credentials, verify manifest/hash/template compatibility, and compare records/views. Reject archives with absolute paths, traversal, links escaping the destination, duplicate entries, excessive expanded size, or unsupported format versions. Do not overwrite an existing instance. Restrict backup filesystem paths to operator configuration.

Templates are immutable versioned artifacts. A changed calculation is a new version even when input/output schemas are identical. Upgrade by copying/exporting authoritative records to a separate target instance, validating results, and explicitly switching the consumer; retain the old instance for rollback. Never use WipeAndRebuild for authoritative cloud records. A general migration engine is outside v1.

## Milestones and exclusions

M1: remote REST provisioning and records on one private host, using disposable test data.
M2: repeat the same workflow through one real MCP client.
M3: restart/reconciliation, scoped access, backup/restore, resource limits, and deployment checks pass; ready for personal ongoing use.
M4: search template, richer Fold summaries, token rotation UX, and optional dashboard/configurable views.

Exclude public multi-user hosting, billing, arbitrary Rust uploads, SQL compatibility, general joins/query planning, distributed replication, public npm onboarding changes, and graphical administration from M1–M3.

Deployment target and hostname are selected when entering the remote milestone. Default planning assumption: an existing private persistent Linux host with HTTPS/private-network access. No purchase, public exposure, firewall change, or mutation of an existing service is implied by this planning request.
