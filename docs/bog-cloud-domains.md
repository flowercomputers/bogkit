# Bog Cloud custom domains

- Public site and REST API: https://cloud.bog.new
- Dedicated MCP endpoint: https://mcp.bog.new/mcp
- Compatible MCP endpoint: https://cloud.bog.new/mcp
- Existing service and authorization issuer: https://flower-bog-cloud.fly.dev

Cloudflare DNS-only CNAMEs for `cloud` and `mcp` point to Fly's app-specific target `6kxn080.flower-bog-cloud.fly.dev`. Fly terminates HTTPS with managed certificates. No extra machines or paid services were added; the existing `bog.new` website is untouched.

This is an additive domain rollout. GitHub callback, browser login, console, device approval, and OAuth issuer remain on the existing Fly hostname. New-domain browser entry points redirect there before creating a host-only cookie. No session or bearer credential is passed between domains in a URL. Existing client registrations and tokens remain valid.

New-domain protected-resource metadata describes its exact REST or MCP address and identifies the existing authorization server. That server accepts only the enumerated service audiences and binds each authorization code to its requested audience. REST-only tokens remain rejected by MCP. All names reach the same permission layer and data.

Use the complete `/mcp` URL when configuring a client. Moving the authorization issuer or console to a new hostname is a separate migration; do not change the GitHub callback environment variable as a DNS-only change.

DNS discovery is published as `_index._agents.cloud.bog.new HTTPS 1 cloud.bog.new. mandatory="alpn,port" alpn="h2" port="443"`. The existing `bog.new` zone is signed with DNSSEC. The record advertises only the HTTPS transport actually served by Fly.
