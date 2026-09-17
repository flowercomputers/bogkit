# Diagnose a Bog through HTTP, MCP or WebMCP

Start with one call: MCP `bog_metrics` with `bog_id` and optional `window: "1h"`, or `GET /v1/bogs/{id}/metrics?window=1h`. The other supported window is `5m`. Supply `workspace_id` explicitly for shared workspaces. Signed-in browser agents have the equivalent WebMCP tool; no dashboard is required.

The snapshot reports request counts grouped by operation and public credential ID, errors by code with a few request IDs, sampled server-side latency, active change waiters and outcomes, cached worker state, cached storage usage, and limits labelled by their actual scope. Human browser sessions are grouped as `human_session`, not individually identified.

## Interpret the numbers accurately

- `observed_since` and `reset_at` identify the current manager's observation start. Counters reset when the manager restarts; `window_complete: false` means a full window is unavailable.
- `truncated: true` means observations were dropped to keep memory bounded. Counts then describe retained observations, not total traffic. The collector retains at most 20,000 requests and 512 operation/credential/Bog combinations globally.
- Percentiles use up to the latest 128 samples per series, with `sample_count` and `method: "recent_samples_nearest_rank"`. Sparse samples are not a reliable service-level estimate. Empty samples have null percentiles.
- Errors include up to 32 recent correlated request IDs. Requests rejected before reaching the shared operation layer, including invalid authentication and malformed transport arguments, are outside this snapshot. Foreign-Bog attempts are not attributed to the target Bog.
- Metrics and event reads do not count toward observed request traffic. They still authenticate and use the normal request allowance.
- A long poll intentionally waits. `waits.duration_ms` includes that wait; `write_ack_to_release_ms` measures only observed matching writes acknowledged after the wait started, in the same worker generation. It ends at server-side release, not delivery to a browser. Check its sample count. Initial cursor acquisition is distinct from a timeout.
- Reading diagnostics does not wake a sleeping worker or extend its idle deadline. Storage is cached from explicit usage reads and may be unavailable or stale; inspect `cached_at`. Worker transition reasons are reported only when known.
- Rate and worker limits may apply across an account or the whole service, rather than to this Bog. The snapshot labels that scope; response rate-limit headers provide the current request allowance.

`list_tokens` now includes `last_used_at`, rounded down to the minute. It records observed authorized Bog access, including diagnostics; null means no retained observation, not proof that a credential was never used. Activity survives manager restart and has a bounded 20,000-credential history.

## Permissions

Workspace members and authorized delegated agents can inspect their selected workspace's Bog metrics. Single-Bog app credentials can inspect coarse worker/storage state and **only their own** traffic, errors and active waits. They cannot read operational events, inspect other tokens' activity, provision Bogs or issue credentials.

No diagnostic response records record keys, record contents, tokens, cookies or authorization headers. Public credential IDs are references for management, never credential secrets.

## Operational history

Use MCP/WebMCP `bog_events` or `GET /v1/bogs/{id}/events?limit=50`. The limit is 1–100. Pass the returned opaque `next_cursor` on the next request. `has_more` indicates another page is available now. If `reset` is true, older history is no longer available; rebuild your local view from the returned page.

History is retained for **at most** seven days, 1,000 events per Bog and 20,000 events service-wide. These are upper bounds, not guaranteed retention under load. The separate, bounded diagnostics database survives normal manager restarts. Cursors detect recreation of that database. Existing activity before this release is not reconstructed.

This history includes accepted provisioning attempts (`bog_provision_requested`, including safe retries), credential issuance/revocation, membership additions/removals affecting existing Bogs, capacity rejections, and known worker transitions. Bulk credential revocation on membership removal is represented by the membership event. Deleted Bogs become inaccessible, including their event endpoint. Event persistence is best-effort and is not a security audit guarantee. It is not a record change feed or permanent audit archive. Continue using `wait_for_change` for application updates. There is no event long-poll, Prometheus export, growth forecast or per-key tracking in this release.

## An agent's triage sequence

1. Call `bog_metrics` once. Check coverage, worker state and active waiters.
2. Compare error codes and sampled latency across operations; identify a noisy credential using its public ID.
3. With workspace access, call `bog_events` to correlate known lifecycle changes. Inspect `list_tokens` for coarse last use.
4. Report the relevant request IDs and observation interval. Avoid claiming a quiet or incomplete window proves health.

Load authorization privately from your existing credential store. Do not put it in prompts, URLs, source files or diagnostic reports.
