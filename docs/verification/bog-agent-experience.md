# Agent-experience verification — 2026-09-18

Historical first-pass baseline (before the phased improvement pass below): local checkout changes following the Scatter feedback. No production rollout or new blind-agent trial is claimed.

- Python helper/client suite: 22 tests passed. Includes pending approval, explicit slow-down backoff, denial, expiry, cancellation, private file permissions, no credential output, additive-definition preservation, revision conflict, failed jobs, and transient capacity rejection.
- Cloud unit suite: 72 passed. Includes actual handoff metadata checked against its corrected OpenAPI response fields.
- OpenAPI integration: 3 passed. Service integration: 4 passed, including a three-Bog inventory below 6,000 bytes with no nested request schemas and complete schemas still available from detail.
- Runtime suite: 20 passed, including search diagnostics after insertion/removal and exclusion of unexposed resources.
- Composable acceptance: 3 passed, covering mutation, additive search, restart and backup. Public discovery: 2 passed over local HTTP.
- Real disposable cloud server plus Python client: provisioned a records Bog, wrote a cabin record, added semantic and BM25 indexes sequentially, confirmed existing record retention, compared real search results, verified diagnostic counts after insertion/deletion, issued a scoped app credential, and started the notebook. Direct private-file paths and unexpected Host/Origin requests were rejected.
- Browser: saved a new note, reloaded and observed persistence, searched “quiet woodland retreat” with the cabin first in Meaning and zero matches in Words, and inspected the rendered layout in an isolated in-app tab.

The first sandboxed cloud run could not bind local sockets. Repeating with local socket permission passed. The first real client trial caught a brief 429 capacity rejection between consecutive definition builds; bounded retries of explicit 429 responses fixed it. Revision conflicts and uncertain writes are not blindly retried.

Reproduce the disposable server/client flow after building `bog-cloud-server` and `bog-records-worker`:

```sh
python3 scripts/cloud/verify_agent_starter.py
```

The harness now chooses temporary loopback ports. Add `--typescript` to exercise the TypeScript notebook. `--preview` retains the disposable preview until interrupted. The test generates temporary local credentials privately and does not touch production.

Authentication continuation was exercised with controlled token-endpoint responses; a new human GitHub approval and a fresh context-free agent session were not performed. At that baseline MCP Apps was researched but not implemented. The phased pass now includes a read-only inspector with protocol tests; real-host validation remains pending.


## Phased-pass verification

The current pass adds the Cowork findings, matching Python/TypeScript clients,
compact discovery, bounded reads/provisioning, sandbox lifecycle and an MCP App.
See the [findings register](../plans/bog-agent-improvement-register.md) for measured
request/byte counts and individual release gates.

Both client canaries run against real local manager/worker processes with a mocked
identity provider: sandbox readiness, private handoff installation, bounded and
ranked reads, selected fields, search, change waiting, read-only enforcement,
routes, diagnostics and own-sandbox deletion. This is a real service integration
test, but not real GitHub approval or an external MCP-host test.

The TypeScript notebook was also checked in an isolated in-app browser tab: saved
a new note, reloaded and observed it, searched for “quiet woodland retreat” with
the cabin first, then switched to Words and observed zero matches. Its rendered
layout was inspected. Private file paths and unexpected Host/Origin were blocked.
The temporary tab, processes and data were removed afterwards.

Real-host OAuth/Apps validation and package publication remain pending; no
production endpoint or pre-existing user Bog was modified.


Final local verification covers the affected definition, runtime, records-worker,
cloud and MCP suites. The helper/Python suite passed **38 tests**; client,
inspector and browser JavaScript suites passed **31 tests**. Both built packages
installed in isolated environments; the TypeScript consumer type check and shared
command-manifest drift check passed. Formatting passed for the five changed Rust
packages. Unrelated pre-existing formatting differences in `cli` and the search
example were left intact.

Two existing survivor-process tests failed during an overlapping integrated
build/test run. With the worker build held stable, all eight resident lifecycle
tests passed; no reproducible defect was found. Other failures during development
were corrected (private dotenv decoding, explicit throttling, API error-contract
expectations and preserved creation-repair guidance) and the affected checks
passed on repeat. This record distinguishes the isolated repeat from a claim that
the original full-suite invocation passed uninterrupted.

The final measured default quickstart used **4 requests / 5,982 response bytes**.
No HTTP readiness polling was needed with bounded creation waiting. Authentication
was preinstalled, human waiting was not measured, and startup health checks were
excluded. The broader independent trial and its limitations are linked from the
findings register. Full response-byte/request ledgers are retained alongside this
file; no credential values are included.
