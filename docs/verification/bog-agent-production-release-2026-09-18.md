# Agent-experience production release — 2026-09-18

## Candidate and verification

Source: `c72c1aecc5619f058afb36e700bb70bf8f651e9d` on `codex/personal-bog-cloud`.
The branch was pushed to GitHub before rollout. The release includes the seven
agent-experience implementation commits plus Linux/CI client prerequisites.

Fresh Linux amd64 verification completed successfully with the deployment
Dockerfile's `verify` target: 326 Rust tests, 38 Python tests, and 35 JavaScript
tests. This includes both clients' real-service journeys, permission checks,
sandbox lifecycle tests, discovery contracts, and runtime reads/search. Docker
configuration validation reported no warnings. The runtime image still contains
only the Rust service binaries; Python and Node are test-stage dependencies.

## Rollout evidence

Production is running Fly release **v28** on the original machine
`4d895395c393e8` and encrypted 3 GiB volume `vol_vdejp8zk83nw3864`.
The running image digest was checked against the uploaded candidate:
`sha256:20f8f34a52da9411c96de6e90b614549d46e7bd2deb1f01cc8fd7a6fada6e4da`.
The image was built with `BOG_BUILD_COMMIT=c72c1aecc5619f058afb36e700bb70bf8f651e9d`.
The production configuration enabling sandboxes is commit `5fefc88`.
Health checks pass and authentication remains configured.

Before replacement, the machine was cordoned and gracefully stopped. Fly
completed recovery snapshot `vs_2ZAgZoBbBDjsQKN5vogN`, created at
2026-09-18T19:12:26Z, with digest
`1e9b8ef41b308ca707581311387b11b61db89c70d12a4c9b05ef1bc27d130b0b`.
Snapshot retention is five days. The machine was uncordoned after deployment.

The first deployment kept sandbox creation disabled. A disposable production Bog
created before the upgrade passed 68 setup checks, then 45 post-upgrade HTTP/MCP
checks covering persisted records, lexical and semantic search, definition
updates, credential scope, and revocation. Its exact ID was retained privately;
cleanup passed and removed only that Bog. The original six legacy Bog IDs were
compared before and after and were unchanged.

After those checks, `BOG_CLOUD_SANDBOXES=true` was enabled using the same image.
Python and TypeScript each completed their real production journey sequentially:
create a ready sandbox; redeem a handoff into a private mode-600 dotenv file;
write records; read ordered batches including missing and duplicate keys; read
key ranges; fetch projected ranked and search results; wait for changes; inspect
routes and diagnostics; and reject a write through the read-only credential.
Both clients deleted their own sandbox. No sandbox remained in the test workspace.
Existing agent authorization continued to work. A Bog request could be looked up
through the bounded observation endpoint. `/v1/me` requests are not Bog observations
and correctly return no matching observation when looked up.

Public discovery checks passed on the production domains: OAuth issuer and
protected-resource metadata, canonical sign-in callback, MCP challenges, console
redirects, all local links advertised by `/llms.txt`, and Markdown negotiation
with `Vary: Accept` on `/` and `/docs`. `/llms.txt` is 1,107 bytes and the practical
agent guide is 3,238 bytes.

No ordinary user Bog was modified or deleted. The service still uses one shared
CPU and 1 GiB memory; no production capacity increase was made. After sandbox use,
rollback must retain an expiry-aware binary, as documented in the operations guide.
The pre-release snapshot is a recovery point, not permission to overwrite newer
production data.

## External publication and host checks

npm authentication is absent (`npm whoami` returned `ENEEDAUTH`). No PyPI
publishing credentials are configured. Neither registry publication has been
attempted. Client source and packaging are available on the GitHub branch.
Actual MCP Apps rendering in a compatible host and a fresh OAuth host onboarding
remain external checks; protocol tests do not substitute for those checks.
