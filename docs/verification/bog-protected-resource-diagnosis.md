# Protected-resource scanner mismatch: isolated

Verified 2026-09-17 at 23:35 UTC against the live deployment.

## Reproduction

- Scan `https://flower-bog-cloud.fly.dev/`: OAuth Protected Resource fails. The root metadata document describes `/mcp`, and the scanner compares it with the supplied homepage URL.
- Scan `https://flower-bog-cloud.fly.dev/mcp`: **OAuth Protected Resource passes**. Scanner reads the 401 challenge, retrieves the path-specific metadata, validates the matching resource and concludes metadata found (both).
- No scanner checks deselected and no service/authentication changes made.

Direct HTTP independently confirms `/mcp` returns 401 with `resource_metadata="https://flower-bog-cloud.fly.dev/.well-known/oauth-protected-resource/mcp"`. That document returns the exact `/mcp` resource identifier. The root metadata document is a compatibility alias with the same resource value.

## Fix

For evaluation, target the protected-resource check at the discovered MCP endpoint. Continue evaluating homepage content at the homepage. A whole-site scanner should discover the endpoint from the MCP server card and evaluate its challenge/metadata there, or accept an explicit per-check resource URL.

Do not replace the canonical MCP resource with the homepage or weaken audience checks. A homepage-matching metadata document would require an actual origin-level protected resource supported by authorization and token validation; simply changing JSON would advertise unsupported behavior.

The endpoint-specific full scan scored73 because homepage-oriented checks were applied to an authenticated protocol endpoint. It is **not** a replacement for the homepage87 score or evidence of a site regression. Its isolated protected-resource pass is the relevant result.

## Standards

- RFC9728 section3.3 requires the metadata resource to match the resource being discovered: https://www.rfc-editor.org/rfc/rfc9728.html#section-3.3
- MCP2025-11-25 permits endpoint-specific or root discovery and prioritizes the challenge's resource_metadata URL: https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization

Current MCP path-specific discovery is verified. The root alias should be understood as compatibility discovery, not a declaration that the public homepage is the protected resource.
