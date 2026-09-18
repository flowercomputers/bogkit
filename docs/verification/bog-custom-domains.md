# Custom-domain rollout — 2026-09-18 UTC

Runtime commit: `0f1196d` on `codex/personal-bog-cloud`, deployed to the existing Fly application and machine. No new compute resources, registry migration, secret rotation, or changes to the apex website.

## Addresses

- Public site and API: https://cloud.bog.new/
- Dedicated MCP: https://mcp.bog.new/mcp
- Existing Fly URL and cloud.bog.new/mcp remain supported.
- GitHub sign-in, human console and OAuth issuer deliberately remain at the Fly hostname. New-domain browser entry points redirect before setting a cookie. See [domain notes](../bog-cloud-domains.md).

## Verified

- Both Cloudflare DNS-only CNAME records point to the app-specific Fly destination. Both Fly-managed TLS certificates are issued and active.
- 124 cloud/MCP tests pass. The additional real-transport domain test covers metadata, exact resource selection, unchanged issuer, authorization-code exchange, wrong-resource rejection, REST-only/MCP separation and revocation, using a mocked GitHub provider.
- Two final public-document tests pass; strict cloud/MCP lint checks pass.
- Live authenticated HTTP and MCP initialization/context calls succeed through all three hosts using an existing approved helper credential.
- Live new-domain disposable record write/read/change-wait, metrics, error isolation, and credential revocation pass. Temporary scoped credential `fbc2690d-626f-4c01-9556-86e036d7fbb6` revoked; temporary record removed.
- Existing chat record fingerprint is unchanged and readable through the new cloud hostname.
- Homepage and connect page checked in Chrome. MCP instructions display the new dedicated address; login/console redirects tested without creating cross-domain cookies.
- This rollout did not repeat fresh human OAuth approval in the actual Codex and Claude clients. Their preceding-release acceptance evidence remains separate from the new-domain transport and authorization tests.

Sanitized live request IDs: `3b837730-c953-46a6-8c13-d2a730aaab6b`, `4e619e9d-6394-41c6-ab7e-338b1d7d3b85`, `c127632b-69ce-42cd-87e1-c02ae3823e50`.

## Readiness

[Cloudflare](https://isitagentready.com/cloud.bog.new): **100/100**, all 15 applicable checks pass, scanned 2026-09-18 01:14:48 UTC. DNS discovery and OAuth protected-resource checks both pass. No check was disabled to improve the score.

[Is Agentic](https://is-agentic.com/scan/cloud.bog.new): saved rescan **98/100**, 2026-09-18 01:16 UTC; 10/11 essential and 12/18 recommended App checks pass. Its OAuth check claims no metadata endpoint responded. Direct HTTPS requests to the same endpoint return 200 with valid existing-issuer metadata, and local authorization plus live authenticated operations pass. Do not present this scanner claim as an observed service outage, or claim a fresh client authorization trial was performed. The saved rescan retained the same score and finding.
