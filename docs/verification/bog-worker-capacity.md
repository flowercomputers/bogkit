# Worker admission under capacity pressure

## Root cause

A resident worker held one of eight permits even when it had no active operation. A cold Bog tried to acquire a permit once and returned `capacity` immediately if none was free. Only the ten-minute idle sweep could reclaim a warm worker. This conflated cached workers with active work and made a sequence of requests to more than eight retained Bogs fail unnecessarily.

The live idle sweep was functioning: after the interval, metrics reported sleeping workers with `idle_timeout`, and the original chat read succeeded. There is no evidence of a leaked slot or a broken maintenance timer in this incident.

## Change

Under pressure, admission now gracefully closes the least recently used worker with no active lease and reserves the released slot. An exclusive, nonblocking per-Bog gate protects in-flight database operations. Concurrent admissions serialize reclamation; startup and physical-survivor limits remain in force. Ordinary ten-minute idle eviction remains unchanged.

A reclaimed Bog retains its data, ID, permissions and running intent. Subsequent access reopens it. Generation changes continue to trigger the documented change-cursor reset. Metrics/events record `worker_sleeping` with reason `capacity_pressure`.

Genuinely occupied slots (active operations, starts, or unresolved surviving processes) may still return a capacity error. The fix does not remove the host limit or promise unlimited concurrent workloads.

## Verification

The added regression test first failed on the old code with the original `capacity` error. The same test now verifies least-recently-used selection, persistence, generation change, retained intent and protection of an active lease. Additional concurrent admission coverage exercises six callers against two slots. Existing tests cover idle eviction, active leases, orphan ownership, manager recovery and retryable deletion.

## Deployed results

Deployed `81783e9` to the existing Fly machine on 2026-09-18 UTC. No host configuration, registry format, permissions or quota changes.

- Full cloud/MCP suites: **126 passed**, zero failures. Clippy passed with warnings denied.
- Live: two cycles over nine existing disposable Bogs, **18 successful reads**, without waiting for idle expiry. Record fingerprints matched across cycles.
- Pressure reclamation was visible in worker diagnostics. A read-only process inventory after the test counted exactly **eight** workers.
- Original chat `21214697-82be-44d6-9328-264cc344aedd` remained readable and its record fingerprint matched the pre-test read. Sanitized request ID: `579b12eb-8e6d-4903-97b7-3a5076cbde2b`.
- Disposable write/read/change-wait, isolation, metrics and MCP checks passed. Test record removed and credential `ccd7c6e6-8d93-48c3-85f7-6ea5a10e1eb8` revoked.
- Issuer/resource discovery and callback checks still passed on cloud.bog.new, mcp.bog.new and the legacy Fly hostname.

The original failure was reproducible locally before the patch and is now resolved both locally and on the live service. Fully busy capacity remains bounded and returns a retryable error rather than interrupting active database operations.
