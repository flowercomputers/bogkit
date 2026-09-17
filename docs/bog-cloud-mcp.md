# Bog Cloud MCP

Bog Cloud exposes remote MCP at `https://flower-bog-cloud.fly.dev/mcp`, sharing the HTTP API's workspace permissions and records. Start at the deployed [/connect](https://flower-bog-cloud.fly.dev/connect) page for client instructions and current verification status.

## Identity and authorization

GitHub sign-in is handled directly by Bog; WorkOS is not required. OAuth authorization uses PKCE and an explicit browser approval. Both HTTPS Client ID Metadata Documents and dynamic client registration are supported. Unauthenticated MCP requests advertise `/.well-known/oauth-protected-resource/mcp`; the root metadata endpoint remains available for compatibility.

`bog:read` permits reading existing resources. `bog:write` includes reading, provisioning and issuing app credentials within current workspace membership. Neither scope grants membership administration or Bog deletion. Membership checks remain independent of OAuth scope. Connections expire after 30 days, can be revoked in the console, and require reauthorization after expiry; refresh tokens are not supported.

Personal workspace selection is the default. Specify `workspace_id` for shared workspaces. Single-Bog app credentials cannot provision Bogs or issue credentials. Never paste credentials into prompts or URLs.

## Transport and compatibility

The Rust SDK is pinned to rmcp 3.4.0. This release targets MCP 2025-11-25 over Streamable HTTP; a broad upgrade to 2026-07-28 is outside this release. Every request is authenticated independently and session IDs are not accepted. Present Origin headers must match the configured allowlist; non-browser clients need no Origin header.

Client metadata requests are HTTPS-only, bounded, and reject redirects and private-network destinations. Callback addresses must match registered values. Native loopback callbacks may vary only their port, retaining the literal host, path and query. Dynamic registration callbacks remain exact.

See [the verification report](verification/mcp-agent-journey-2026-09-17.md) for actual client versions, results and pending gates. Inspector or SDK harness results alone do not establish natural-language client compatibility.

## Operations

The current tool surface includes workspace and Bog discovery, creation, schema inspection, record reads/replacements/deletion, bounded views, atomic batches, change waiting, and scoped credential management. `tools/list` is the executable contract.

Creation requires `name` and `idempotency_key`; `template` defaults to `records-v1`. Retry with the same creation key and body. Reads default to 100 records, with a maximum page size of 1000 and offset of 10000. Change waiting uses `timeout` (0–25 seconds); `timeout_seconds` is a compatibility alias, and supplying both is invalid. Treat cursors as opaque and refetch on reset; there is no retained event replay.

Successful tool results preserve `status`, `data` and `request_id`. Operation failures contain an error and request ID. Invalid arguments are protocol errors with request-ID data. Missing or invalid credentials return HTTP 401. A valid OAuth connection needing write scope receives the standard HTTP insufficient-scope challenge; missing membership cannot be resolved by a scope upgrade.

`issue_token` returns a secret and is retained for compatibility. Avoid invoking it in an ordinary agent transcript. Private app-access handoff is planned in phase 2 and is not yet claimed as available.

## Verification commands

Build the worker before running tests that start real processes:

```sh
cargo build --locked -p bog-cloud-records --bin bog-records-worker
cargo test --locked -p bog-cloud -p bog-cloud-mcp
cargo clippy --locked -p bog-cloud -p bog-cloud-mcp --all-targets -- -D warnings
```

Requests and serialized MCP results are bounded to 1 MiB; operations have a 30-second request deadline. Large results return an actionable error rather than silently truncating records. Use smaller pages when needed.
