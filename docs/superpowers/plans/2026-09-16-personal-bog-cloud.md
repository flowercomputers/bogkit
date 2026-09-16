# Personal Bog Cloud Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Execute inline unless the user separately chooses delegation.

**Goal:** Deliver a private service that creates persistent Fold databases over REST and exposes the same operations through MCP.

**Architecture:** A Rust gateway manages allowlisted worker processes, isolated storage directories, scoped credentials, and durable registry state on one host. Workers serve a versioned JSON-record template through Unix sockets. REST and MCP share authorized operations; Fold owns all application records and views.

**Tech Stack:** Existing Rust 2024 workspace, Fold/Fjall, bog-serve/Axum/Tokio, Serde/Schemars, SQLite registry, official Rust MCP SDK, Python standard-library acceptance scripts, Linux deployment and macOS development.

**Spec:** `docs/superpowers/specs/2026-09-16-personal-bog-cloud-design.md`

## Global Constraints

- Preserve upstream behavior by default; new durability and lifecycle options are opt-in for existing apps.
- One persistent host; no horizontal replication or cross-Bog transactions in v1.
- REST and MCP invoke the same authorization and operation layer.
- Workers listen on Unix sockets owned by the service account; no public worker ports.
- Runtime user data, tokens, registry files, and backups never enter Git.
- Never log authorization headers, raw tokens, or document bodies by default.
- Notifications request a refresh; they are not a durable change log.
- Important-data readiness requires a successful independent restore test.

All remaining requirements and exact limits in the spec apply to every phase. No implementation or deployment is performed by writing these documents.

---

## Execution order

| Phase | Deliverable | Exit gate | Execution plan |
|---|---|---|---|
| 0 | Stored JSON and a dependable worker contract | Nested JSON round-trip, atomic views, durable acknowledgement, shutdown/reopen | [Storage and worker](2026-09-16-bog-cloud-storage.md) |
| 1 | Private REST provisioning | Create two Bogs, scoped CRUD/batches, manager restart, real second-machine access | [Management and REST](2026-09-16-bog-cloud-service.md) |
| 2 | Agent access | Initialize/discover/call through MCP and one actual client, with scope enforcement | [MCP and operations](2026-09-16-bog-cloud-mcp-operations.md) |
| 3 | Personal-use readiness | Independent backup restore, interrupted-operation recovery, bounds, deployed acceptance | [MCP and operations](2026-09-16-bog-cloud-mcp-operations.md) |
| 4 | Broader utility | Search/summary template validated against an actual use case | New bounded plan after phase 3 evidence |

Phase 0 blocks phase 1. Phase 1's local acceptance blocks remote deployment. Phase 2 reuses phase 1's service methods. Phase 3 is required before claiming readiness for important data. Do not make phase 1 wait for a dashboard or public client onboarding.

Phase 4 candidates, in order: searchable notes template; selected configurable Fold summaries; conditional record updates/revisions; durable event log if offline replay is needed; dashboard; custom application builds. Each candidate requires its own testable design and scope. These are roadmap options, not hidden work inside the first release.

## Branch and change discipline

Created `codex/personal-bog-cloud` from upstream Sam commit `5a0fb83de9a9633a74f4e6c559ef52a2fd432206`. Keep Sam's branch untouched. Do not merge unrelated playground or benchmark branches.

For each implementation task: inspect current state, implement its meaningful failing test, verify the failure is the expected missing behavior, implement, run focused tests, review the diff, then checkpoint only the task's files. Commits must not include user data or unrelated concurrent edits. Public push/PR/deployment status must be reported separately from local completion.

## Decisions made for planning

1. Trusted versioned templates precede custom-code hosting.
2. JSON records plus count/list views are the first template; search follows.
3. SQLite stores management metadata; Fold stores application records and views.
4. One worker per Bog; no automatic scale-to-zero initially.
5. Private bearer-authenticated REST/MCP first; public OAuth onboarding is separate.
6. Backups initially pause one instance briefly, enabling a simple verifiable consistent copy.
7. No UI is needed to pass the first acceptance test.

## Decisions requiring execution-time evidence

- Available persistent host, deployment directory, hostname, and second test machine. Investigate existing choices read-only; a blocked remote environment must not be reported as remote success.
- Compatible released MCP SDK and registry/crypto dependencies. Verify primary documentation and pin resolved versions before using APIs.
- Actual checkpoint latency, worker memory overhead, and storage consumption. Record measurements from representative data; do not convert defaults into capacity claims.

## Completion ledger

- [x] Upstream branch refreshed and base commit recorded.
- [x] Dedicated working branch created.
- [x] Design, subsystem plans, limits, and acceptance gates written.
- [ ] Phase 0: JSON/worker proof.
- [ ] Phase 1: REST management proof on local host.
- [ ] Phase 1: REST proof from a remote machine.
- [ ] Phase 2: MCP proof through a real client.
- [ ] Phase 3: restore and recovery proof.
- [ ] Phase 3: bounded deployment and operational handoff.

## Final acceptance transcript

Save redacted commands/results with build commit, template version, dates, host identities, and test record IDs in `docs/verification/bog-cloud-acceptance.md`. Never include tokens. Run this sequence with a fresh prefix and delete only that run's disposable data through explicit operator cleanup:

1. Authenticate from machine B to machine A; reject a missing credential.
2. Create Bog A with an idempotency key; retry and recover the same ID.
3. Create Bog B; issue independent read/write credentials.
4. Put a nested JSON document in A, read it, replace it, list it, and check total.
5. Apply a valid batch; reject an invalid batch without any partial mutation.
6. Delete a record and verify docs and total agree.
7. Confirm A credentials cannot read, write, or subscribe to B.
8. Restart a worker and the manager; preserve IDs, records, scopes, and views.
9. Connect a real MCP client; repeat creation and record operations through tools.
10. Revoke a token and reject the next request/tool call, including existing sessions.
11. Back up A; restore as C; compare records and views; verify new credentials.
12. Run bounds and shutdown checks; confirm no unmanaged workers or public worker ports remain.
