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

## Remaining release gates

- Tested release image and isolated Fly deployment with persistent volume.
- REST and SDK MCP acceptance over the real HTTPS endpoint from a separate machine.
- Service restart persistence and independent restore on the deployment host.
- Actual natural-language agent-client workflow; protocol/SDK tests alone do not satisfy this gate.
- Record the deployed source revision, image, endpoint, client version and redacted remote results here.

No milestone is marked remotely complete by this report yet. Backups on the same volume are not off-host disaster recovery; no recurring backup schedule has been installed.

## Actual Codex client — local

Codex CLI 0.145.0, GPT-5.5, completed 12 actual MCP tool calls against a real loopback listener. A natural-language request created a Bog, wrote/read nested JSON, replaced the document, applied a batch, checked Fold counts (1 → 3 → 2), deleted one record, and read the final two records. Independent REST assertions matched both final objects and the count. Redacted result: `PASS`, fixture `b97b10f8-002f-4145-91c0-9d1d04ea039a` (disposable local root subsequently removed).

The runner uses temporary `-c` connection settings and process-local approval for only the four authorized fixture mutation tools. Other saved servers are disabled for that invocation; the shell sandbox stays read-only. No saved MCP configuration is changed. `scripts/cloud/codex_acceptance.py` supports an optional `BOG_CODEX_MODEL` for compatibility with the installed CLI. The initial default-client attempt could not load the desktop model metadata; the successful run used GPT-5.5. An initial unattended attempt correctly stopped at its mutation-approval prompt; the scoped fixture approval setting enabled the authorized test.

This passes the actual agent-client workflow locally. Remote endpoint/client and service restart/restore evidence are still pending.

## Linux and review completion

The Docker `verify` stage passed the complete Fold, serve, records, cloud and MCP suite on Linux at source revision `56b5aeb`. Image: `sha256:3c688ddc9f836481b755b78711a12ed3bc51b926a15f79e17e95876e8f4af670`. Evidence: `/tmp/bog-cloud-linux-release.log`. Final scoped Clippy checks also passed with warnings denied.

The final review found and reproduced a manager-restart failure involving a temporarily paused surviving worker. Commit `fb131eb` preserves worker ownership and identity through that interruption. Both established-worker and pre-socket startup regression tests passed; the reviewer independently reran the original reproduction and confirmed recovery.

## Fly preparation

Created the isolated `flower-bog-cloud` app in `flower-computer-co` and encrypted 3 GiB `bog_data` volume `vol_vdejp8zk83nw3864` in `iad`. Owner-secret provisioning requires the specific approval requested after automatic approval review rejected that action. Resource creation is not deployment or endpoint acceptance evidence.

Fly's remote Linux amd64 release build succeeded and pushed `registry.fly.io/flower-bog-cloud:deployment-01M2NKS3532VZ86ETV9RXK4406` (32 MB). Runtime source and embedded build revision: `56b5aeb`; the build includes the configuration path correction subsequently committed as `f4bf348`. This was explicitly build-only: no service Machine was deployed and no owner secret was provisioned. Evidence: `/tmp/bog-cloud-fly-build.log`.
