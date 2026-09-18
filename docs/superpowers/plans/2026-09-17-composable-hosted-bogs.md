# Composable Hosted Bogs Implementation Plan

Goal: implement the approved composition-based hosted Bog model, preserving records-v1.
Spec: docs/bog-language-cloud-architecture.md; scope narrowed by the accepted plan below.

## Binding decisions
- JSON definitions, one string-keyed JSON-object source input, many resources.
- Shared bog-definition and bog-runtime crates; enum-dispatched Fold terminals, no uploaded code.
- Catalog: tables, filter, projection, count, numeric stats, numeric rankings, BM25 and ESE/Anny cosine search.
- JSON Pointer fields. Missing ordered comparisons false, missing differs from null. Missing terminal fields skip, wrong present types reject whole writes. Search strings joined with newlines.
- Explicit exposed operations, shared discovery/schema/API descriptors; management authority for definition edits.
- Synchronous atomic derived updates, checkpoint before ack. Existing restart-aware cursors remain notification cursors.
- Additive updates only, expected revision check, persistent jobs, reads available while writes paused, separate candidate built from stable logical export, verified count/digest, durable atomic activation, crash recovery and rollback before activation.
- records-v1 storage and APIs, existing credentials, data and permissions preserved. Old backups remain supported; new archives include definitions/versions.
- One pinned 512d f32 ESE model/tokenizer, checksum verified, Anny cosine. BM25 tokenizer versioned.
- Existing 16MiB source/1MiB request/100 batch limits. 16 resources, 8 stages, one semantic index, 10000 vectors, 8KiB extracted text, 4KiB query, 50 hits. One host build, 5 minute timeout, capacity admission.
- No joins, multiple inputs, grouped aggregates, retention, weighting/hybrid, parser, arbitrary models, dashboard or paid expansion.

## Tasks
1. Isolated worktree at committed cloud baseline; compatibility fixtures.
2. Definition validation/normalization/digests/contracts plus local configurable Fold runtime.
3. Registry/supervisor/worker/service and HTTP/MCP/WebMCP integration.
4. BM25 and pinned ESE/Anny synchronous search.
5. Additive builds, activation/recovery, backup compatibility.
6. Shared local/HTTP/MCP conformance, docs, feature flag, hosted canary if available/authorized.

## Acceptance
Agent discovers and validates todo composition, creates it, mutates records and observes filtered list/count/ranking, adds text/semantic search on populated data, reads during build, retries paused writes, searches after activation, restarts and restores without changed records/permissions. Local, HTTP, MCP agree.
Test malformed definitions/types/quotas, rollback after index updates, dimension errors, private resource access, stale revisions, timeout/disk failure and crash activation boundaries. Run existing Fold/serve/cloud-records/cloud/cloud-mcp and WebMCP suites plus new tests. Do not claim hosted deployment without actual verification.
