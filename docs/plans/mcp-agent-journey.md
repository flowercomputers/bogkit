# MCP agent journey implementation ledger

Approved plan: connect, understand, and build through MCP (2026-09-17).

## Gates
1. Connection: CIMD, scope challenge, truthful connection guide, Inspector and actual Codex/Claude OAuth. Deploy independently after local/live verification.
2. Understanding: instructions/resources/typed results and private app-access handoff, local helper/download. Only begin after phase 1 gate.
3. Browser affordances and fresh-agent application trials; independent accounts and scanner report.

## Constraints and rulings
- Existing clean feature branch codex/personal-bog-cloud is the authorized working branch; preserve other work, existing data, credentials, hosting and Flower design.
- No WorkOS, new database features, paid expansion, domain migration, or registry-format changes.
- Phase 1 auth worker owns native OAuth + MCP scope middleware/tests. Parent owns connection page, client trials and integration.
- Connection instructions must distinguish proven client flows from pending ones. Human approval and actual-client gates cannot be replaced by test fixtures.
- Handoff interface produced in phase 2 will be consumed by helper and phase 3 WebMCP. No phase 3 implementation before phase 2 gate.
- Local helper may need its own device approval; it must not extract another client's tokens.

## Status
- Phase 1: in progress; baseline clean ceed480; installed Codex 0.145.0 and Claude Code 2.1.139.
- Phase 2: pending.
- Phase 3: pending.

### Phase 1 evidence
- Real Codex 0.145.0 registration failed before consent: requested authorization_code + refresh_token. Fix selects supported authorization_code without promising refresh.
- Public discovery tests pass in native and legacy modes, including /connect and unchanged Flower shell.
- Claude Code 2.1.139 CLI not authorized; browser login currently shows subscription requirement. User asked to switch subscribed account or run critique agent. No paid upgrade.
- Ruling: live actual-client verification necessarily follows a candidate deployment; do not declare release accepted or start dependent phase until gate passes.
