# Bog Cloud acceptance evidence

Branch: `codex/personal-bog-cloud`, based on Sam's `sam/vibe-bog-serve` at `5a0fb83`.

## Local evidence — 2026-09-16

- Fold/serve regression, records codec, durable-before-ack, schema mismatch/I/O, process isolation, competing writers, SIGTERM and abrupt worker kill tests passed.
- Worker shutdown with an unfinished request exits nonzero within its hard deadline and leaves the owned socket for safe recovery.
- Registry idempotency persists across reopen and concurrent connections; credentials are digest-only, scoped and revocable. Live operation checks re-evaluate revocation.
- Real loopback REST acceptance passed creation/retry, nested JSON, replacement, Fold count updates, batch rejection atomicity, successful batch/delete, scope isolation and revocation.
- Manager/worker restart tests preserved acknowledged records and scoped credentials. A slow worker startup did not block reads to a healthy Bog. Failed binary startup retried three times and became explicitly failed.
- Official rmcp 3.4.0 SDK initialized over Streamable HTTP, negotiated `2025-11-25`, discovered exactly eight tools, and completed CRUD/batch/view operations. REST independently saw the same data. Revoking a token blocked the already-connected client. Exact Origin/Host, session, argument and response-bound tests passed.
- Closed-store backup restored into a new identity; full record table and count matched. Production restore verifies a logical digest/count before starting the worker. Corrupt backup data was rejected. Failed copy resumed the source. Interrupted restore stayed failed; surviving-worker adoption and incomplete staging recovery passed.
- Storage-exhaustion simulation used an impossible configured reserve in a temporary directory; no disk-filling operation was performed.

These are local checks, not deployment evidence. Raw development logs are under `/tmp/bog-cloud-*.log`; they are not committed and contain no intentional credentials.

## Release gates

The initial single-host release gates passed, including deployment, remote REST/MCP, actual Codex-client use, independent restore, and Machine restart persistence. Evidence follows below. Backups on the same volume are not off-host disaster recovery; no recurring application backup schedule has been installed.

## Actual Codex client — local

Codex CLI 0.145.0, GPT-5.5, completed 12 actual MCP tool calls against a real loopback listener. A natural-language request created a Bog, wrote/read nested JSON, replaced the document, applied a batch, checked Fold counts (1 → 3 → 2), deleted one record, and read the final two records. Independent REST assertions matched both final objects and the count. Redacted result: `PASS`, fixture `b97b10f8-002f-4145-91c0-9d1d04ea039a` (disposable local root subsequently removed).

The runner uses temporary `-c` connection settings and process-local approval for only the four authorized fixture mutation tools. Other saved servers are disabled for that invocation; the shell sandbox stays read-only. No saved MCP configuration is changed. `scripts/cloud/codex_acceptance.py` supports an optional `BOG_CODEX_MODEL` for compatibility with the installed CLI. The initial default-client attempt could not load the desktop model metadata; the successful run used GPT-5.5. An initial unattended attempt correctly stopped at its mutation-approval prompt; the scoped fixture approval setting enabled the authorized test.

This local result was followed by the successful remote checks recorded below.

## Linux and review completion

The Docker `verify` stage passed the complete Fold, serve, records, cloud and MCP suite on Linux at source revision `56b5aeb`. Image: `sha256:3c688ddc9f836481b755b78711a12ed3bc51b926a15f79e17e95876e8f4af670`. Evidence: `/tmp/bog-cloud-linux-release.log`. Final scoped Clippy checks also passed with warnings denied.

The final review found and reproduced a manager-restart failure involving a temporarily paused surviving worker. Commit `fb131eb` preserves worker ownership and identity through that interruption. Both established-worker and pre-socket startup regression tests passed; the reviewer independently reran the original reproduction and confirmed recovery.

## Fly preparation

Created the isolated `flower-bog-cloud` app in `flower-computer-co` and encrypted 3 GiB `bog_data` volume `vol_vdejp8zk83nw3864` in `iad`. Owner-secret provisioning was subsequently explicitly approved and completed through Fly Secrets. A private local copy is stored outside the repository with mode 0600.

Fly's remote Linux amd64 release build succeeded and pushed `registry.fly.io/flower-bog-cloud:deployment-01M2NKS3532VZ86ETV9RXK4406` (32 MB). Runtime source and embedded build revision: `56b5aeb`; the build includes the configuration path correction subsequently committed as `f4bf348`. This initial build-only step was followed by deployment of the same image. Evidence: `/tmp/bog-cloud-fly-build.log`.

## Live deployment and remote acceptance — 2026-09-16

- Origin: https://flower-bog-cloud.fly.dev; MCP: https://flower-bog-cloud.fly.dev/mcp.
- Organization: `flower-computer-co`; Machine: `4d895395c393e8`, region `iad`; one shared CPU, 1 GiB RAM, encrypted 3 GiB persistent volume.
- Deployed runtime source: `56b5aeb`; image tag above, digest `sha256:3f7fb3528b92d117a88256c70e1e79b6fc9bdf39c2df331ecb53cc3b7c05991b`. Later commits update deployment configuration, acceptance scripts and documentation without changing runtime source.
- The operator's Mac acted as a separate remote client over HTTPS. REST acceptance passed idempotent creation, nested objects, replacement, Fold counts, scope isolation, invalid-batch atomicity, batch/delete and revocation.
- Official rmcp 3.4.0 client passed creation/CRUD/batch/views, independent REST comparisons, and revocation of an already-connected client; negotiated protocol `2025-11-25`.
- Codex CLI 0.145.0 with GPT-5.5 completed 12 real MCP calls; independent REST assertions verified final records and counts. Saved client configuration was not changed.
- Closed-store backup `ad8ec5d7-6915-45a8-b742-3f0bd610059b` restored source `d733dcdc-ce0f-4f6f-8cd4-768f26ff1a38` into independent Bog `3786ca96-eebc-4839-8066-c5f32207f85a`. Full document table and count matched; source-scoped credentials could not access the restored Bog.
- Restarted the Fly Machine with SIGTERM; both stores and the test scoped credential survived. The test credential was then revoked. An initial restart command was rejected by Fly for exceeding its 60-second timeout limit; the corrected command passed.
- After restart: HTTPS health returned 200; unauthenticated REST and MCP returned 401. Fly's service health check passed.

Redacted evidence: `/tmp/bog-cloud-fly-deploy.log`, `/tmp/bog-cloud-remote-rest.log`, `/tmp/bog-cloud-remote-sdk.log`, `/tmp/bog-cloud-remote-codex.log`, `/tmp/bog-cloud-remote-recovery.log`. Recovery runner: `scripts/cloud/fly_recovery_acceptance.py`.

Five acceptance Bogs remain for inspection and consume five of the initial eight instance slots: REST fixtures `d733dcdc-ce0f-4f6f-8cd4-768f26ff1a38` and `4206f626-5bb6-461d-896a-685b0d0dfed0`, SDK fixture `b73a147b-29af-4de8-a6c2-1e854959e5d8`, Codex fixture `792ef450-9a95-41d4-9177-720601253cd8`, and the restored Bog above. Only synthetic test records are present. Instance deletion is not an implemented API; do not repeatedly run creation acceptance tests against this bounded service.

## Public getting-started guide — 2026-09-16

Deployed runtime revision `adc5949`, image `registry.fly.io/flower-bog-cloud:deployment-01M2NTDYBCJ8PKZR46X95J09AD`. The root page now explains agent and HTTP usage, with copyable examples, scoped-token guidance, endpoint reference and current limitations.

Verified desktop and 390px mobile layouts in Chrome, copy feedback and expandable examples. The exact create/write/read/views/batch/delete snippets passed against a disposable local gateway, with final record and count assertions. Extended REST tests passed for public asset types/security headers while protected API authorization remains enforced. After deployment, all public assets returned 200, unauthenticated REST/MCP returned 401, and existing synthetic records matched their pre-deployment contents. Fly health and DNS checks passed; the live page and copy control were inspected in Chrome. No new live Bog was created for this documentation test.
