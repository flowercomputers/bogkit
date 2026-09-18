# Readiness scans after agent observability

Target: https://flower-bog-cloud.fly.dev/
Deployed observability release: `1d528c5`.
Both scans requested through their browser Rescan/Scan controls. Existing selections preserved; no checks disabled to increase a score. Is Agentic remains declared App.

| Scanner | Fresh result | Timestamp |
| --- | --- | --- |
| [Cloudflare Is Your Site Agent-Ready?](https://isitagentready.com/flower-bog-cloud.fly.dev) | **87/100, Level 5 Agent-Native**, 13/15 applicable checks pass | 2026-09-17 23:27:54 UTC (browser displayed 19:27:54 EDT) |
| [Is Agentic](https://is-agentic.com/scan/flower-bog-cloud.fly.dev) | **100/100**, 11/11 essential, 12/18 recommended, 36 positive bonus signals | Snapshot 2026-09-17 23:28 UTC |

Both aggregate scores match the earlier successful baseline. Is Agentic's total is capped at 100 after bonuses; it does not mean every recommended check passes.

## Cloudflare remaining flags

- DNS-AID records absent under the provider-owned Fly hostname.
- OAuth protected-resource check compares the public homepage with the canonical protected `/mcp` resource and reports a resource mismatch. Its separate MCP probe correctly sees a 401 with the path-specific resource metadata challenge. No authentication configuration changed for this scan.

Passes include OpenAPI/API discovery, OAuth authorization-server discovery, MCP server card, agent skills, WebMCP (two public tools), Markdown negotiation, robots/content signals, sitemap, links and ARD.

## Is Agentic remaining partial findings

- API/docs linked from homepage: 67%, describes docs as thin or unreachable.
- Public API reachable endpoints: 43%, no authenticated surface verified.
- Onboarding: 50%, described but not exercised.
- MCP: 83%, endpoint found but tools/scopes inaccessible without credentials. Scanner explicitly recommends keeping authentication enabled.
- Rate-limit headers: 50%, documented but not observed without credentials.
- Versioning/deprecation: 67%, versioned paths detected but policy not detected.

Typed REST errors pass. Response schema coverage is reported as 93%; 40/40 operations have IDs, 38/40 typed schemas. Scanner also incorrectly attributes an npm `fly-cli` package to this service; that bonus is not evidence of a Bog CLI.

## Freshness and direct checks

The initial refresh replaced an older 100 snapshot with an **86/100 snapshot dated 20:22 UTC**, still predating this test. Its essential failures were incomplete network probes. One retry produced the fresh 23:28 UTC 100/100 report above. The stale 86 is not counted as a current regression.

Direct public checks during this run confirmed:

- Homepage: HTTP 200, Markdown for `Accept: text/markdown`, HTML for `Accept: text/html`, both with `Vary: Accept`.
- Missing page: HTTP 404 with Markdown guidance.
- Linked `/docs`: HTTP 200, with version/deprecation policy and new metrics guidance present.

These scanners do not authenticate into private Bogs. They therefore do not replace the separately completed HTTP/MCP checks of metrics, token isolation, change waiting, revocation or sleeping-worker behavior. No service code or deployment changes were made for this scan.
