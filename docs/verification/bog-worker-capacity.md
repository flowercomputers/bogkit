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

Deployment and final live results are recorded after verification below.
