# Integrating composable Bogs with the deployed cloud branch

## Inspected revisions

- Current cloud branch: `codex/personal-bog-cloud`, `d760757`.
- Incoming isolated branch: `codex/composable-hosted-bogs`, `7ed53ea`.
- Common ancestor: `40a4543`.
- Incoming work: 62 files, including shared definitions/runtime, resource APIs, additive rebuilds, search, and definition-aware backup/restore.
- Non-mutating merge preview: conflicts only in `cloud/src/lib.rs` and `cloud/src/supervisor.rs`. A clean textual merge elsewhere is not evidence of behavioral compatibility.
- The untracked architecture document in the current checkout is byte-identical to the incoming tracked document. Preserve the local file when preparing integration; do not discard unrelated files.

## Integration sequence

1. Create an isolated integration worktree based on the current cloud head. Merge the incoming commit there, preserving both histories and leaving both original worktrees intact.
2. Resolve the module conflict by retaining both domain routing and definitions. Retain both supervisor admission serialization and build capacity controls. Preserve canonical cloud.bog.new authorization, the legacy issuer compatibility path, and mcp.bog.new resource discovery.
3. Protect building/activating Bogs from pressure reclamation, matching the incoming idle-eviction protection. Coordinate candidate admission with worker admission: reclaim an eligible unrelated warm worker when safe, retain capacity for the source and candidate, and never bypass the physical capacity limit. Test a source that starts asleep as well as one already running.
4. Verify records-v1 compatibility and all existing authority boundaries: app credentials, delegated agents, workspaces, allowances, private handoff, request IDs, change cursors and observability. Expose new capabilities through the existing shared HTTP/MCP/WebMCP permission layer.
5. Test registry v4-to-v5 migration on a disposable copy. The incoming binary upgrades the registry even with composable creation disabled; flag-off is not old-binary rollback. Preserve an exact pre-upgrade recovery copy and rehearse recovery. After new writes, prefer a compatible forward fix rather than restoring an older copy and discarding writes. Retain the original-store artifacts already preserved by the feature branch.
6. Run the combined verification against the actual merged tree and its matching worker binary: existing cloud/MCP tests, Fold/search tests, definition/runtime tests, record persistence, builds, migration/restart/restore and browser-tool contracts. Add regressions specifically crossing build/eviction/admission boundaries, including cancellation, activation and restart. Check disabled-feature behavior and advertised operations.
7. Build the deployment artifact and measure model-backed runtime/build memory on the existing host budget before enabling hosted compositions. Do not infer capacity from the eight-worker count or local functional tests. Keep the feature disabled until this is demonstrated; no paid expansion.
8. Only after the combined gates pass, merge the verified integration branch into the cloud branch and conduct a disposable hosted application trial. Recheck auth/discovery, the original chat, and worker pressure behavior. Report implementation, merge and deployment separately.

## Finishing gate

An agent can use the canonical service to create a composed todo Bog, write/read/filter/rank its records, add and query search, then reopen it with unchanged records and permissions. Existing records-only apps continue working. Rebuilds and ordinary requests share the bounded host without evicting a build's source, interrupting active database operations or exceeding capacity.

## Status

The isolated integration branch preserves both source histories and includes regression fixes for build/restore admission, old-worker compatibility, and retained quotas. Combined verification passed 285 Rust tests and 17 browser-tool tests (one optional latency benchmark ignored). The isolated production-image capacity check also passed on nine Bogs and 900 records; actual hosted canary verification remains a separate activation gate. Final branch integration is tracked in the accompanying verification report. No production migration or feature activation is implied by the local merge.

## Coordination status

The feature agent's detailed handoff is received with the user's explicit authorization. Both agents agree on the isolated third-branch approach, build/capacity tests, registry recovery and measured model footprint. Original worktrees remain preserved. See the integration verification report for current implementation and release status.
