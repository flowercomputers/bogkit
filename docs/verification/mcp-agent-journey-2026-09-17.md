# MCP agent journey verification — 2026-09-17

## Phase 1 candidate

Live host: https://flower-bog-cloud.fly.dev. Existing Fly host configuration and registry format unchanged. Commits 7e7cb78 and 8a832be deployed.

| Check | Evidence | Outcome |
| --- | --- | --- |
| Local service suites | 107 tests, plus strengthened metadata/OAuth tests; strict Clippy | Passed before live trials |
| Official MCP Inspector 2.7.0 | tools/list returned 13 tools | Passed; not a substitute for client OAuth |
| Codex CLI 0.154.0 OAuth | Official HTTPS client metadata, GitHub-backed browser approval, CLI success | Passed |
| Codex actual MCP operations | Personal workspace, create/retry same ID, upsert/read, timeout-zero wait | Passed |
| Claude Code 2.1.274 OAuth | Official HTTPS client metadata, GitHub-backed browser approval, CLI success | Passed |
| Claude actual MCP operations | Protocol advertisement capped through 2025-11-25; workspace discovery, create/retry, write/read/wait | Passed live on 165017c |
| Existing chat | Record fingerprint unchanged after both deployments | Passed |
| Connect page | Desktop and 390px mobile; no horizontal overflow, mobile menu works | Passed |

Codex disposable Bog: `290fe549-a7b7-4849-98dd-0a570054504d`. Sanitized request IDs: creation `2852a2c9-ecd8-45fc-96c5-a2330a0dc4ec`, retry `fb5756cf-32b6-4bf4-a0df-6d997251cd61`, read `c64ce1ef-0e54-4a6c-b2bc-3f2f266fc5cd`, wait `c5ebc723-be98-430a-80f5-814f01a64205`. No existing Bog data was read by the client trial; no app credentials were issued.

## Remaining gates

Phase 1 passed. Claude disposable Bog: `71b09918-c906-43f3-80e3-0d70efd70aa4`; creation request `2aee9cc9-9689-4422-a983-ebfc0d6d3899`, retry `f591d40a-5e90-4852-b26d-c007c70de406`. Phase 2 private credential handoff passed local and live verification (below); Phase 3 fresh-agent application trials are in progress. No claim is made here for two independent real accounts, browser WebMCP acceptance, or fresh scanner scores.

## Phase 2 accepted

Commit58900a9 deployed.116Rusttests,7JStests,8Pythonhelpertests passed; strict package Clippy passed. Actual Codex discovered context/templates and prepared two private handoffs. Helper installation saved mode600; browser download worked (browser-default permissions subsequently restricted). Neither credential appeared in tool results. Both passed bounded reads/change waiting, write versus read-only scope, and denied provisioning, minting and cross-Bog access. Both revoked: requests `3c165537-124e-484b-bb10-39622e5c09df`, `8591ac2d-0365-480b-8b66-f85d07bbc062`; both subsequently returned401. Existing chat fingerprint unchanged after deployment.

Real second-account acceptance remains pending at the user's request; local isolation tests pass. Phase3 remains in progress.

Official MCP Inspector2.7.0 repeated after Phase2:16tools discovered successfully. The browser download has ordinary browser/OS file permissions; documentation explicitly directs users to restrict them. The helper enforces0600 itself.
