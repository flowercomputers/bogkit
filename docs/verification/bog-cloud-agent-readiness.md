# Agent-readiness follow-up — September 17, 2026

Deployed runtime commit `1d48c54` to the existing Fly prototype at approximately 16:27 UTC. No registry migration, infrastructure expansion, credential rotation, or existing-record mutation was performed.

Image: `registry.fly.io/flower-bog-cloud:deployment-01M2R2R1P41K5S63GRN66V2BWH`.
Digest: `sha256:6a34a19665c596ffb2d4e414a25a8f07a20bb10be3426c74d423b56967c2bce2`.

## Shipped

- Distinct device approval/error messages, with five-second polling versus ten-minute request/approval throttling guidance.
- Full native `/llms.txt` operation discovery and explicit guidance that MCP credential results can enter transcripts. HTTP issuance directly to a private file is the recommended secret-handling path.
- Public documentation, use cases, free prototype allowance, contact/about/privacy notes, canonical/social metadata, and structured application description.
- Robots rules, sitemap, homepage discovery links, Markdown negotiation, and useful public 404s. Protected resource routes retain authentication.
- OpenAPI response/error schemas and unique identifiers for all 33 documented operations. Actual records-router responses are checked against the relevant schemas.
- Actual authenticated rate-limit remaining/reset headers, with retry timing on exhausted request buckets.
- Public API catalog, current experimental MCP card, Agent Skills index and digest-checked usage skill, and ARD catalog. Public metadata CORS does not enable CORS on the data API.

## Verification

The cloud and MCP suite passed 91 tests during integration. Subsequent targeted contract, discovery, native-auth, and manifest tests also passed. Strict Clippy checks passed for both packages. Independent review found no remaining blocking regression.

After deployment, checked public documents, content types, Markdown/HTML negotiation, catalog HEAD, skill digest, unique OpenAPI operation IDs, unsupported OAuth metadata 404s, private-route 401s, authenticated `/v1/me`, and actual rate-limit headers. Confirmed GitHub login still sends HTTP 303 to GitHub and authenticated MCP initialize succeeds using protocol 2025-11-25. Viewed the new documentation page and followed its homepage link.

The legacy chat Bog `21214697-82be-44d6-9328-264cc344aedd` remains readable. Its record-data fingerprint before and after deployment was identical. No credentials or record contents were printed by these checks.

## Scanner results

Both requested sites were run through their browser controls after deployment; no checks were deselected to improve scores.

| Scanner | Before | Fresh result | Scope |
| --- | --- | --- | --- |
| [Is Agentic](https://is-agentic.com/scan/flower-bog-cloud.fly.dev) | 65/100 | 92/100 | Inferred App, unchanged selection; newly discovered capabilities increased recommended checks from 16 to 21 |
| [Is Your Site Agent-Ready?](https://isitagentready.com/flower-bog-cloud.fly.dev) | 0/100 | 60/100 | Same 15 non-commerce checks; nine pass, six fail |

Cloudflare's fresh result is timestamped 16:27:26 UTC. Passing checks: robots, sitemap, Link headers, Markdown, AI crawler rules, API catalog, MCP server card, Agent Skills, and ARD.

Is Agentic visibly completed with 92/100 and “Strong technical baseline.” It still displayed “Saving completed scan…” at verification time and retained an older page title and older narrative agent journey. The observed fresh technical score is verified; persistence of the shared snapshot and a fresh narrative journey are not claimed.

## Deliberate limitations

- Native GitHub identity and Bog bearer credentials work; Bog does not expose an OAuth/OIDC authorization server, OAuth protected-resource discovery, or WorkOS Auth.md registration. Those three Cloudflare checks remain unsupported rather than advertised falsely.
- WebMCP remains deferred. This release advertises the actual HTTP and remote MCP interfaces.
- DNS-AID/DNSSEC requires control of the discovery DNS zone; the current hostname is under Fly's domain.
- Content Signals specify content reuse policy. No training/input-use policy was invented on the user's behalf.
- Is Agentic still requests OAuth-scoped permission metadata and reports GraphQL recommendations despite this service having no GraphQL API. It also retains an onboarding-friction finding; the live docs explicitly describe the existing free, self-serve GitHub/device journey. No extra protocol or commercial feature was added solely to satisfy these checks.
- MCP cards and ARD remain evolving drafts. The card records its namespaced discovery identifier and the existing `bog-cloud` runtime name without renaming the running MCP server.
