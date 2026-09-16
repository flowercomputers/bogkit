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

- Linux integrated suite on the final code revision.
- Tested release image and isolated Fly deployment with persistent volume.
- REST and SDK MCP acceptance over the real HTTPS endpoint from a separate machine.
- Service restart persistence and independent restore on the deployment host.
- Actual natural-language agent-client workflow; protocol/SDK tests alone do not satisfy this gate.
- Record the deployed source revision, image, endpoint, client version and redacted remote results here.

No milestone is marked remotely complete by this report yet. Backups on the same volume are not off-host disaster recovery; no recurring backup schedule has been installed.
