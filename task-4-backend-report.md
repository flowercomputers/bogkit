# Shared REST/MCP backend integration

Implemented in cloud/src/{service,http,gateway,contract}.rs and cloud-mcp/src/{tools,transport}.rs.

- WorkOS configuration loads once, all required six variables or fail closed; optional separately registered device client. Public configured WorkOS disallows operator bearer; opaque app credentials remain supported. JWT-shaped failures never fall back. Root owns explicit legacy binary startup opt-in.
- Verified JWTs become workspace-scoped agent principals. Server sessions become human principals. Explicit query/tool workspace selection checks membership. Scoped creation rechecks membership inside the registry transaction. Listing never falls back to global for a workspace identity.
- Browser routes include login/callback/session/logout/refresh, binding cookie clearance, no-store private responses and exact Origin plus CSRF for cookie mutations. Console session returns account ID, memberships and CSRF only. No provider tokens enter response bodies.
- REST owner membership/invitation management, signed-in preview/accept, single-Bog token metadata/issuance/revocation, explicit ID-confirmed deletion followed by retryable cleanup, actual usage endpoint.
- MCP has 13 tools, explicit workspace_id schemas, no membership administration, single-Bog application credential issue/list/revoke. REST/MCP share operation service and public descriptions. Creation defaults to records-v1 and aggregates missing name/idempotency requirements.
- Public discovery includes templates, overview, auth.md, llms.txt, protected-resource metadata, OpenAPI with bodies/path/query/header parameters and management routes. Unauthorized REST/MCP advertises resource metadata. Authentication configuration flag in overview and health.
- Shared per-identity 600 requests/minute bound and 64 concurrent normal operation bound. Dedicated bounded change waiters bypass normal operation semaphore. REST GET changes and MCP wait_for_change share cursors. Actual worker responses gain cursor based on their own seq while lease is held; never separate snapshot seq. Defaults/max wait 25s, with reset and timeout results from changes module.

Verification performed:
- cargo check -p bog-cloud -p bog-cloud-mcp passed.
- cloud gateway_integration: signed fixture JWT isolation, personal defaults, explicit foreign workspace denial, public operator denial, invalid JWT, app unable to list/provision credentials; browser mock provider login/callback/session, mutation denied without Origin/CSRF or wrong origin/token, allowed with both, logout invalidates session: 2 passed.
- Existing MCP authorization 3 passed; protocol 5 passed (includes REST/MCP changed cursor consistency and timeout).
- Existing REST 1 and service 3 passed.
- contract example/path completeness unit test passed.
- Clippy attempted; dependency changes.rs nested-if lint reported to owning agent. Repeat coordinated by root once dependency edits settle.

No external provider, real account, production deployment, real browser, or fresh installed client validation claimed. Static console assets and visual testing owned by root. Provider release gates remain required. OpenAPI structural tests validate documented examples and path bindings, not a third-party full OpenAPI validator.
