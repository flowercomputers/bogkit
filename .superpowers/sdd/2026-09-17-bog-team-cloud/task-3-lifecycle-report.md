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

## Round 1 recovery correction

Addressed the review finding about untracked survivors becoming healthy after initial reconciliation.

- Periodic maintenance now repeats a survivor-only reconciliation. Identity/schema probes are bounded to 500 ms, busy instance gates are skipped, and closed stores are never spawned by this pass.
- Verified running-intent survivors enter resident accounting with a fresh idle timestamp. Existing tracked workers are skipped so recurring scans cannot reset their idle deadline.
- Stopped-intent survivors receive only identity-verified shutdown requests. Both periodic recovery and explicit stop retries wait for lifetime/store ownership to be released; unavailable pre-socket workers remain retryable.
- Tombstone cleanup reuses the same stopped-survivor path rather than duplicating shutdown probes.
- Added real child-process tests with inherited lifetime locks: a paused pre-socket worker resumes after manager replacement, receives no Bog request, is adopted and idle-evicted, and releases capacity for an unrelated Bog. Separate stopped-intent scenarios prove both automatic maintenance and explicit stop retry.

Verification: all five resident lifecycle tests and both real manager-crash tests passed together. Scoped `cargo clippy -p bog-cloud --lib --no-deps -- -D warnings` passed. Backup recovery tests passed 2/3; the remaining existing comparison fails because concurrent task-6 integration correctly adds different cursors to original/restored Bog responses with identical data/seq. Root was notified to update that separate cursor assertion. Unix-socket tests ran with sandbox escalation. Existing manager test operator opt-in changes were preserved and not included in this correction.

## Generation snapshot correction (Task 4/6 review finding 2)

`Running` now retains its authoritative generation. Startup/adoption returns a private immutable snapshot of client, generation, and last-use clock while holding startup serialization; `WorkerLease.generation` comes from that snapshot, without a later registry lookup. New workers use the generation returned by `start_generation`; survivor reconciliation captures the verified worker generation under the exclusive instance gate. Existing backup `start_locked` callers retain their client-only API.

Gateway and change-wait owners were coordinated to consume lease generation. A new regression keeps an old worker response and lease alive, kills that worker, obtains a replacement through another simultaneous read lease, then tags the earlier response. Even with identical sequence values, the old and new cursors differ, and waiting with the old cursor reports a reset.

Verification: all six resident lifecycle tests passed, including the new crash/restart generation regression. `cargo check -p bog-cloud` and scoped Clippy with warnings denied both passed. Gateway/change-wait integration changes are owned and committed separately.
