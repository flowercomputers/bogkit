# Team preview verification

The token-access preview is now deployed at the user’s explicit request. GitHub signup, device authorization and the authenticated team console remain unavailable until WorkOS is configured. The earlier verification notes below describe the pre-deployment checks; full signup activation still follows `docs/bog-cloud-production-gates.md`.

## Verified locally

- Complete Linux Docker verification at source snapshot `5a2eb08`: **151 tests passed, one existing ignored test**. Includes registry migration, accounts/workspaces, quotas, REST/MCP, OAuth fixtures, browser session/CSRF/logout, crash/restart, and backup restoration.
- Concurrent version-1 migration repeated with 20 rounds of eight connections and 20 rounds of four separate processes. Every successful opener observed version 2 and clean foreign keys; unknown future versions are rejected.
- Storage tests cover quota boundaries, replacements, repeated keys within batches, concurrent writes, oversized store shrinkage, custom transactions, and durable restart.
- Lifecycle tests cover 32 retained resources / eight resident workers, idle eviction, delayed surviving workers, interrupted stops, and an old response held across a concurrent restart. This is functional admission testing, not a measured supported-user count.
- HTTP and MCP share workspace selection, credential restrictions and change cursors. Mock provider tests verify independent accounts and explicit shared-workspace requests. Public global-operator authentication is disabled when WorkOS is configured; production binaries require explicit opt-in for legacy startup without WorkOS.
- Eight backup-tool tests passed. A real age 1.3.2 encryption/decryption and isolated restore drill preserved Bog identity, records and owner permissions. The object store was mocked locally; off-host recovery is not established by this test.
- Console inspected in Chrome using local API fixtures: personal workspace, storage usage, creation feedback, one-time credential display/hide, and invitation link display. REST browser flow separately exercised against a signed mock OAuth provider. This is not evidence of real GitHub login.

The macOS full regression run hit an existing Unix-socket readiness race in `serve/tests/daemon.rs`; the focused rerun passed all five tests. The full Linux suite passed. Whole-workspace formatting reports pre-existing differences in CLI/search example files; these unrelated files were left unchanged.

## Not yet verified or activated

Real WorkOS production GitHub/Connect/device flows and provider revocation; fresh external MCP and HTTP agents; the second real account invitation flow; exact operator legacy identity claim; remote chat credential replacement and owner-secret rotation; organization-owned S3/R2 destination and daily schedule; off-host restore rehearsal; operational alerts; exact deployed-host workload capacity. The user confirmed the chat runs locally on another machine, so its continuity cannot be proven from this checkout.

Live host read-only check: Fly app `flower-bog-cloud`, machine `4d895395c393e8`, iad, one shared CPU and 1,024 MiB RAM; health check passing on the unchanged `deployment-01M2NTDYBCJ8PKZR46X95J09AD` image. No hosting expansion was performed.

Final permission correction `67a6391` reserves invitation creation/revocation, member removal and Bog deletion for human browser sessions. Agents retain app-credential management. Seventeen focused unit/auth tests and two gateway tests passed after the correction; independent final review repeated the permission regression and both gateway tests successfully. No remaining review finding from the final scoped pass.

The already-built Linux lifecycle executable also passed all six tests in a disposable container limited to one CPU and 1 GiB RAM (2.90 seconds). This covers lifecycle and admission behavior with small fixtures, not full-store performance or actual Fly host capacity. The initial attempt to run Cargo under those limits was stopped because it rebuilt dependencies; compilation resource samples were not treated as service measurements.

Final strict Clippy checks passed for cloud, MCP, records and serve across all targets with dependency linting excluded. The last correction was a semantics-preserving conditional simplification in MCP challenge handling.

## Live token-access preview deployment

Deployed runtime commit `7a0ef09` to `https://flower-bog-cloud.fly.dev/`, image `registry.fly.io/flower-bog-cloud:deployment-01M2PQNE2NWPPA58XHGBGG4K20`, digest `sha256:4c736fad869a2b81b86521768ebcb0f8d0c48f737ba757dbd1b5c902c04fc1b0`. Branch pushed to origin `codex/personal-bog-cloud` (repository redirects to flowercomputers/bogkit).

Cordon + graceful SIGTERM stopped the old service before cold volume snapshot `vs_kzyazlD2DPRS70xOzoGb` completed at 2026-09-17T03:50:37Z. Snapshot retention is five days. This is a recovery snapshot, not the planned daily encrypted off-host backup service. Registry migrated to the new format; never roll back the old binary against this migrated registry. A rollback requires restoring the matching pre-migration snapshot.

Temporary `BOG_ALLOW_LEGACY_PUBLIC_OPERATOR=true` preserves the existing access model while WorkOS is absent. The homepage and console explicitly show GitHub sign-in pending. Existing owner and scoped credentials were preserved; no remote-chat configuration or owner-secret rotation was performed.

Live checks passed: all six existing resource IDs and schemas; a scoped credential issued before migration; page limits and response cursor; change wait; denial of credential minting by an app credential; real write/read/delete in the existing disposable test Bog; MCP initialization and discovery of 13 tools; health and public discovery/templates/OpenAPI/llms endpoints. Temporary probe credentials were revoked and the probe record removed. Chrome verified the live homepage and sign-in status page.
