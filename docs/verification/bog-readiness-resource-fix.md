# Readiness repair: distinct REST and MCP resources

## Change

The root protected-resource metadata now describes a supported origin-level REST resource. The path-specific document continues to describe `/mcp`. Authorization-server metadata enumerates both. HTTP API challenges point to the REST document; MCP challenges keep the MCP document.

This is backed by authorization behavior: the requested resource is recorded with the approval grant, must match exactly at code exchange, and is stored with the issued token. New origin-bound tokens are accepted only by REST. Existing MCP-bound tokens retain their previous REST compatibility. Unknown audiences, cross-resource code exchange, and REST-only tokens presented at MCP are rejected. No registry migration or existing credential rewrite.

The approval screen identifies the selected resource. Authentication guidance and public docs describe both flows. This supersedes the earlier diagnosis's choice to leave the root compatibility alias unchanged: a real REST OAuth resource is now implemented instead of changing the metadata alone.

## Verification

The real HTTP/MCP OAuth transport test covers both scopes for both resources, exact metadata/challenges, cross-resource exchange rejection, REST-only MCP rejection, read-only write denial, existing MCP compatibility and revocation. All 122 cloud/MCP tests and 15 JavaScript tests passed; strict cloud/MCP Clippy passed. Live scan results will be recorded after rollout.

## Remaining boundary

DNS-AID for the Fly-owned hostname cannot be configured from this repository or the Fly application. A Flower-controlled hostname is a separate DNS/deployment prerequisite. No checks are disabled or unsupported capabilities advertised to raise scores.

Standards: [RFC9728](https://www.rfc-editor.org/rfc/rfc9728.html), [MCP authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization).
