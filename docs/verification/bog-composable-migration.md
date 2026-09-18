# Migration and compatibility verification

## Scope and fixture

Added `cloud/tests/registry_v4_compatibility.rs`. The fixture executes the unchanged deployed migrations 001–004 directly; it does not create a v5 registry and lower its version. It includes a records-v1 Bog with stable ID/generation, account, workspace, membership, uncapped allowance, read-only app credential, delegated agent credential, Bog/workspace idempotency keys, and audit history. Credentials are synthetic. A real records-v1 store contains a persisted document.

The test copies the quiescent fixture before opening the merged service with composable creation disabled. It verifies migration to v5, exact equality of every pre-existing table, stable idempotency results, working app and delegated credentials, read-only and foreign-Bog rejection, disabled creation, unchanged stored records, and a separately copied recovery registry/store. It verifies the original recovery registry remains byte-identical and at v4, independently re-upgrades another copy, and checks foreign-key integrity. No running service, existing customer data, or credentials are used.

## Compatibility review

Compared the merged tree against d760757. Domain routing, native authentication, gateway authentication, and private app handoff implementation are unchanged. The shared operation dispatcher still authorizes target/scope before new resource operations; definition creation/rebuild additionally requires workspace management authority. MCP tool additions map through that same dispatcher. Existing request observation and change handling remain in the shared service; this read-only review is not a replacement for the parent's combined suite.

## Verification

- `cargo test -p bog-cloud --test registry_v4_compatibility --test auth --test native_auth --test gateway_integration`: **11 passed** (1 migration, 2 credential, 6 native authentication, 2 gateway isolation/CSRF).
- Initial socket-using tests were blocked by the sandbox; rerunning with approved local socket access passed.
- Initial isolated compile lacked cached ESE artifacts. Copied the existing root target's model and tokenizer into the worktree target cache; no network download or service changes.

## Recovery boundary

Feature-off startup still advances registry schema to v5. An old binary is not an in-place rollback. Keep a complete, quiescent pre-upgrade store copy and restore into a separate directory for recovery. Never lower `user_version`. Restoring old state after new writes would discard those writes; prefer a compatible forward fix.

## Preserved old executable rehearsal

`BOG_TEST_V4_SERVER=/Users/edouard/Developer/bog-kit/target/debug/bog-cloud-server cargo test -p bog-cloud --test registry_v4_compatibility` **passed**. The optional probe clears the entire inherited environment, supplies only a disposable copied root and synthetic owner credential, and does not enable public legacy operator access. The old executable accepts copied v4 registry state and reaches its missing-authentication guard (after registry open, before any listener/worker); it rejects the upgraded v5 registry with “registry version is newer than this server.” Both table snapshots and version markers remain unchanged by the probes. This verifies registry recovery compatibility, not a full old-worker restart/HTTP recovery.

Tested executable SHA-256: `cefa6691b1a68d68d892d61bb36676bdf45b6cca3e2f6cbf0fa500eab179207d`. Parent task identifies this preserved executable as the deployed d760757 code build; no embedded commit identifier was assumed. The process checks run locally against synthetic state only.
