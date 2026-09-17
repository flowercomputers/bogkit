# Bog Cloud MCP journey — 17 September 2026

## Result

Three releases implemented on the existing Fly host. Actual Codex and Claude clients authenticated through GitHub, created disposable Bogs, built working local chats, installed scoped credentials privately, and passed live reading, writing, change waiting, application restart, and revocation checks. Existing chat records were unchanged.

This is not an unconditional acceptance claim: a second real GitHub account is pending at the user's request, and signed-in WebMCP awaits GitHub sign-in in the supported in-app browser. Local permission/isolation tests and real public WebMCP calls passed.

## Releases and verification

| Release | Commit | Verified behavior |
| --- | --- | --- |
| Connection | `7e7cb78`, `8a832be`, `165017c` | Path-specific protected-resource metadata, client metadata documents, native loopback callbacks, scope challenges, actual client OAuth, protocol negotiation through 2025-11-25 |
| Private app access | `58900a9` | 16 tools, four read-only resources, result schemas, ten-minute account-bound handoffs, private helper and explicit browser download |
| Browser tools | `232e768` | Schema inspection, bounded previews, caller-specific allowances, private preparation, current-session checks and console feedback |
| Trial-driven guidance | `2b4cb10` | Same-Bog capacity retries, exact private configuration fields, authorized helper execution and owner revocation instructions |

Local verification: **116 Rust tests, 14 JavaScript tests, 8 Python helper tests** passed. Strict Clippy passed for cloud/MCP packages and their targets (`--no-deps`); no claim is made for unrelated dependency lint. Documentation follow-up contract and idempotent-retry tests passed. Independent reviews found and resolved allowance selection when another person's personal workspace is shared, member credential rollback, and recovery wording.

The official MCP Inspector **2.7.0** discovered all 16 tools. This was diagnostic evidence, separate from actual client trials.

## Actual clients

| Client | Version | Outcome |
| --- | --- | --- |
| Codex CLI | 0.154.0 | Actual GitHub-backed OAuth via official client metadata; workspace discovery, creation/retry, direct MCP write/read/wait; independently generated chat passed hosted HTTP read/write/wait, restart and actual revocation |
| Claude Code | 2.1.274 | Actual GitHub-backed OAuth via official client metadata; workspace/allowance discovery, creation/retry, MCP write/read/wait; independently generated chat passed 18 app unit checks and 20 live smoke checks, followed by actual revocation |

Fresh application-building contexts began with the homepage URL and reused the connections approved in phase 1. They were not second independent GitHub identities. The private helper received its separately approved connection once; subsequent installations reused only its own authorization cache. Neither client credential store was extracted.

Codex chat: `4b253f39-25f3-4f16-929d-51cb8c4990b9`. Live app write woke a pending HTTP and MCP wait; the same message survived process restart. Browser submission passed. Revocation returned 204; reads and waits returned 401, and the browser cleared messages and disabled sending.

Claude chat: `f4845ece-b33a-4b6b-8b3b-ea2687313d6e`. Live pending wait woke in approximately 1.04 seconds; messages survived process restart. Browser submission passed. Actual revocation returned 204; reads and waits returned 401, and the application displayed access denial. Its earlier simulated invalid-token test is not counted as revocation proof.

A noninteractive Codex run initially required explicit permission for its write tool. The authorized tool setting was then applied and direct MCP upsert/read passed. An initial local listening restriction was likewise resolved before runtime results were counted.

## Private delivery and browser checks

Both helper installation and the explicit signed-in console download produced working single-Bog credentials. Write and read-only permissions were exercised. Provisioning, token issuance and access to another Bog were denied. All four test app credentials were revoked and subsequent access failed.

The helper creates mode-0600 files. Browser download permissions depend on the browser/OS (0644 observed); documentation instructs users to restrict them, and the test file was restricted afterward. Private values were not printed or returned as tool results. A targeted artifact scan found no test credential values in app sources, reports or client transcripts.

Actual in-app-browser public WebMCP calls passed for service discovery and templates. Signed-in WebMCP tests remain pending login there; JavaScript integration tests are not presented as a substitute. Regular signed-in console rendering and mobile navigation passed at 390px with no horizontal overflow. Temporary viewport overrides were cleared.

## Friction discovered and limits

- Eight resident workers is a real shared-host ceiling, independent of uncapped workspace quotas. A new test Bog initially failed with capacity, then reopened after idle eviction. No infrastructure was expanded.
- Claude created four extra failed test Bogs while misunderstanding capacity recovery. Guidance now explicitly says to retain the returned Bog ID and retry the same creation key/body; polling descriptions alone does not restart a worker. These fixtures remain retained, with no app credentials, for owner cleanup.
- Both agents needed clearer private-file instructions; exact configuration keys and agent-run helper behavior are now documented. Claude also corrected its app's confusion between flat HTTP responses and MCP's result envelope.
- Access expires after the existing 30-day connection lifetime; refresh tokens remain deferred. Pending handoffs expire on restart, which was observed during deployment and recovered by preparing a new handoff.
- Second real-account sharing/removal and authenticated browser WebMCP remain pending. Local two-account membership/isolation, revocation, handoff replay/concurrency and restart tests passed.

Hosting remains one existing Fly machine in iad, one shared CPU, 1 GiB RAM and the existing 3 GiB volume. No paid expansion, registry migration, domain move, or backup infrastructure was introduced.

## Sanitized request evidence

| Operation | Request ID |
| --- | --- |
| Codex initial OAuth trial create | `2852a2c9-ecd8-45fc-96c5-a2330a0dc4ec` |
| Codex identical retry | `fb5756cf-32b6-4bf4-a0df-6d997251cd61` |
| Claude initial OAuth trial create | `2aee9cc9-9689-4422-a983-ebfc0d6d3899` |
| Claude identical retry | `f591d40a-5e90-4852-b26d-c007c70de406` |
| Codex fresh create / retry | `f4b7c733-81ec-4fe9-afec-c27bea365c3b` / `edf50bd7-dc02-449c-a268-42f9ace0819a` |
| Codex direct MCP upsert / read | `ee246d29-2a00-4040-9746-37ccb10f5072` / `78e26535-5ac4-4470-8235-7f915f4aba9f` |
| Codex pending MCP wait woke | `d69c2c73-4a74-4872-88b0-1cf3f9566594` |
| Codex app revocation | `361d68c4-be74-45bf-ad3c-4643763abe90` |
| Claude private preparation | `6f7b2973-0ae6-413b-8cb9-48ae3d3d8af5` |
| Claude app revocation | `e15d5704-4002-4630-8617-736b4b937fa6` |
| Claude app denied read after revocation | `2c309356-6d03-4b0d-9a3a-b22bf081a21d` |
| Helper / download test revocations | `3c165537-124e-484b-bb10-39622e5c09df` / `8591ac2d-0365-480b-8b66-f85d07bbc062` |

## Readiness scanners

Refreshes during the deployment window failed: Cloudflare reported network aborts (6/100), and Is Agentic retained its older report with an explicit refresh-failed notice. Direct homepage/robots checks returned HTTP 200 in under 0.1 seconds. A post-deployment retry is recorded below; old scores are not claimed as new results.


Cloudflare retry completed at **20:20:52 UTC: 87/100, Level 5**, unchanged check selection. Remaining flags: DNS-AID on the Fly hostname, and the scanner comparing the homepage against the canonical protected `/mcp` resource. Actual Codex/Claude authorization verifies that protected endpoint.

Is Agentic's second rescan displayed **100/100**, but its persisted snapshot remained **18:17:36 UTC**, older than this trial. A fresh persisted score is therefore **unverified**, not counted as a new 100/100 result.

Final `2b4cb10` deployment succeeded on the same machine; live health/discovery checks passed and the original chat record fingerprint remained unchanged. Private credential artifact scan: 34 designated app/report/transcript files, zero credential-value matches. Test app and local probe processes were stopped.

## Retained acceptance fixtures

These are disposable test Bogs, not the existing chat. All issued test app credentials are revoked. The four capacity-failed Claude fixtures contain no trial records; they remain for owner cleanup rather than weakening the delegated-agent deletion restriction.

- `290fe549-a7b7-4849-98dd-0a570054504d` — phase 1 Codex
- `71b09918-c906-43f3-80e3-0d70efd70aa4` — phase 1 Claude
- `4b253f39-25f3-4f16-929d-51cb8c4990b9` — fresh Codex chat
- `f4845ece-b33a-4b6b-8b3b-ea2687313d6e` — fresh Claude chat
- `b8693596-9851-470e-b288-462ca093bf88` — failed capacity attempt
- `44d89cd3-becf-42cb-9ddd-04126e3897db` — failed capacity attempt
- `d8d93756-7680-4e68-9b09-e41353b63bae` — failed capacity attempt
- `11c58c23-425b-4bd8-b754-87ec2296a33c` — failed capacity attempt
