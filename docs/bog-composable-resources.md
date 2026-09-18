# Composable Bogs

Composable Bogs build several resources from one collection of string-keyed JSON objects. A write changes the source and its derived resources together. The example todo definition produces an open-task list, its count and a priority ranking. Its internal statistics resource is deliberately absent from the exposed API.

This is an opt-in prototype. Set `BOG_CLOUD_COMPOSABLE=true` on the cloud manager. Existing `records-v1` Bogs continue to use their existing APIs. No hosted rollout is implied by these examples.

## Discover without a checkout

Authenticate using the service's `/auth.md`. `GET /v1/components` returns `enabled`, the complete `definition_schema`, effective limits, and full `examples.todo`, `examples.todo_search` and `examples.todo_semantic` JSON definitions. These examples are compiled into the service; a fresh agent does not need this repository. Use the catalog's actual limits when adapting an example. `POST /v1/definitions/validate` accepts `{"definition": ...}`; it does not accept a bare definition.

MCP offers the same catalog as `discover_capabilities` and the resource `bog://guide/components`. Continue with `validate_definition`, `create_bog_from_definition`, `describe_definition`, `list_resources`, `query_resource`, `search_resource`, `plan_definition_update`, `apply_definition_update` and `definition_update_status`. Signed-in WebMCP offers these names with `bog_` prefixes. Resource discovery includes exposed operation request and response schemas. Address a query/search by resource name, and use an `action` for a query when needed to distinguish exposed reads. Keep private resource names out of application assumptions.

When `enabled` is false, do not attempt configurable creation or definition changes. Existing Bogs remain available. Management authority is required to inspect or change definitions; matching app read credentials may discover and query their Bog's exposed resources.

The hosted preview at https://cloud.bog.new has composition enabled as of 2026-09-18. Connect remote agents at https://mcp.bog.new/mcp. Check the live component catalog for effective availability and limits; the local manager default remains disabled.

## Run locally

The local runner serves the same configured runtime on loopback. Run from the repository root:

```sh
cargo run --locked -p bog-cloud-records --bin bog-defined -- \
  --data-dir /tmp/bog-todo-local \
  --definition-file docs/examples/composable/todo-semantic.json \
  --port 7877
```

In another terminal:

```sh
curl -fsS http://127.0.0.1:7877/batch \
  -H 'Content-Type: application/json' \
  --data-binary @docs/examples/composable/todo-records.json
curl -fsS http://127.0.0.1:7877/operations/open_count \
  -H 'Content-Type: application/json' --data '{}'
curl -fsS http://127.0.0.1:7877/operations/semantic_search \
  -H 'Content-Type: application/json' \
  --data '{"query":"publish software changelog","limit":3}'
```

The count is two and the semantic result ranks `release` first. Stop the runner with Ctrl-C. The data directory persists between runs; reopen it with the same definition. The local runner exposes only configured operations and blocks internal worker controls.

The standalone runner accepts the same limit fields through `BOG_COMPOSABLE_LIMITS`; the hosted manager uses `BOG_CLOUD_COMPOSABLE_LIMITS` as described below.

To receive local change notifications, expose a wait operation before starting a fresh local store:

```sh
jq '.expose.wait = {target:"docs",action:"wait"}' \
  docs/examples/composable/todo-semantic.json > /tmp/todo-with-wait.json
cargo run --locked -p bog-cloud-records --bin bog-defined -- \
  --data-dir /tmp/bog-todo-with-wait \
  --definition-file /tmp/todo-with-wait.json --port 7878
```

Acquire a cursor, then pass it back to wait for a change:

```sh
curl -fsS http://127.0.0.1:7878/operations/wait \
  -H 'Content-Type: application/json' --data '{}' > /tmp/todo-cursor.json
jq '{cursor:.data.cursor,timeout_ms:25000}' /tmp/todo-cursor.json | \
  curl -fsS http://127.0.0.1:7878/operations/wait \
    -H 'Content-Type: application/json' --data-binary @-
```

The response contains `seq` and `data` with `seq`, `cursor`, `changed` and `reset`. Omit the cursor to acquire the current state immediately. The default wait is 25 seconds, with a maximum of 30 seconds. Keep cursors opaque; a restart sets `reset`, so refetch the resource. Notifications indicate source changes and do not provide durable event replay.

## Run the todo example

Run from the repository root, with `BASE` set to your service URL and `TOKEN` to your existing management credential. Commands use `curl` and `jq`.

```sh
curl -fsS "$BASE/v1" | jq .
curl -fsS "$BASE/v1/components" -H "Authorization: Bearer $TOKEN" | jq .
curl -fsS "$BASE/v1/definitions/validate" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data "$(jq -c '{definition:.}' docs/examples/composable/todo.json)" | jq .
jq -n --slurpfile definition docs/examples/composable/todo.json \
  '{name:"todo",definition:$definition[0]}' > /tmp/todo-create.json
curl -fsS "$BASE/v1/bogs" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: todo-composition-1' \
  --data-binary @/tmp/todo-create.json > /tmp/todo-created.json
BOG_ID=$(jq -r .id /tmp/todo-created.json)
curl -fsS "$BASE/v1/bogs/$BOG_ID/batch" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data-binary @docs/examples/composable/todo-records.json
curl -fsS "$BASE/v1/bogs/$BOG_ID/resources" -H "Authorization: Bearer $TOKEN" | jq .
for resource in open open_count priority; do
  curl -fsS "$BASE/v1/bogs/$BOG_ID/resources/$resource/query" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    --data '{}' | jq .
done
```

The sample contains three tasks. Two remain open; “Ship release” has the highest open-task priority. The open list exposes only title and priority. The private statistics resource has no exposed operation and cannot be queried through the public API.

## Add search to existing records

The second definition preserves every existing resource and operation and adds a BM25 text index. Send the current revision to avoid overwriting another definition change.

```sh
curl -fsS "$BASE/v1/bogs/$BOG_ID/definition" \
  -H "Authorization: Bearer $TOKEN" > /tmp/todo-definition.json
jq -n --slurpfile current /tmp/todo-definition.json \
  --slurpfile definition docs/examples/composable/todo-search.json \
  '{expected_revision:$current[0].revision,definition:$definition[0]}' > /tmp/todo-update.json
curl -fsS "$BASE/v1/bogs/$BOG_ID/definition/plan" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data-binary @/tmp/todo-update.json | jq .
curl -fsS "$BASE/v1/bogs/$BOG_ID/definition/apply" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data-binary @/tmp/todo-update.json > /tmp/todo-job.json
JOB_ID=$(jq -r .job_id /tmp/todo-job.json)
curl -fsS "$BASE/v1/bogs/$BOG_ID/definition/jobs/$JOB_ID" \
  -H "Authorization: Bearer $TOKEN" | jq .
# Once the job has succeeded:
curl -fsS "$BASE/v1/bogs/$BOG_ID/resources/text_search/search" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data '{"query":"changelog","limit":10}' | jq .
```

Definition edits require management authority. A read credential can query exposed resources but cannot write source records or change definitions. Existing resources remain readable during a build; writes may be paused and must be retried after activation. Do not treat acceptance of a job as successful activation: inspect the persistent job result. `succeeded` means the verified revision was durably activated; the Bog status separately reports worker availability. `failed` retains the previous active revision and resumes writes. `recovery_required` means the build failed but the worker could not confirm that writes resumed: keep retrying reads, leave writes paused, and recover/restart the manager. Restart recovery marks the interrupted job failed and resumes a surviving worker before accepting new writes. Do not attempt to bypass the pause with a different credential.

`todo-semantic.json` adds the fixed semantic index to the text-search definition. The pinned model and tokenizer are verified and embedded when the binary is built; no model asset provisioning is needed on the runtime host. The acceptance test exercises this definition with the pinned ESE model, including search after restart and backup restoration.

## Data rules and limits

Fields use JSON Pointers, such as `/title` and `/priority`. Stages run in order. Filters exclude missing values from ordered comparisons. Missing fields differ from explicit null; missing terminal fields are skipped, whereas incompatible present terminal types reject the entire write. Search combines selected strings with newlines.

Definitions allow at most 16 resources, eight stages per resource and one semantic index. Semantic indexes allow at most 10,000 vectors. Extracted text is bounded at 8 KiB, queries at 4 KiB and search responses at 50 hits. Existing source, request and batch limits remain in force.

Operators can lower these ceilings using JSON configuration before starting the manager:

```sh
export BOG_CLOUD_COMPOSABLE_LIMITS='{"resources":8,"stages_per_resource":4,"vectors":1000,"text_bytes":4096,"query_bytes":2048,"hits":20,"build_timeout_seconds":120}'
```

Omitted fields use the defaults: `resources=16`, `stages_per_resource=8`, `vectors=10000`, `text_bytes=8192`, `query_bytes=4096`, `hits=50` and `build_timeout_seconds=300`. Values must be positive and no greater than their default; unknown fields are rejected. `GET /v1/components` advertises the effective limits. Existing Bogs remain readable after ceilings are lowered. New definitions and writes enforce the policy; when source or vector usage is already over its limit, shrinking it is allowed.

Inspect usage with an authorized credential:

```sh
curl -fsS "$BASE/v1/bogs/$BOG_ID/usage" \
  -H "Authorization: Bearer $TOKEN" | jq .
```

`logical_bytes` measures the source JSON and keys. `physical_store_bytes` measures files in the active store directory, including storage overhead; it does not measure the whole Bog directory. `vector_payload_bytes` is only the raw 512-dimensional f32 payload; it excludes search graph and storage overhead. These are different measures and should not be added together. `derived_data_bytes` is null because Fjall shares files across source and derived keyspaces; `derived_data_bytes_reason` explains that limitation.

Only additive definition changes are supported. Preserve the original resources and exposed operations when adding a new resource. Derived resources are rebuilt from existing source records; callers do not have to reinsert them. Backups retain the active definition along with source data; restores create a separate Bog and do not extend an old Bog credential to the restored one. After the first activation, the original `data` store and any `data.schema` remain as a rollback artifact. This adds at most one retained original store per Bog; its bytes are excluded from `physical_store_bytes`. Unselected rebuild candidates are reclaimed during manager recovery. Backup exports include the active revision, not the retained original artifact.

## Verification

The fixture is shared by `cloud/tests/composable_acceptance.rs`. Run:

```sh
cargo build -p bog-cloud-records --bin bog-records-worker
cargo test -p bog-cloud --test composable_acceptance
```

The process test uses a temporary service root and real worker processes. Backup and restore use the manager's administrative interface directly; they are not public HTTP routes. The acceptance test also adds the semantic fixture and checks actual search before and after restart. Building the model-backed runtime requires the pinned model assets; the resulting binary embeds them. `scripts/cloud/verify_composable.sh` runs the wider regression checks.
