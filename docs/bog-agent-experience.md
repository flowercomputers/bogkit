# Agent connection and project workflow

This guide describes the staged source changes following the Scatter and Cowork trials. Availability on a deployed service depends on its running build; this document is not a deployment record.

## Connect once, then continue

Download the service's `/bog-app-access.py`, or use `scripts/cloud/bog_app_access.py` in this checkout:

```sh
python3 scripts/cloud/bog_app_access.py --connect --auth-file /private/path/agent.json
```

The directory must be private. Show the public approval URL, keep the process running, and await its completion. Approval on Bog is authoritative; a chat acknowledgment is neither needed nor sufficient. Pending approval keeps polling; throttling increases the delay; denial, expiration, or cancellation stops the operation. The helper stores authorization in an owned mode-600 file, checks `/v1/me`, and only then reports Connected. Reuse that explicitly selected authorization file to install scoped app access. Never display its contents.

An agent host must support waiting for a running process or resuming it. Bog cannot wake a conversation that the host has ended. If the process is interrupted before collecting authorization, start a fresh approval. If verification fails after collection, rerun `--connect` with the same file to retry verification. App credentials remain separate from management credentials.

## Compact discovery

`list_bogs` / `GET /v1/bogs` now returns inventory entries with `capability_summary` (resource names, kinds, actions), definition identity, `writes_paused`, and `resources_url`. It does not duplicate nested schemas or build-job payloads. Fetch a selected Bog's detail or `/resources` for full operation schemas. Existing clients that read `capabilities` from inventory should move to `/resources`; the detailed API remains available.

## Build a project

The [Python and TypeScript clients](../clients/README.md) and their [Python](../starters/cloud-notebook/README.md) / [TypeScript](../starters/cloud-notebook-ts/README.md) notebook starters cover project creation/readiness, private app installation, record operations, search, and diagnostics. They use the existing HTTP permission checks. Shared workspaces require explicit selection.

`Client.add_search` reads the current definition, appends a named BM25 or semantic resource, preserves every existing resource/operation, plans against the revision, applies, and waits for the job to succeed. It reports conflicts and failed/recovery-required jobs instead of overwriting or claiming an accepted build is active. Low-level definition APIs remain the authoritative contract. This convenience operation currently lives in the client, not a new remote MCP tool.

Usage responses for configurable Bogs include `search_resources`, `definition_revision`, and `boot_id`. Each exposed search resource reports fields and searchable source-record count. Counts are computed on demand under the runtime lock, respecting filters/projections and present text fields; they are not an independent scan for corrupted index entries. Index maintenance is synchronous with source writes. Compare response sequences only within a worker boot; use opaque change cursors across restarts.

## MCP Apps: useful after connection

[MCP Apps](https://modelcontextprotocol.io/extensions/apps/overview) associate tools with sandboxed HTML resources rendered inside supporting hosts. Bog now includes a read-only resource inspector, search comparison and diagnostic view using this extension. The host mediates tool calls and controls available capabilities; support varies by client. Ordinary structured tool results must remain a fallback.

Initial access to the MCP server remains the host's [MCP authorization](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization) responsibility. An App UI is not a replacement for OAuth or the private device helper, and should not receive credentials. A status panel could help an already-connected user understand scoped app setup and open the existing explicit approval/download page. It cannot independently resume a finished agent task or install a secret into the user's filesystem.

The [implemented inspector](../cloud-mcp/INSPECTOR.md) uses existing authenticated read tools, with no credential delivery and no new authority. Protocol and permission tests pass locally; rendering in a real supporting host remains a release gate. Non-supporting clients continue receiving structured results.


## Discovery and efficient reads

Start at `/llms.txt` for the small document directory or `/agent.md` for the
practical quickstart. The root and `/docs` also negotiate Markdown. Public guides
contain no account details. Use authenticated `/v1/workspaces` and `/v1/me` for
workspace allowance and credential scope; host capacity may still prevent creation.

Creation can opt into `wait:true` (at most 25 seconds) and `app_access` preparation.
The response includes usable routes and schema/status links. A handoff is a
nonsecret reference, not an issued app token. Unready creations remain accepted;
retain the ID and original idempotency key instead of creating another Bog.

Ranked queries support opt-in `include_fields`. Exposed table reads support up to
100 ordered keys with explicit null missing values, and exclusive key bounds.
See the [chat example](../starters/cloud-chat/README.md) for cursor-before-read
synchronization with bounded recent-message refreshes. Change waiting signals
invalidation; it does not replay a durable message history.

## Temporary resources and diagnostics

When explicitly enabled, a workspace can have one one-hour sandbox outside its
ordinary Bog count, within host/storage limits. Agents can delete only sandboxes
created by that same credential. Ordinary Bogs never expire implicitly. Owner
bulk cleanup freezes an explicit ID set in a preview and rechecks permission at
execution. See the [operations guide](bog-cloud-operations.md) for rollout and
rollback restrictions.

Authenticated request lookup retains bounded, redacted observations for up to an
hour. App credentials see only their own activity. Missing entries can be expired,
evicted or lost on restart; they are never evidence that a write succeeded or failed.
Search diagnostics report source records containing searchable text, not an
independent index-integrity audit. The [reproduction report](verification/search-reproduction-2026-09-18.md)
records the actual small-corpus semantic limitations and lexical fallback guidance.

Implementation, measured journeys and outstanding external gates are tracked in
[the findings register](plans/bog-agent-improvement-register.md). Packages are built
locally; no registry publication or production rollout is implied.
