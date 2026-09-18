# Canonical Bog authorization — 2026-09-18 UTC

## Confirmed fault

Before this change, `https://cloud.bog.new/.well-known/oauth-authorization-server` returned 200 but declared `https://flower-bog-cloud.fly.dev` as its issuer. The same copied document appeared on mcp.bog.new. A direct issuer-discovery client must reject that mismatch under [RFC 8414 section 3.3](https://www.rfc-editor.org/rfc/rfc8414.html#section-3.3).

`python3 scripts/cloud/verify-discovery.py` reproduced the live failure before deployment: `Issuer mismatch at https://cloud.bog.new`.

The prior MCP protected-resource discovery path did identify the Fly authorization server correctly. That did not make the copied authorization-server documents valid. The previous report's reliance on HTTP 200 and functional transport tests was insufficient evidence for strict issuer discovery. The scanner's internal rejection logic is not available; the standards violation is independently confirmed.

## Fix

- Canonical issuer, GitHub callback, browser sessions and console move to cloud.bog.new.
- The old Fly issuer remains a complete compatibility endpoint with matching metadata and response issuer for existing clients.
- Authorization codes bind to their initiating issuer, client, redirect URI, resource and PKCE challenge. Exchanges through the other issuer fail without consuming the code.
- MCP-only hostname stops serving a copied authorization-server document. It returns an explanatory 404 and directs clients through protected-resource metadata to cloud.bog.new.
- Existing stored registrations, access tokens, data and permissions are preserved. No registry migration.
- GitHub application retains both exact callback URLs, without wildcard matching. The cloud callback setting is active in the deployed service.

## Verification

The local real-HTTP tests exercise both issuer flows with a mocked GitHub provider, including approval, exact metadata issuer matching, incorrect-issuer exchange, old/new resource audiences, REST-only restrictions and token revocation. Live results are recorded below after deployment.


## Live results

Deployed commit `5faa684` to the existing Fly machine, without infrastructure expansion.

- Cloud and MCP suites: 124 tests passed. Relevant Clippy checks passed with warnings denied.
- `scripts/cloud/verify-discovery.py`: exact issuer metadata, protected-resource metadata and challenges passed on cloud.bog.new, mcp.bog.new and the legacy Fly hostname. Canonical callback and browser redirects passed.
- Real GitHub sign-in returned to cloud.bog.new/console with the existing personal and Flower workspaces. No repository permissions were requested.
- An existing approved device credential initialized MCP and fetched current context through all three hosts. Fresh Codex/Claude OAuth approvals were not repeated in this release; local transport tests cover both issuer flows.
- Disposable fixture write/read/change-wait, metrics, isolation and token revocation passed. Temporary credential `3d43d786-47dc-4334-973e-8f093a14de1c` was revoked and the temporary record removed. Example sanitized request ID: `705ccef9-0953-4edd-8906-91e29c57dbf1`.
- Cloudflare Is It Agent Ready: **100/100**, fresh scan at 2026-09-18 01:48:39 UTC; OAuth/OIDC and protected-resource checks passed.
- Is Agentic: **100/100**, fresh scan at 2026-09-18 01:48:49 UTC; all 11 essential checks passed (previously 98/100 with OAuth partial). This aggregate score does not mean every recommended check passed; indexing and other recommended checks remain partial.

### Separate capacity limitation

The final read-only check of the original chat Bog was blocked by HTTP 429 `capacity`: all eight resident worker slots were occupied. Latest sanitized request ID: `3730116d-a93b-413d-8f51-576c486323df`. This is worker capacity, not OAuth rejection or a request-rate limit. No original chat records or credentials were changed. Its post-release record fingerprint therefore remains unverified in this release. No running user workers were interrupted and the host limit was not increased.
