# Task 3: retained resources and resident worker lifecycle

Implemented in supervisor/config plus focused lifecycle tests and narrowly updated existing recovery expectations (approved by root).

- Default idle timeout is 600 seconds; resident/start limits remain eight/two.
- Lease completion updates a per-worker idle clock before releasing its read gate. Idle maintenance only acquires gates without active leases and preserves desired-running intent, IDs, data, and credentials.
- Wake on demand uses existing generation/identity, inherited lifetime-lock, and Fjall safety. New processes retain authoritative registry generation changes.
- Reconciliation adopts verified surviving workers, recovers interrupted maintenance/restores, retries tombstone cleanup, and does not eagerly start closed retained resources.
- Physical capacity checks account for untracked workers holding ownership locks, including pre-socket survivors. Capacity errors tell callers to retry or stop an unused resource.
- `spawn_maintenance()` holds only a Weak supervisor reference. The manager must start it and abort its handle before graceful shutdown; gateway integration agent notified.
- `cleanup_deleted(id)` requires an existing authorization tombstone, closes tracked or verified surviving workers, holds both lifetime/store locks through filesystem removal, and records cleanup completion. Both startup reconciliation and periodic maintenance retry outstanding deletions. It never creates the tombstone or changes authorization.

## Verification

All local process tests required escalation because the managed sandbox denies Unix socket binding; no production service or secret was accessed.

- `cargo test -p bog-cloud --test resident_lifecycle --test manager_restart --test end_to_end`: seven tests passed. Covers durable idle wake, lease completion grace, no active eviction, 32 retained/eight resident, tombstone rejection and retry cleanup, physical pre-socket orphan capacity, real manager-crash paused/pre-socket worker identity preservation, and independent worker restart.
- `cargo test -p bog-cloud --test backup_restore`: three tests passed, including inherited-worker backup adoption and interrupted maintenance/restore recovery.
- Existing tests that depended on eager startup now explicitly trigger demand. Interrupted maintenance first becomes stopped with running intent, then wakes ready.
- Scoped clippy found two local style issues, both fixed; remaining findings on the final repeat were concurrent HTTP and changes-module collapsible-if lints (no lifecycle warnings); root notified. Unscoped clippy also reports pre-existing anny range-loop lints.

No deployment performed. Full gateway/authorization integration and Linux verification remain with root.
