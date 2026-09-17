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

Pending deployment and disposable-fixture verification. The original chat is read-only throughout verification; temporary app access will be revoked.

## Deliberate limits

Counters reset on manager restart. Rolling history and percentiles are bounded and disclose partial coverage. Storage is cached, not a live worker probe. Event recording is best-effort, not a permanent security audit. Per-key tracking, forecasts, Prometheus export, event long-poll and dashboards remain out of scope. Real second-account browser acceptance remains deferred at the user's request; local isolation tests do not claim to replace it. WebMCP tools are tested through the browser-tool harness, not a new signed-in browser acceptance run.

See [diagnostic semantics](../bog-cloud-observability.md) for the exact contract.
