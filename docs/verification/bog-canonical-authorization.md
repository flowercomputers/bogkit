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
- GitHub application retains both exact callback URLs, without wildcard matching. The active Fly callback setting is staged for the matching deployment.

## Verification

The local real-HTTP tests exercise both issuer flows with a mocked GitHub provider, including approval, exact metadata issuer matching, incorrect-issuer exchange, old/new resource audiences, REST-only restrictions and token revocation. Live results are recorded below after deployment.
