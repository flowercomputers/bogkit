# Agent improvement findings register

Source snapshot: two user-supplied September 18, 2026 reports: **Scatter agent-experience feedback** (source task `01a0b516-05d2-7573-8a9f-55fb74ff1b3c`) and **Cowork field report and hardening brief**. S1–S9 below correspond to Scatter's numbered consolidated feedback. F IDs preserve Cowork's original numbering; that report contains no F8.

This register separates current checkout implementation from demonstrated behavior and remaining external gates. It is not a production release claim. Historical report estimates are not a measured before/after benchmark of the current source. Existing records, permissions and important data must be preserved; no paid infrastructure expansion or human dashboard is part of this work.

## Scatter findings

| ID | Observed issue / ownership | Current implementation and evidence | Remaining work or boundary |
| --- | --- | --- | --- |
| S1 | Assistant ended its turn after device link; server already supported polling. Agent/host behavior. | Private helper performs polling, state handling and private storage; auth guidance requires continued polling. | Fresh actual-host trial with website approval and no chat acknowledgment remains an external gate. Denial/expiry/backoff fixtures cannot establish host resumption. |
| S2 | Large capability-rich inventory overwhelmed context. Server response plus agent output handling. | Compact inventory, explicit routes and schema surfaces in current checkout; deterministic journey records actual response bytes. | Compare under representative multi-Bog inventory and a fresh context. Keep full validation contracts explicitly obtainable. |
| S3 | Connection → workspace → project → scoped access was manually orchestrated. | Existing project/helper flow plus bounded wait during creation, discoverable app-access handoff and explicit workspace guidance. | Real app installation on a second host/actual agent tooling remains an external acceptance step. Shared workspace must stay explicit. |
| S4 | Additive search required several low-level operations. | Python client `add_search` preserves existing definition and checks build completion; exact search harness exercises both additions to populated records. | No invented server-side one-call semantic API. Revision-conflict handling and failures remain visible; accepted job is not activation. |
| S5 | Custom private credential loading and local bridge code. | Python and TypeScript SDKs/CLIs, private helper and equivalent loopback notebook starters exist, with private-file and origin/host checks. | Local package builds and both real-server journeys pass; package-registry publication remains an explicit external release. The local starter is not multi-user app authentication. |
| S6 | Search demo initially made an unchecked lexical claim; freshness hard to see. | Indexed fields/counts and synchronous maintenance diagnostics; exact Cowork/Scatter reproduction and update/delete/reopen/full manager-restart checks passed locally. | Encoder relevance limitation persists. Single-record cabin first place does not prove useful discrimination. No evidence-based minimum corpus threshold exists. |
| S7 | App-access OpenAPI success was described as revocation. | Dedicated handoff response contract and response-contract checks exist. | Revalidate externally on the candidate release; historical mismatch alone does not establish current live drift. |
| S8 | Confused responsibilities of MCP, WebMCP, skills and host. | Guides separate client authorization, private helper and advisory discovery. | Client-specific OAuth/continuation and signed-in WebMCP need actual clients. Skill hosting cannot ensure installation; server prompts cannot wake an ended task. |
| S9 | Fetch restrictions, preview syntax, shared-tab testing, screenshots and unsupported claims. Agent/host/tool execution. | Classified as operational lessons, not server defects. Disposable local harnesses isolate fixtures and redact credentials. | Fresh visual/browser work must use an isolated tab, preserve user content and verify actual rendering. No server feature can certify these behaviors. |

## Cowork findings

| ID | Finding | Current implementation / bounded decision | Remaining validation or deferred scope |
| --- | --- | --- | --- |
| F1 | Nested device-token errors broke standard polling. | Device-token endpoint now uses flat OAuth error strings with rich detail separately; compatibility readers accept both shapes. | Actual unmodified RFC client and website-only approval test remains external. Native and operator-only deployment modes must not advertise unavailable auth. |
| F2 | Routes buried in schemas; guessed docs list produced misleading 404. | Compact routes in creation/detail and explicit routes/schema access; docs 404 offers authorized applicable list suggestion. | Deterministic harness replays returned route examples and repairs default docs 404. General edit-distance route guessing is not promised. |
| F3 | No key-set/range reads; ranked results lacked bodies. | Bounded batch_get, exclusive after/before bounds, opt-in ranked include_fields. | Values-by-default intentionally not changed; bounded projection preserves response size and compatibility. Snapshot cold-start API deferred pending measured need and contract design. |
| F4 | Poor semantic relevance on exact three-message example. | Reproduced actual pinned model scores and verified extraction against direct encoder cosine. Added measured record and freshness checks. | Relevance remains limited; no unsupported corpus-size threshold/hint or model-quality fix claimed. |
| F5 | Private credential handoff was not discoverable and host blocked .env. | App-access preparation discoverable; private helper writes locally using explicit private output, existing same-account authorization and one-use handoff. | npx/uvx publication deferred. Server cannot write an arbitrary client filesystem. Host .env bridge restrictions need actual-host test; never work around by displaying credentials. |
| F6 | HTML-only docs imposed parsing overhead. | Markdown agent/docs surfaces and root content negotiation in checkout. | Auth/console HTML does not become a token transport or credential-bearing markdown mirror. |
| F7 | No single sufficient entry document. | Bounded `/agent.md`, `/llms.txt`, deployment-aware authentication and versioned notes recipe. | Current guide links recipe/schema/auth detail, so strict one-document/four-request/six-KB target must be measured rather than declared met. Fresh context trial pending. |
| F8 | No finding with this ID in supplied report. | Reserved / absent. | Do not fabricate a missing finding. |
| F9 | Creation required repeated polling and access setup. | `wait:true` bounded at 25 seconds, inline routes, optional nonsecret app_access preparation; granular endpoints retained. | A timed-out wait remains accepted/creating. Raw secrets are not returned by the combined flow. dry_run/credentials/deliver proposal is not the implemented shape. |
| F10 | Effective creation allowances absent from context. | `/v1/me` enriched with effective workspace allowance, usage and credential scope; uncapped status separated from storage/capacity. | Normal-account and shared-workspace checks are needed; an operator harness cannot prove human quota behavior. |
| F11 | Successful responses lacked next steps. | Creation includes structured next and executable compact routes. | Universal next arrays on every mutation are not implemented; do not claim full proposal. |
| F12 | Errors lacked repair fields and server-side lookup. | Content-free request lookup with one-hour bounded retention, owner authorization and app-credential isolation; route suggestions for supported case. | Rich fix/retry for every validation error is deferred. Request observations reset on restart and may be evicted; no bodies/secrets retained. |
| F13 | Exploratory Bogs cluttered workspace. | Explicit sandbox flag/expiry and bounded preview/confirmation cleanup flow in checkout. | Confirm deletion is authorized; no automatic cleanup of report's historical real Bogs. Real elapsed expiry and quota/restart tests are separate from journey measurement. |
| F14 | Wanted full generated CLI and no-install distribution. | Both `bog-cloud` CLIs cover connection, private installation, creation, reads/search/waits, additive search, diagnostics/explain and sandbox/owner cleanup. Shared contract fixtures and generated command manifest checks pass. | Local Python wheel and npm tarball installation verified; public npx/uvx distribution awaits authorized package publication. Orchestration remains explicit, rather than automatically exposing every internal operation. |
| F15 | REST/MCP/CLI/docs vocabulary could drift. | Shared definition compilation and hosted schemas/routes reduce drift for Bog resource operations; parity/contract tests cover supported transports. | CLI command metadata is generated from the server operation table; explicit transport adapters retain their appropriate names and envelopes. Documentation must not falsely state universal equivalence. |

## Measurement protocol and finishing criteria

Run `python3 scripts/cloud/verify_agent_journey.py` after building `bog-cloud-server` and `bog-records-worker`. It creates a private disposable `/tmp` root, uses a random available localhost port and an in-memory private owner credential, starts one server, then measures each HTTP response's byte count, elapsed milliseconds and status. It never prints response bodies or credentials. Startup health checks are excluded and identified; no server restart is used by this journey harness.

The flow measures root Markdown, llms index and agent guide separately, default create-with-wait → write → read, then the downloaded notes recipe, creation-route replay, ranked projection, top-level wait envelope, batch_get, key bounds, expected 404 suggestions and redacted request lookup. Expected negative probes are counted separately from discovery wrong turns. Standard-library assertions check response structure and behavior; they are not full JSON Schema validation or a fresh agent comprehension trial.

Finishing criteria for this local measurement: all assertions pass; totals are reported without invented token counts; expected failures remain distinguishable from unexpected errors; private temporary server/data are removed. The comparison baseline remains the report's approximate 12 requests/240 KB/two wrong turns, not independently remeasured pre-change source. Authentication approval/host continuation is outside the deterministic operator run and must be tested separately before declaring the real agent journey accepted.

## Measured local run, September 18

The deterministic harness passed all assertions, including the notes recipe keyword search. No production or external agent-host interaction occurred.

| Measured scope | Requests | Response bytes | Notes |
| --- | --- | --- | --- |
| Root + llms + guide + default create/write/read | 6 | 10,175 | 1,103.073 ms summed request elapsed time |
| One-guide/default subset of the same run | 4 | 5,982 | Excludes authentication, root/index and defined recipe; not an independent trial |
| All probes including recipe and negative cases | 21 | 18,728 | 3 intentional 4xx; zero unexpected errors |

The one-guide/default subset meets the four-request/six-KB target (5,982 UTF-8 response bytes). No before/after speedup, token count or fresh-agent acceptance is inferred. Startup checks were excluded; server restart count was zero. UUIDs and timing vary per run.


## Phase delivery and release gates

All implementation below is local to this branch. No production release, package
publication or external MCP-host acceptance is implied. Existing granular APIs
remain available and new convenience/read features are opt-in.

| Phase | Local delivery | Acceptance / remaining gate |
| --- | --- | --- |
| 0 | This findings map; prior baseline retained separately; disposable measured journey; separate implementation commits. | Historical reports are the only pre-change comparison. Authentication and human waiting are not included in the operator benchmark. |
| 1 | Shared compact routes/notes recipe; mode-aware agent/docs/llms content; HTML/Markdown negotiation, link headers, skill/manifests. | Public HTTP and executable notes checks pass. llms <2 KB, agent guide <6 KB (stricter 3.3 KB test). |
| 2 | Flat device-token errors with compatible clients; continued polling, private JSON/dotenv, cached-auth refresh and verified connection. | Helper failure fixtures and real local handoff redemption pass. Actual website-only approval/host continuation remains external. |
| 3 | Routes and next actions in creation; bounded 25-second wait with concurrent limit; optional existing handoff; effective context and safe docs-route correction. | Measured default quickstart meets request/byte target. No new unrestricted credentials are returned. |
| 4 | Projected ranked results; 100-key ordered batch reads; exclusive key bounds; cursor-before-view chat example. | Runtime projection/ordering/mutation tests and both real clients pass. Whole-Bog snapshot is explicitly deferred. |
| 5 | Exact Cowork/Scatter reproduction, freshness/restart checks, searchable counts, repair/retry details and one-hour redacted request lookup. | Weak semantic ranking reproduced and explained by pinned encoder scores; no universal relevance threshold claimed. Permission tests cover own-app request lookup. |
| 6 | Python wheel + TypeScript/npm package, both `bog-cloud` CLIs, shared fixtures and generated manifest, equivalent notebook/chat starters. | Local installation/type checks and real-server client journeys pass. Registry credentials/publication remain external. |
| 7 | One one-hour sandbox per workspace, separate allowance, persistent expiry, same-credential deletion, frozen-ID owner cleanup. | Controlled expiry/concurrency/restart/revocation tests pass; feature defaults off pending explicit rollout. Rollback restrictions documented. |
| 8 | Read-only MCP inspector with ordinary structured fallback; safe browser connection status. | Local protocol, hostile-content and permission tests pass. Real OAuth-capable MCP host and Apps rendering still require an available compatible host. |

Release each group only after its relevant external gates. After an authorized
rollout, independently check running version, public discovery and a disposable
write/read/search journey; do not infer deployment from source, commits or tests.


## Independent local trial

A fresh agent received only the local root URL and a private preinstalled
authorization-file path, with no repository access or supplied API routes. It
used public HTTP discovery to create a notes definition, write/read three notes,
run lexical and semantic searches and fetch recent notes. All **12 requests**
succeeded, totaling **27,704 response bytes** and **1.796 seconds** of request
time, with zero readiness polls or wrong-turn HTTP requests. The initial sandbox
network denial occurred before reaching the service and is separately recorded.
This broader custom-definition journey is not the four-request default-template
benchmark. No human approval or package installation was part of this trial.

The agent flagged advertised production origins in a local deployment, personal
versus legacy workspace wording, and unclear literal JSON Pointer result keys.
The first two are fixed through configured-origin discovery and mode-aware text;
the short guide now explicitly explains pointer-keyed values. Regression checks
cover the new origin and wording behavior. See the [sanitized request ledger](../verification/independent-agent-trial-2026-09-18.json).

This is one independent local trial of the integrated candidate, not three
separately released phase trials or an external MCP Apps-host test. Repeat the
root-only trial on each authorized deployed candidate after phases 3, 6 and 8.
