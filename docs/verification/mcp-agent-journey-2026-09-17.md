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

Phase 1 passed. Claude disposable Bog: `71b09918-c906-43f3-80e3-0d70efd70aa4`; creation request `2aee9cc9-9689-4422-a983-ebfc0d6d3899`, retry `f591d40a-5e90-4852-b26d-c007c70de406`. Phase 2 private credential handoff is under local verification; Phase 3 fresh-agent application trials remain pending. No claim is made here for two independent real accounts, browser WebMCP acceptance, or fresh scanner scores.
