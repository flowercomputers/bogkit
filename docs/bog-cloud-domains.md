# Bog Cloud custom domains

- Public site, REST API, GitHub sign-in, console and canonical OAuth issuer: https://cloud.bog.new
- Dedicated MCP endpoint: https://mcp.bog.new/mcp
- Compatible MCP endpoint: https://cloud.bog.new/mcp
- Legacy service and OAuth issuer: https://flower-bog-cloud.fly.dev

Cloudflare DNS-only CNAMEs for `cloud` and `mcp` point to Fly's app-specific target `6kxn080.flower-bog-cloud.fly.dev`. Fly terminates HTTPS with managed certificates. No extra machines or paid services were added; the existing `bog.new` website is untouched.

## Authorization discovery

Each authorization issuer publishes matching metadata at its own `/.well-known/oauth-authorization-server`. The cloud issuer's endpoints use cloud.bog.new; the legacy issuer's endpoints use the Fly hostname. New MCP resource metadata advertises the cloud issuer. The Fly resource continues advertising the legacy issuer for existing clients.

The MCP-only hostname does not publish an authorization-server document: that path returns 404 with a link and explanatory JSON pointing to the cloud issuer. Its protected-resource document identifies the real authorization server. An issuer copied under another domain is not a valid substitute for issuer discovery.

Authorization codes are bound to the issuer that initiated the request, as well as the client, redirect, resource and PKCE challenge. Exchanging a code through the other issuer is rejected. Existing stored access tokens and registrations remain valid; original Fly resource audiences are still accepted with the same permissions. REST-only audiences remain rejected by MCP.

## GitHub and browser sessions

The organization-owned GitHub OAuth application registers both exact callbacks, with no wildcard matching:

- https://cloud.bog.new/auth/callback — active callback, configured by `BOG_GITHUB_REDIRECT_URI` in Fly secrets.
- https://flower-bog-cloud.fly.dev/auth/callback — retained for rollback.

Browser login, console and approval entry points on other hosts redirect to cloud.bog.new before creating a session. Cookies remain host-only. Existing browser users may need to sign in once on the new hostname; bearer credentials are unaffected. A login already in progress during the cutover may need to be restarted. Session and bearer credentials are never transferred between hosts through URL parameters.

Keep the legacy issuer until existing client connections have been deliberately migrated or retired. Do not remove its metadata or change its `iss` response to the new domain: cached clients must receive the issuer they requested.

## Verification and DNS discovery

Run `python3 scripts/cloud/verify-discovery.py` for read-only live checks of issuer matching, endpoints, resource metadata, challenges, GitHub callback and browser redirects. This does not grant access, exchange credentials, or constitute a full user approval test.

DNS discovery is published as `_index._agents.cloud.bog.new HTTPS 1 cloud.bog.new. mandatory="alpn,port" alpn="h2" port="443"`. The existing `bog.new` zone is signed with DNSSEC. The record advertises only the HTTPS transport actually served by Fly.
