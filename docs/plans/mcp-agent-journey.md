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
- Phase 1: complete on165017c; actual Codex0.154.0 and Claude2.1.274 liveOAuth+operational trials passed.
- Phase 2: complete on58900a9; both private delivery methods passed live and test credentials revoked.
- Phase 3: implemented and deployed; both actual-client app trials passed. External second-account and signed-in WebMCP acceptance remain pending.

### Phase 1 evidence
- Real Codex 0.145.0 registration failed before consent: requested authorization_code + refresh_token. Fix selects supported authorization_code without promising refresh.
- Public discovery tests pass in native and legacy modes, including /connect and unchanged Flower shell.
- Claude Code 2.1.139 CLI not authorized; browser login currently shows subscription requirement. User asked to switch subscribed account or run critique agent. No paid upgrade.
- Ruling: live actual-client verification necessarily follows a candidate deployment; do not declare release accepted or start dependent phase until gate passes.

- Phase 1 candidate committed 7e7cb78; 107 full-suite tests passed before final strengthened cases; worker additionally passed36 cloud unit +12 MCP tests. Strict Clippy passed after three targeted fixes.
- Independent review found no blocking defects; successful external HTTPS CIMD login remains an evidence limit.
- Official Inspector tools/list passed (13 tools) with an existing private test credential; not counted as actual-client OAuth.
- Both CLI accounts now authenticated; Claude auto-updated to2.1.274.
- Local /connect visual check passed in existing Flower shell; candidate Fly build in progress.

- Candidate7e7cb78 deployed successfully; both actual clients now select CIMD. Both official documents use loopback callbacks without fixed ports. Ruling: apply OAuth native loopback port exception only to native public metadata, or omitted application_type with exclusively loopback redirects; preserve literal host/path/query and exact DCR callbacks. This is needed for real native-client interoperability, not an arbitrary redirect wildcard.

- Native callback fix committed8a832be;5 metadata tests, OAuth transport regression and strictClippy pass; independent scoped review approved.
- Current installed clients after userlogin/update: Codex0.154.0 and ClaudeCode2.1.274. Earlier0.145.0 Codex exercised DCR; current clients exercise CIMD.
- Mobile check via Chrome device emulation390x844: page scrollWidth==390, navigation opened withConnect/Docs/Console links. Emulation cleared afterward.

-8a832be live. Both realclients reached correct consent through official HTTPS CIMD: Codex atchatgpt.com/oauth/codex/client.json, Claude atclaude.ai/oauth/claude-code-client-metadata. Both request bog:write, correctloopbackcallback. Awaiting explicit userconfirmation atbrowser permission step; do not approve before response.

- User approved both 30-day bog:write test connections. Both real CLIs confirmed successful OAuth login. Codex0.154.0 passed workspace discovery, one disposable Bog creation/retry, write/read, and timeout0 change wait. Fixture290fe549-a7b7-4849-98dd-0a570054504d; creation request2852a2c9-ecd8-45fc-96c5-a2330a0dc4ec; retryfb5756cf-32b6-4bf4-a0df-6d997251cd61.
- Claude2.1.274 authenticated but tools discovery rejected missing ttlMs/cacheScope; actual client trial did not create any Bog. Investigating SDK protocol negotiation before phase2. Existing chat fingerprint unchanged on8a832be.

- Protocol fix: SDK supportedVersions included2026 even though initialize selected2025. Bound advertisement/negotiation to known revisions through2025-11-25, preserving existing older-client support. Actual Claude local discovery/list passed. Eight protocol tests and expanded OAuth denial/exchange-binding tests passed; strictClippy passed. Independent review pending.

-165017c deployed; Claude live trial PASS with disposable71b09918-c906-43f3-80e3-0d70efd70aa4. Creation2aee9cc9-9689-4422-a983-ebfc0d6d3899, retryf591d40a-5e90-4852-b26d-c007c70de406, read2dcc564c-0f55-4b54-b61d-bad6d1701052. Existingchat unchanged. Phase1 accepted.

### Phase 2 local evidence
- Account-bound ten-minute in-memory private handoffs, one redemption, rechecked permission, additive native SQLite labels, no registry migration. Backend review found member rollback failure; fixed by narrowly matched internal revocation with audit, tested with forced label-storage failure.
- MCP now has16tools,4read-only resources, instructions, operation-specific success schemas. All14transporttests passed with actual response JSONSchema checks. Pythonhelper8tests and7console/WebMCP JS tests pass; realhelper-to-localgatewayintegration produced600 file and verifiedscopedtoken.
- Console signed-out handoff loss fixed with validatedUUID-onlysessionStorage; independentreviewapprovedfix. No credentials stored there. Backend andclientreviews no remainingblockers. Fullfinalsuite/lints pendingcandidate.

- Phase2 candidate finalchecks:116Rusttests+7JStests+8Pythonhelpertests passed; strictcloud/MCPalltargets--no-depsClippypassed. Candidate ready for deployment and liveprivate-deliverychecks.

- User explicitly chose to leave real second-GitHub-account sharing acceptance pending. Local two-account isolation remains tested; do not claim independent real-account verification. Continue remainingclient/browser/appchecks.

### Phase 2 live gate
- User approved private installer and console test download. Both redeemed same-account handoffs on58900a9. Helper saved mode600; browser default644, guidance already tells users to restrict downloaded-file permissions; test download restricted600 afterward. Read/write/wait and read-only enforcement passed; provisioning/mint/other-Bog access denied. Both credential revocations returned204 and subsequent reads401.
- Revocation request IDs3c165537-124e-484b-bb10-39622e5c09df and8591ac2d-0365-480b-8b66-f85d07bbc062. Test harness filename variable fixed; no service defect. Phase2 accepted.

### Phase 3 candidate
- Added schema, bounded previews (default5/max20), allowance and private-preparation WebMCP tools; current-session checks, cancellation/status and console refresh.14JS+8helper+116Rusttests passed; strict packageClippy passed.
- Review fixed default allowance selection through caller /v1/me.workspace_id, including another personal workspace first and uppercase explicit UUIDs. Scoped re-review approved. Final phase1/2 integration review found only owner-revocation wording, corrected in helper/console/Connect. Exact private configuration keys now documented after fresh-app observation.
- Real public WebMCP discovery/templates passed in in-app browser. Signed-in WebMCP awaits GitHub login there; regular Chrome console and both MCPclients already authenticated.
- FreshCodex created one Bog, idempotency passed, initially hit residentcapacity; on-demand read recovered afteridleeviction without expansion. Continuing liveapptrial; Claudetrial running.

### Final outcome
- 232e768 and guidance follow-up2b4cb10 deployed successfully. Existing chat fingerprint unchanged; health/discovery passed. No infrastructure expansion or registry change.
- Actual Codex and Claude fresh app trials passed live read/write/wait/restart; parent browser messages and actual credential revocation passed. All four test app credentials revoked. Private artifact scan34files zero credential matches. Owned test app/probe processes stopped.
- GitHub sign-in remains pending in the in-app browser for authenticated WebMCP. Actual public WebMCP calls passed; regular signed-in console390px navigation passed. Real secondaccount pending per user direction.
- Cloudflare fresh retry87/100 at20:20:52UTC; IsAgentic displayed100 but persisted18:17:36UTC older snapshot, freshness not claimed.
- Retained eight disposable fixtures listed in verification report; no deletion permissions broadened. Unrelated untracked docs/bog-language-cloud-architecture.md belongs to other work and is excluded.
