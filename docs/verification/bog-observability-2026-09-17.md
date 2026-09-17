# Agent-first Bog observability verification

## Scope

HTTP, MCP and signed-in WebMCP expose `bog_metrics` and `bog_events`. Token listing adds coarse persisted `last_used_at`. No visual dashboard or paid infrastructure was added. Registry format is unchanged; diagnostic history uses a separate bounded database.

## Local verification

- Cloud and MCP regression suites cover authorization, persistence, lifecycle and actual HTTP/MCP transport. Final run: 122 passed, zero failures.
- All 15 browser JavaScript tests pass, including new diagnostic argument validation, explicit workspace routing, read-only behavior and session revalidation.
- New backend tests exercise app credential isolation, foreign-target rejection, revoked membership, no worker wake, minute-debounced activity persistence, event retention/paging/restart, bounded observations and latency samples, waiter cancellation and causal release timing.
- Independent review found and resolved per-request SQLite work and misleading creation events on retries. Provisioning events are explicitly named `bog_provision_requested`; they count accepted attempts, including safe retries.
- Root-level strict lint initially also checked unchanged dependencies and found existing `anny` warnings. Strict relevant-package lint passed with `--no-deps`.

## Live verification

Deployed `1d528c5` to the existing Fly machine `4d895395c393e8`; health and smoke checks passed. No host expansion.

Live HTTP checks passed: metrics and event discovery, explicit windows, active waiters, a write releasing a waiter, scoped request attribution, error/request-ID correlation, event pagination, coarse token activity, cross-Bog denial and app-event denial. The temporary record was deleted and credential `43e70fb3-29fc-4fc9-921b-193b592adaa3` revoked; subsequent use returned 401. One causal waiter-release sample was observed.

Live MCP initialize, tool discovery and `bog_metrics` passed through the deployed HTTP transport using an already approved identity. This is not a new independent Codex/Claude onboarding trial.

A second fixture remained sleeping with unchanged generation after metrics and event reads. Public discovery checks passed. The original chat's records matched their pre-deployment fingerprint.

Sanitized sample request IDs: `49c23e80-d031-4e29-bffd-96cef531adbe`, `d084c39f-c372-43b6-9740-f23fe84b4e07`, `d9d9bf2e-9b80-4854-ade2-f58a8dbf6cc2`.

## Deliberate limits

Counters reset on manager restart. Rolling history and percentiles are bounded and disclose partial coverage. Storage is cached, not a live worker probe. Event recording is best-effort, not a permanent security audit. Per-key tracking, forecasts, Prometheus export, event long-poll and dashboards remain out of scope. Real second-account browser acceptance remains deferred at the user's request; local isolation tests do not claim to replace it. WebMCP tools are tested through the browser-tool harness, not a new signed-in browser acceptance run.

See [diagnostic semantics](../bog-cloud-observability.md) for the exact contract.
