# Bog Cloud readiness repairs — 2026-09-17

Baseline: deployed Flower design at c22a173. The fresh 17:41 UTC scans scored Is Agentic 92/100 and Cloudflare 60/100. The saved Is Agentic JSON report contains additional findings beyond the UI's first five recommendations. No scanner checks are being disabled to raise the score.

## Findings and disposition

| Finding | Change or limit |
| --- | --- |
| OAuth discovery / scoped permissions / protected-resource discovery | Self-hosted authorization-code flow, PKCE S256, exact registered callbacks, explicit existing GitHub-backed consent, resource binding, read-only and read/write scopes. RFC 8414 and RFC 9728 documents and MCP challenges reflect the actual implementation. No WorkOS or other paid auth provider. |
| Revocation and scope safety | OAuth secrets are hashed in private native auth storage and linked to existing revocable account credentials. Read scope cannot create, write, or mint. Delegation still cannot perform human-owner administration. Account suspension, membership removal and console revocation retain their existing checks. |
| WebMCP | Real public discovery/template tools; signed-in console tools for workspaces, listing, description and creation. Explicit workspace selection defaults to personal, never the mutable UI selection. No credential secrets are returned. Current document.modelContext and older navigator.modelContext supported. |
| Content Signals | User approved search=yes, ai-input=yes, ai-train=no for public documents. Protected paths remain disallowed and authenticated. |
| Onboarding / when-to-use / thin docs | Explicit free tier, self-serve credentials, useful use cases, actual disposable-Bog testing path, and no claim of a separate sandbox. Machine-readable pricing and interface declarations in /v1. |
| About, Contact, organization information | Substantial factual pages. Contact email and Brooklyn location verified against Flower's public contact page; no street address invented. |
| Versioning / deprecation | Published major-version policy and future Deprecation/Sunset/migration Link behavior; no invented notice period or active sunset. |
| Rate-limit headers | Existing enforced buckets now also expose structured RateLimit-Policy and RateLimit headers, retaining old fields. Public requests do not invent an authenticated quota. |
| MCP auth-only scan | Keep authentication. Verify tools with an authorized actual MCP SDK client rather than exposing private tools anonymously. |
| Search discoverability | Product names, public links, sitemap and factual structured metadata strengthened. External indexing/ranking is not under direct control and may lag deployment. |
| GraphQL error/schema/deprecation suggestions | No GraphQL interface exists. HTTP/MCP capabilities explicitly documented; no synthetic GraphQL endpoint added. |
| Auth.md agent-registration draft | The guide now explicitly identifies itself as auth.md and documents real device/OAuth flows. The separate identity-assertion/anonymous claim protocol is not implemented or falsely advertised as agent_auth. Standard OAuth client registration is distinct from that draft. |
| DNS-AID | fly.dev DNS is not controlled by Flower. A Flower-controlled hostname is required; requested separately. No domain migration or fabricated DNS record. |

## OAuth prototype limits

Public clients register exact HTTPS or HTTP loopback-IP callbacks. Registrations expire after 30 days. In-flight grants are bounded, rate-limited, expire after ten minutes, and are lost on restart. Codes last 60 seconds and are single-use. Access tokens persist and expire after 30 days. Reconnect after expiry: refresh tokens and client metadata document discovery are not advertised. There are no OIDC ID tokens. One protected resource covers this deployment's HTTP and MCP API, advertised at its canonical /mcp URL. An OAuth client must validate state and issuer; tokens never appear in the consent page.

Auth storage gains private additive tables; the Bog registry remains version 4. Existing native agent credentials, scoped app credentials, legacy chat access, workspaces, quotas and the Flower design are preserved.

## Local verification

- Full cloud/MCP suites: 104 tests passed, including an actual HTTP OAuth exchange followed by MCP SDK calls.
- Negative cases: invalid callback, wrong resource, missing CSRF, wrong PKCE, code replay, read-only creation/write/mint, delegated deletion/account credential issuance, console and protocol revocation.
- Reopened auth storage preserves scopes; wrong audience, expiry and account suspension fail closed.
- Four JavaScript tests verify public/signed-out/private tool registration, explicit workspace choice, idempotency, and absence of secret results.
- Browser preview actually discovered and called bog_templates through WebMCP. Documentation rendered in the existing Flower layout.
- Strict Clippy and JS syntax checks passed.

Deployment verification and refreshed scanner results will be recorded after release.
