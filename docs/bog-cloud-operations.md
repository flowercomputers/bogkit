# Bog Cloud operations

The first service is a private, single-owner records platform. A Bog is a separate Fold store and native worker process on one persistent host, not a separate virtual machine. Only the compiled `records-v1` template is allowed. One manager owns the registry and all workers; adding a second replica is unsupported.

## Build and run locally

```sh
cargo build --locked -p bog-cloud-records --bin bog-records-worker -p bog-cloud --bins -p bog-cloud-mcp --bin bog-cloud-server
export BOG_CLOUD_ROOT=/tmp/bog-dev
export BOG_WORKER_BINARY="$PWD/target/debug/bog-records-worker"
export BOG_CLOUD_BIND=127.0.0.1:8080
# Supply BOG_CLOUD_OWNER_TOKEN from a protected secret source (32+ random bytes).
./target/debug/bog-cloud-server
```

Keep the root short: worker Unix sockets must fit within 100 bytes. It must be an absolute real directory, owned by the operator; the service makes it private. Never point two managers at one root or open a live store from another process. SIGTERM drains requests and checkpoints workers. Restart reconciliation recovers desired running Bogs and adopts verified surviving workers. Three failed starts produce a visible failed status.

`BOG_CLOUD_MAX_ACTIVE` defaults to 8; `BOG_CLOUD_MAX_STARTS` to 2; `BOG_CLOUD_MIN_FREE_BYTES` to 67108864 (64 MiB). Set these before starting the manager. Backups also reserve their complete copy size plus 64 MiB. The reserve reduces disk exhaustion risk but does not reserve blocks atomically against unrelated host processes. Actual write/checkpoint failures do not produce successful acknowledgements.

## HTTP contract

Every `/v1` request requires `Authorization: Bearer ...`. POST and PUT require JSON content type. Creation requires an `Idempotency-Key`; repeating the same normalized request returns the same resource ID. A changed body with the same key conflicts. Creation returns 202; poll the resource until `status` is `ready` before using it.

| Method and path | Purpose |
| --- | --- |
| `POST /v1/bogs` | Owner creates `{name,template:"records-v1"}`. |
| `GET /v1/bogs` | Owner lists resources and states. |
| `GET /v1/bogs/{id}` | Describe an authorized resource. |
| `GET /v1/bogs/{id}/schema` | Actual worker schema and template version. |
| `GET`, `PUT`, `DELETE /v1/bogs/{id}/docs/{key}` | Read, completely replace, or remove a JSON object. |
| `POST /v1/bogs/{id}/batch` | Array of `{op:"upsert",key,data}` / `{op:"remove",key}`; all operations validate before one transaction. |
| `GET /v1/bogs/{id}/views/docs?limit=100&offset=0` | Fold's document table. |
| `GET /v1/bogs/{id}/views/total` | Fold's maintained count, including replacement/retraction semantics. |
| `POST /v1/bogs/{id}/tokens` | Owner issues `{scope:"read"}` or `{scope:"write"}`; raw token returned once. |
| `DELETE /v1/bogs/{id}/tokens/{token-id}` | Owner revokes immediately. |

Records use nonempty UTF-8 keys up to 256 bytes; control characters, slash, `.` and `..` are rejected. Percent-encode keys in HTTP paths. Objects are at most 256 KiB with nesting depth 32. Request bodies: 1 MiB; batches: 100 operations; page limits: 100 default, 1000 maximum; offset: at most 10000. Record-view responses stop at a bounded 4 MiB before materializing larger results. MCP has a tighter serialized result limit; request a smaller page when advised. At most 64 shared operations run concurrently.

Errors carry a stable code and request ID: 400 validation, 401 credential failure, 403 insufficient scope, 404 missing/out-of-scope resource, 405 method, 409 conflict, 413 oversized content, 429 capacity, 503 unavailable. Request logs contain IDs/status/duration, not headers or record bodies. `/healthz` checks registry access. A failed individual Bog remains visible without blocking healthy ones.

Tokens are random and only digests are stored in the registry. Scoped tokens cannot create/list all Bogs or issue tokens. Static credentials have revocation, not time-based expiry. Rotate a Bog credential by issuing a replacement, updating its client, and revoking the old token. Rotate the owner secret by changing its protected host configuration and restarting; existing scoped credentials continue to work. There is no public signup, OAuth installation, per-person ownership, arbitrary Rust execution, database erase, or public watch endpoint.

## Backups and restores

Run the admin CLI on the manager host. It connects to the private `admin.sock` and requires the owner token. It never starts another manager or directly edits the live registry.

```sh
bog-cloud-admin backup BOG_UUID
bog-cloud-admin restore ARCHIVE_UUID --name recovered-example
```

The manager drains that Bog, stops the worker, verifies its logical records/count, and holds the storage engine's OS lock while copying the closed store. Completed backups are UUID directories below `BOG_CLOUD_ROOT/backups`. A versioned manifest records file sizes, SHA-256 checksums, template, source identity, build revision and a logical record/count digest. Incomplete staging directories are not backups and are cleaned at startup. The source resumes after copy success or failure; an unconfirmed worker stop fails closed and remains explicitly failed.

Restore validates paths, regular files, digests, version and a 1 GiB expanded-size limit, then allocates a fresh UUID. It copies into that new instance, verifies logical records and count before startup, and checks the worker's real schema. It never imports credentials, sockets or process identity. A failed/interrupted restore is left failed and cannot replace the source. Issue new scoped credentials for a restored Bog.

Copy completed backup directories to independent protected storage for host-loss recovery. Backups retained only on the same Fly volume are not disaster recovery. No recurring backup job is installed by these files. Retention and off-host storage remain explicit operator choices.

## Fly deployment

`deploy/bog-cloud/fly.toml` targets a separate `flower-bog-cloud` app in Flower Computer Co, region `iad`, one shared CPU / 1 GiB RAM machine and one persistent volume mounted at `/data`. The entrypoint initializes only `/data/bog`, then drops to UID 10001 before starting the service. Only the authenticated gateway port is exposed through Fly HTTPS; worker/admin sockets remain local. Automatic stop is disabled; restart policy is always. Do not enable automatic horizontal scaling or multiple Machines against independent volumes: that would produce different registries.

Build/deploy from a tested commit, with that revision in `BOG_BUILD_COMMIT`. Set `BOG_CLOUD_OWNER_TOKEN` using Fly secrets through stdin, never a literal command argument. `BOG_CLOUD_ALLOWED_HOSTS` and `BOG_CLOUD_ALLOWED_ORIGINS` must match the endpoint exactly. Fly logs provide request status history; export/retention is a separate operator setting.

Run from the repository root; Fly resolves the configured Dockerfile relative to the configuration file, while the explicit working directory supplies the repository build context:

```sh
flyctl deploy . --config deploy/bog-cloud/fly.toml --ha=false --build-arg BOG_BUILD_COMMIT="$(git rev-parse HEAD)"
```

The initial encrypted 3 GiB volume has Fly's automatic snapshots enabled with five-day retention. These platform snapshots are separate from the application's checked, closed-store backups and do not establish a tested disaster-recovery procedure.

A restart uses the same volume and credentials. For rollback, stop the new service, retain the volume and backups, and deploy a previously verified compatible image. Never roll back across unknown storage/template versions in place. Template changes use a new instance with validated record transfer and preserve the source.

Actual deployment, client and recovery evidence is recorded in [acceptance](verification/bog-cloud-acceptance.md). Prepared configuration alone does not establish a live deployment.

## Connect to the deployed service

The verified endpoint is `https://flower-bog-cloud.fly.dev`; MCP uses `/mcp`. The approved owner credential is in Fly Secrets and in the private local file `~/.config/bog-cloud/flower-bog-cloud.env` (mode 0600). Do not commit or share that file. To load it into a terminal session without printing the token:

```sh
source ~/.config/bog-cloud/flower-bog-cloud.env
export BOG_CLOUD_TOKEN="$BOG_CLOUD_OWNER_TOKEN"
export BOG_CLOUD_URL=https://flower-bog-cloud.fly.dev
```

Use the [MCP connection instructions](bog-cloud-mcp.md#tested-codex-cli-workflow) for Codex. Ordinary application clients should receive a scoped credential for their Bog through the owner-only token endpoint. The owner token can create resources and access all Bogs.

The first deployment retains five synthetic acceptance Bogs, leaving three of the initial eight slots available. Their IDs are in the acceptance report. Instance deletion, additional templates and off-host backup scheduling remain follow-up work; the running service currently supports the fixed records template and its document/count Fold views.
