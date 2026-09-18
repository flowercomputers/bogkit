# Independent critique repairs — 2026-09-18

Source: the independent Claude report at https://claude.ai/artifact/3JNdA3wSeevSPeancPQpd3.

Runtime source: `012384a`. Hosting and registry version 5 remain unchanged.

## Disposition

1. Batch: canonical `{ops:[...]}` across resource metadata, HTTP and MCP. HTTP bare arrays and `operations` remain accepted; conflicting envelopes fail.
2. Installer: canonical origin; immediately flushed approval instructions, including redirected output.
3. Approval reuse: explicit private `--auth-file` reuse, with ownership/permissions/origin checks. A fresh installer still needs its own approval; it never reads another client's private credential store.
4. Search: opt-in bounded `include_fields` projection; semantic `score = 1 - distance` and optional `max_distance`. No universal relevance threshold is implied.
5. Updates: durable timestamps, stage, record counts, paused-write state and operational events; recovery reporting survives manager restart. Unknown/early counts are null.
6. Wait: canonical `timeout` seconds, maximum 25. Compatible spellings remain accepted and conflicting arguments fail. Hosted resource waits use existing permission and waiter controls.
7. Schemas: real action response schemas plus separate hosted request/response envelopes. Tested against returned values.
8. Empty queries: intentional terminal defaults are documented and tested, rather than removed.
9. Recovery errors: unavailable resource actions direct callers to resource discovery instead of workspace guesswork.
10. Allowances: local flag retained; effective allowance and its source distinguish account inheritance from a workspace override.
11. BM25: actual tokenization and absence of stemming are documented.
12. MCP: public guidance advertises `https://mcp.bog.new/mcp`. Alias-specific OAuth resource metadata remains correct for each requested URL.
13. Permissions: app management requests fail before malformed operation arguments can obscure the permission error.
14. Identity: defined Bogs identify themselves as `kind: defined`, with null template fields; template Bogs retain their template identity.
15. Revocation: repeats and concurrent revocations produce one change/event.
16. Cold-start diagnosis: scoped credentials can inspect worker metrics; management-only event access remains restricted.

## Local verification

- Combined persistence, definition, runtime, worker, cloud, MCP and model-pinning suite: 300 Rust tests passed.
- Browser tool tests: 17 passed.
- Private installer tests: 14 passed.
- Supporting Python tests: 12 passed.
- Final canonical-guide follow-up: 4 focused contract tests passed; 3 server-card discovery tests passed.
- Clippy completed successfully with warnings in existing dependency code and one non-blocking style suggestion in the updated runtime.
- Independent review found two response-schema mismatches; both were corrected and tested before packaging.

## Live verification

Deployed image `registry.fly.io/flower-bog-cloud:critique-repairs-012384a`, digest `sha256:00aff32ea9273f5026f784fc5db888e82822ccdbc1cf8e1eca477187dc6c4d62`, on existing machine `4d895395c393e8` (1 shared CPU, 1 GiB, existing volume).

- Disposable Bog `994d92e2-9875-4e27-bae3-81527eea31ab`: safe retried creation, HTTP/MCP canonical batches, two definition updates, projected text/semantic search parity, similarity scores, job progress, scoped metrics, cross-transport change waiting, denied management requests and cross-Bog isolation all passed. Test credentials were revoked and rejected afterward; the Bog was then deleted and returned 404.
- Live discovery passed for all three hostnames, including correct OAuth challenges, issuer/audience metadata and canonical server cards. Published examples validated; 28 MCP tools were discoverable and two guide resources were read.
- Existing six legacy Bogs preserved. Chat `21214697-82be-44d6-9328-264cc344aedd` remained readable with the same record fingerprint before and after deployment.
- Sanitized request references: creation `5604bdb2-d680-4429-9b89-eea6611c481f`; HTTP batch `fbe7d968-f357-47da-a356-f7a9c3461029`; MCP batch `25537d7d-1852-49be-b209-64c99f7b5c3e`; post-revocation denial `2ca2a7ee-e6e2-48ce-b5c5-5282a067a66c`; preserved chat `c79e800f-a0b8-47e8-a00c-78b07bdccfc2`.

The original critique Bog and its credentials were not removed. Only resources created by this repair canary were cleaned up.

This repair uses real HTTP/MCP transport checks; it does not claim a new clean Codex or Claude user trial, a second person's authorization, or a new readiness scanner score. The original critique agent should repeat its reported failures after deployment.

## Third-party retest handoff

Start again from https://cloud.bog.new using your normal account. Re-run the original sixteen repros, especially schema-driven batch writes, empty default queries, resource wait, search projection and semantic filtering, update-job progress, allowance inheritance, and repeated credential revocation. Use a fresh disposable Bog for writes. Confirm the private installer displays its approval link when output is captured; reuse only an explicitly authorized private auth file if one already exists. Preserve existing app records and never include credential values in the report. For failures, report the request ID, expected published schema, sanitized request, and observed response. Distinguish documented compatibility choices from failures.
