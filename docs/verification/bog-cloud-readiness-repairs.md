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

## Published verification

Deployed code revision **dad505c** to the existing Fly machine `4d895395c393e8` in iad, without changing machine size, volume, quotas or paid capacity. Image: `registry.fly.io/flower-bog-cloud:deployment-01M2R926T0EJ7ENEW6VD5MW5EG` (digest `sha256:c8fa177bae49fa4ab2c3f2ee117b8034dcaf990737cf3d49c94c5d1657dc7bfb`).

Live checks passed for health, HTML/Markdown, OAuth metadata and challenges, the two named scopes in OpenAPI, content policy, self-serve pricing discovery, structured rate-limit headers using an existing authorized agent, all exact Flower assets, and seven shared-layout pages. A browser agent actually called the deployed `bog_templates` WebMCP tool. The legacy chat's record-data fingerprint remained unchanged. The new OAuth authorization-code happy path was tested locally over actual HTTP/MCP with a fixture GitHub provider; no new production connection was silently approved as the user.

## Fresh scanner results

| Scanner | Before | After | Evidence |
| --- | --- | --- | --- |
| [Is Agentic](https://is-agentic.com/scan/flower-bog-cloud.fly.dev) | 92/100 | **100/100** | Persisted API report: 2026-09-17T18:16:50.341Z; browser snapshot 18:16 UTC. App remains selected, now explicitly declared. All 11 essential checks pass. |
| [Cloudflare](https://isitagentready.com/flower-bog-cloud.fly.dev) | 60/100 | **87/100, Level 5** | Fresh scan 18:16:57 UTC; 13/15 checks pass; no checks deselected. |

The scores are not claims that every recommendation passed. Is Agentic's bonuses bring the aggregate to 100 while its report retains partial findings. Its stored API reports recommended checks 14/22; the browser's App view displays 12/18, both showing 16.3/20. Earlier API and browser counts also differed, so those counts should not be treated as an exact before/after denominator.

Cloudflare now passes Content Signals, OAuth server discovery, Auth.md and WebMCP. Its Auth.md pass does **not** establish support for the distinct identity-assertion/anonymous-claim draft; that remains explicitly unsupported.

Cloudflare's two remaining flags:

1. **DNS-AID:** requires a Flower-controlled hostname. No control over the provider's `fly.dev` zone.
2. **Protected-resource target mismatch:** the scanner checks the homepage origin against metadata whose canonical resource is `/mcp`, and labels the successful JSON response a mismatch. Both `/.well-known/oauth-protected-resource` and the path-specific `/.well-known/oauth-protected-resource/mcp` serve the correct protected MCP resource and issuer. The authorization flow binds that resource. Do not change the canonical protected-resource identity to the public homepage just to satisfy this comparison. See the [MCP authorization specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization).

Is Agentic still reports search-index visibility, unauthenticated verification limits (protected API/MCP, self-serve onboarding, actual account rate limits), and partial docs/version-policy findings despite the deployed content and authenticated checks above. These are documented measurement limits, not reasons to expose private data or fabricate anonymous API results. Its old narrative agent journey is not evidence of a new authorized end-to-end app trial.
