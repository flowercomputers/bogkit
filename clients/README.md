# Bog Cloud clients (local preview packages)

Both packages are server-side clients. Node 22+ and Python 3.9+ are supported.
No package has been published to a registry.

Install from this checkout:

```sh
python3 -m pip install ./clients/python
npm install ./clients/typescript
```

Both install a `bog-cloud` command; install them in separate environments if you
want to compare CLIs. Python imports `Client` from `bog_client`; TypeScript imports
`Client` from `@bog/cloud`. TypeScript declarations accompany the dependency-free
JavaScript runtime, so no compiler or build step is required by consumers.

```sh
bog-cloud --auth-file /private/path/auth.json connect
bog-cloud --auth-file /private/path/auth.json create --name notes --idempotency-key stable-request-key
bog-cloud --config /private/path/app.json resources
bog-cloud --config /private/path/app.json batch-get first missing
bog-cloud --config /private/path/app.json list --after last-key --limit 20
bog-cloud --config /private/path/app.json search --resource text_search --query hello
bog-cloud --config /private/path/app.json wait --cursor opaque-cursor --timeout 25
```

The Python CLI accepts space-separated values after `--fields` and
`--include-fields`; the Node CLI accepts repeated flags. JSON request input uses
`--input path.json` (or `--input -` for stdin). Output is JSON; approval instructions
and generic failures go to stderr. `create` returns the Bog ID after readiness;
it does not mint a credential. Use the private app-access installer to deliver an
app-scoped credential. `bog_project.py` combines create and that installer.

Private app configuration accepts JSON or dotenv containing `BOG_CLOUD_URL`,
`BOG_ID`, and `BOG_CLOUD_TOKEN`. Files must be owned regular files with mode 600;
symlinks are refused. Dotenv is parsed as data and never sourced by a shell.
Authorization uses a separate JSON file with `origin`, `access_token`, and numeric
Unix-seconds `expires_at`. Tokens are only sent to the configured HTTPS origin
(loopback HTTP is allowed); redirects are refused. TS `writePrivate` uses exclusive
creation and supports JSON or dotenv. Device polling accepts nested and flat error
codes. Existing authorization can be used through `Client.fromAuth`.

Methods cover resources/routes, get/put/remove/batch, batch_get (`batchGet` in TS),
list with exclusive after/before endpoints, ranked search with include_fields,
changes/wait, diagnostics, request lookup, idempotent creation/readiness, and
additive search updates that retain existing resources and revision checks.
Reads preserve server envelopes including order, scores, duplicate batch keys,
and null missing values. Errors never echo arbitrary service bodies. Network
failures are not retried: a write may already have succeeded. Keep creation keys
stable and inspect state/jobs before attempting an uncertain operation again.

`../starters/cloud-notebook` and `../starters/cloud-notebook-ts` keep credentials on
the server. `../starters/cloud-chat` shows change notification and history paging.
The fixtures in `fixtures/requests.json` drive both clients' real loopback HTTP tests.

Lifecycle and private delivery commands:

```sh
bog-cloud --auth-file /private/path/auth.json create --name sandbox-demo --idempotency-key stable-demo --sandbox --wait --app-access read
bog-cloud --auth-file /private/path/auth.json install --handoff HANDOFF_ID --output /private/path/app.env --format dotenv
bog-cloud --config /private/path/app.env top --resource priority --limit 10 --include-fields /title
bog-cloud --config /private/path/app.env query --resource docs --input query.json
bog-cloud --auth-file /private/path/auth.json explain REQUEST_ID
bog-cloud --auth-file /private/path/auth.json cleanup-preview --name-prefix sandbox-
bog-cloud --auth-file /private/path/auth.json cleanup-execute --preview-id PREVIEW_ID --confirm PREVIEW_ID
bog-cloud --auth-file /private/path/auth.json --bog-id SANDBOX_ID delete-sandbox --confirm SANDBOX_ID
```

Creation defaults to waiting for readiness; `--no-wait` returns current creation
state immediately. `--sandbox` creates a temporary ownable sandbox.
`--app-access read|write` asks the server for a nonsecret private-delivery handoff;
`--app-label` sets its label. The handoff remains available in the creation result
when readiness is polled. `install` consumes it once and saves the credential to a
private file, never stdout. Node installation requires an existing matching auth
file and new output path; Python also supports `--replace` through its existing
private installer. A failed delivery after redemption requires revocation and a
new handoff, never retrying the old redemption.

`explain` aliases `request-status`; an expired or missing observation is not proof
that a write failed. Cleanup execution requires the exact preview ID as confirmation
and uses the server's frozen preview, not a fresh prefix match. Sandbox deletion
requires confirming its exact ID; the server enforces creator/owner permissions.
These commands do not issue raw credentials to stdout.

`command-manifest.json` is generated from the server's shared operation table.
Run `python3 clients/generate-command-manifest.py --check` to detect drift. An
explicit actual-service canary is available at `typescript/test/service-smoke.mjs`;
set `BOG_TEST_ORIGIN` and `BOG_TEST_AUTH_FILE`. It creates and removes its own
sandbox and prints only a check summary, never the private authorization file.

Ranked `top` calls omit record values by default. Request `include_fields` in Python
or the fourth `includeFields` argument in TypeScript (CLI `--include-fields`) to
receive pointer-keyed values. Hosted query and batch-get responses use `data`.

Both request layers retry only HTTP 429 with finite nonnegative numeric
`retry_after_ms` or `Retry-After` guidance: at most two retries within a 30-second
request budget, preserving method, body and idempotency key. A missing or excessive
delay is returned to the caller. Conflicts and uncertain transport failures are
never retried. `BogError` retains the delay as `retry_after_ms` (Python) or
`retryAfterMs` (TypeScript).

The equivalent Python actual-service canary is `python/service_smoke.py`.
`cargo test -p bog-cloud --test client_journeys` runs both canaries against fresh
local services and actual workers after building `bog-records-worker`; identity
provider responses are mocked locally, so this does not verify GitHub approval.
