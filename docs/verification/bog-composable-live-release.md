# Composable Bog Cloud live release — 2026-09-18

## Published state

Composition is enabled on https://cloud.bog.new; remote MCP is https://mcp.bog.new/mcp. The legacy Fly hostname remains compatible. Runtime source is `69f0e42`, image `registry.fly.io/flower-bog-cloud:composable-release-69f0e42`, digest `sha256:ed666a6e084efd9112203df976961dc7cc650dc6303cff7e5c19b1b4ed7fd9b1`.

The existing Fly machine `4d895395c393e8` in iad remains one shared CPU, 1 GiB RAM and its existing 3 GiB encrypted volume. Final checks show started, uncordoned, health passing and `BOG_CLOUD_COMPOSABLE=true`. No hosting expansion or new paid service was provisioned.

## Documentation and agent surface

The public homepage, /docs, /llms.txt and /v1 discovery describe composition, effective availability and the discovery→validate→create→query/search→plan/apply/status workflow. The authenticated component catalog now serves the complete definition schema plus three executable examples: todo, text search and semantic search. The MCP component guide exposes the same catalog; quick-start instructions, operation descriptions, output envelopes and WebMCP descriptions were updated. The old validation example incorrectly posted a bare definition; it now uses the API's required `definition` envelope.

Focused verification passed 27 Rust tests and 13 WebMCP JavaScript tests. Strict Clippy passed with warnings denied. The phased live-canary runner passed four offline tests. Independent review found no remaining blocking issues. These checks supplement the preceding combined integration suite (285 Rust and 17 JavaScript tests); that unchanged runtime suite was not unnecessarily repeated for the documentation-only changes.

## Upgrade and recovery boundary

Public traffic was cordoned and the machine gracefully stopped before a complete volume snapshot. Snapshot `vs_Gz2AzD404L1SAM8ow98p` completed at `2026-09-18T04:23:55Z`, digest `5978d34f7105e456ca1102b2cdf8cc1bfd438b57c6d0ea452933f7583cc2c20c`, five-day retention. The new image first deployed with composition disabled. Original chat fingerprint, existing delegated-agent access and OAuth discovery passed before enabling composition.

Registry v5 is now live. The previous binary cannot be used against this registry. Keep the completed recovery snapshot; never lower the registry version marker. Restoring that snapshot after subsequent writes would lose those writes, so prefer a compatible forward fix. This snapshot is a bounded rollout recovery measure, not a new backup infrastructure project.

## Live checks

- Public docs, OpenAPI, WebMCP script and homepage guidance passed served-content checks. All three catalog definitions validated through MCP. The component and quick-start MCP resources were read successfully. tools/list exposed 28 tools, including all new composition operations.
- The existing approved agent credential still accessed /v1/me, /v1/workspaces and /v1/components after migration. Canonical and legacy OAuth issuer metadata, all three protected-resource challenges, GitHub callback and console redirects passed before and after restart.
- One uniquely named disposable Bog was created through HTTP; an identical idempotent retry returned the same ID. Three sample records were written with a scoped credential. Populated definitions were upgraded first to text search through HTTP, then semantic search through MCP. Both durable jobs completed.
- Live HTTP/MCP results agreed for filtered/projected records, count, ranking, BM25 search and semantic search. Definition/resource inspection and update-job status worked. The private resource was inaccessible. Read credentials could not write or access the original chat Bog. All test credentials were revoked and subsequent requests returned 401.
- Change waiting woke after a disposable write; the temporary record was removed. A private definition-aware backup was restored into a second uniquely named Bog; records, maintained count, text search and semantic search matched.
- Eighteen read requests cycled nine existing disposable fixtures twice, exercising resident-worker pressure without waiting for the idle timeout. Pressure eviction was observed and the chat fingerprint remained unchanged.
- A real graceful Fly machine restart completed. The original canary and restored copy retained their records and indexes; completed definition jobs, HTTP/MCP queries and change waiting worked after reopening.
- Exact-ID-confirmed deletion removed only the two new disposable Bogs (`c82a7921-bfcb-4b71-87e0-f07ba6951a2d`, `52da7985-515d-4be0-900b-88d1be11a885`); follow-up reads returned 404. The original six legacy resources remained and the chat record fingerprint matched its pre-upgrade value. The small synthetic backup archive `48a6bc55-5ece-4324-9a38-d6de967acdde` remains as a recovery-test artifact.
- Sampled recent live logs contained neither the known owner/delegated credentials nor bearer-token prefixes. This is a sampled check, not a historical log audit.

The phased canary reported 61 request observations for creation/build/verification, 39 after restart and 3 for cleanup (HTTP observations plus separate MCP result observations). Useful sanitized IDs: post-upgrade chat `53ddffd4-8d91-4963-876b-a5b4af8929a3`; component discovery `36543dca-842d-43f6-ad16-f08b781032a2`; MCP component resource `fefb8203-fbd6-428d-b296-97d7f11b10c9`; final chat `8aad9a3c-c56a-4d17-9460-8d3891533956`; deleted canary 404 `cea58231-a2ed-4a05-b3e9-ac589457079c`.

## Actual host observations and limits

Sixty one-second host samples during the small canary reported minimum MemAvailable 801,344 KiB (782.6 MiB), maximum sampled manager/worker RSS sum 36,488 KiB (35.6 MiB), and up to two observed workers in that sampling interval. The separate nine-fixture pressure test passed afterwards. These are sampled observations, not instantaneous peaks, a load test, a guarantee for eight simultaneous semantic workloads, or a supported-user-count claim. The earlier isolated 1 GiB/one-CPU image test remains complementary evidence for nine Bogs and 900 records.

Allowances remain unchanged: ordinary workspaces three retained Bogs, legacy configured allowance eight, existing uncapped flags preserved, platform ceiling 32 retained Bogs, eight resident workers and two starts. Composition has its own effective limits in the authenticated catalog; it does not add paid capacity.

The rollout exercised real HTTP and MCP transport with existing credentials, not fresh interactive Codex/Claude GitHub sign-ins. No new client-authorization claim, browser WebMCP invocation claim or agent-readiness scanner score is made here. Browser tool contracts were tested locally and their deployed script was checked. A third-party agent can now start from the public homepage and test the expanded live surface.

## Test-run corrections

Fly rejected a requested 120-second restart timeout before performing it; the supported 60-second graceful restart passed. The private restore-cleanup helper initially expected 204 while deletion correctly returned the documented 202; deletion was confirmed by 404, and no additional resource was touched. Neither issue required a service code change.
