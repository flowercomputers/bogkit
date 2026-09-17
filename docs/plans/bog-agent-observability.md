# Agent-first Bog observability

User direction: agents are the primary consumers; no dedicated visual dashboard. HTTP/MCP and machine-readable contracts first.

## Scope and finishing criteria

1. One authorized, read-only per-Bog snapshot (`GET /v1/bogs/{id}/metrics`, `bog_metrics`) for 5m/1h windows: bounded traffic/error/latency measurements, credential attribution, wait health, worker lifecycle, cached storage and correctly scoped limits.
2. Coarse persisted token `last_used_at`; no synchronous persistence write per request. No record keys, contents or secrets in diagnostics.
3. Bounded retained operational events (`GET /v1/bogs/{id}/events`, `bog_events`) with opaque paging, retention and reset semantics. Seven days, at most 1,000 events per Bog and 20,000 globally. Not a data-change replay feed.
4. Workspace members/delegated agents get their authorized Bog's detail; app credentials get only their own credential traffic plus coarse resource state. Apps cannot inspect administrative events or other credentials.
5. Diagnostics never start a sleeping worker or extend its idle deadline. Rolling counters are in-memory, honestly report partial observation/reset; retained events and token activity use an additive separate SQLite file. Registry format unchanged.
6. HTTP, MCP and WebMCP share permissions/contracts; documentation gives an agent one-call diagnosis examples. No dashboard, Prometheus export, event long-poll, seven-day growth forecasts, or per-key tracking in this release.
7. Verify boundaries, boundedness, windows, paging/restart, waiter cancellation, request correlation, last-use persistence and no-wake behavior. Review; deploy on existing host; run live diagnostics and preserve existing chat data.

## Work ownership

- Core worker: collector/persistence/lifecycle/wait hooks and shared operations.
- Interface worker: HTTP/MCP routes, schemas wiring and transport tests.
- Parent: shared public contracts, WebMCP, guidance, integration/live checks.

## Rulings

- Existing feature branch is the authorized working branch; unrelated architecture notes are excluded.
- Event history is bounded operational history, not a promise of permanent audit retention or record replay.
- Unknown lifecycle causes remain unknown; do not infer deploy versus crash from generation alone.

## Status

Implementation in progress.
