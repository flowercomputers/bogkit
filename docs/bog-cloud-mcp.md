> **Legacy deployment reference.** The remainder of this document describes the earlier single-owner release. The team implementation on this branch adds WorkOS authorization, workspaces, scoped expiring app credentials, deletion, idle workers and change waiting. It has not passed production activation yet.
>
> For the new release, use [production configuration and gates](bog-cloud-production-gates.md), [backup operations](bog-cloud-backups.md), and [remote chat migration](bog-chat-upgrade.md). The running new server publishes its current contract at `/v1`, `/auth.md` and `/openapi.json`. Startup without WorkOS now requires an explicit legacy migration opt-in.

# Private Bog Cloud MCP

The combined `bog-cloud-server` binary exposes `/v1` REST and `/mcp` Streamable HTTP over the same authorized CloudService and persistent records. This is a private single-owner service, with scoped credentials for individual databases.

## Tested transport and client

The official [Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk), `rmcp = 3.4.0`, is pinned in the manifest and lockfile. Its [Streamable HTTP server API](https://docs.rs/rmcp/3.4.0/rmcp/transport/streamable_http_server/index.html) supplies JSON-RPC initialization, notifications, discovery, and protocol errors. The included SDK client negotiates **2025-11-25**; this exact version is asserted by integration tests. The SDK also implements newer protocol features, which are not claimed as tested here.

Tested client: `cloud-mcp/examples/mcp_acceptance.rs` with rmcp 3.4.0 `StreamableHttpClientTransport::with_client`, a TLS-enabled reqwest client, and `StreamableHttpClientTransportConfig::auth_header(token)`. The argument is the raw token; the SDK supplies the Bearer prefix. The SDK's `ServiceExt::serve` performs initialization and the initialized notification.

This SDK harness is protocol evidence. Connecting a selected natural-language MCP client and performing the workflow through that client remains a separate gate. No user's client settings have been edited; no untested client compatibility or one-click OAuth installation is claimed. M2 remains pending until an actual client workflow on a second machine is recorded.

## Run the combined gateway

Build the trusted worker and combined server:

```sh
cargo build --locked -p bog-cloud-records --bin bog-records-worker
cargo build --locked -p bog-cloud-mcp --bin bog-cloud-server
```

Provide configuration through the operator's protected environment mechanism:

| Variable | Purpose |
| --- | --- |
| `BOG_CLOUD_ROOT` | Absolute persistent root; keep it short enough for Unix sockets. |
| `BOG_WORKER_BINARY` | Absolute path to the trusted `bog-records-worker` executable. |
| `BOG_CLOUD_OWNER_TOKEN` | Secret owner credential, 32–4096 bytes. |
| `BOG_CLOUD_BIND` | Defaults to `127.0.0.1:8080`. |
| `BOG_CLOUD_ALLOWED_ORIGINS` | Optional comma-separated exact Origin values, such as `https://client.example`. |
| `BOG_CLOUD_ALLOWED_HOSTS` | Optional comma-separated gateway hostnames or host:port authorities for MCP; the SDK defaults to loopback hosts. Set the actual HTTPS hostname when using a reverse proxy. |

Then run `target/debug/bog-cloud-server`. Remote use requires operator-configured HTTPS/private access. Startup reconciles desired running instances. SIGTERM/SIGINT drains gateway requests and stops workers while preserving their desired state for the next restart.

Every MCP request, including initialization, discovery, notifications, GET, and DELETE, must contain `Authorization: Bearer <token>`. The bearer is authenticated from that request, and tool operations recheck the live registry for revocation. Tokens currently have no time-based expiry; revoke them explicitly. Revoking a scoped token does not rotate the owner token.

Transport is stateless even for legacy clients. The SDK `NeverSessionManager` stores no session identity, and the endpoint rejects any `Mcp-Session-Id` header. There is no session credential or mixed-identity session to reuse. The handler obtains its principal from the current request's HTTP extensions, never tool arguments. A client object kept open across revocation loses access on its next request.

Without an Origin header, non-browser clients can connect normally. A present Origin must exactly match the operator allowlist; an empty allowlist rejects every present Origin, including `null`. Duplicate authorization or Origin headers are rejected. Host validation retains the SDK's DNS rebinding protection. Authorization never relies on these browser-facing checks.

## Tools and results

Exactly eight tools are advertised:

| Tool | Arguments / behavior |
| --- | --- |
| `create_bog` | `name`, `template: "records-v1"`, required `idempotency_key`; reuse the identical body/key for a retry. |
| `list_bogs` | No arguments; owner-only registry listing. |
| `describe_bog` | `bog_id`; status plus actual template schema when ready. |
| `get_record` | `bog_id`, `key`; read one JSON object. |
| `upsert_record` | `bog_id`, `key`, object `data`; complete replacement. |
| `delete_record` | `bog_id`, `key`. |
| `read_view` | `bog_id`, `view: "docs" | "total"`, optional `limit`, `offset`. |
| `batch` | `bog_id`, `operations` array of `{op:"upsert",key,data}` or `{op:"remove",key}`; atomic within one Bog. |

Inputs use generated JSON schemas and reject unknown fields. Read tools carry read-only annotations; replacements, deletions, and batches carry destructive annotations. Only creation with its required idempotency key advertises mutation idempotency. Annotations do not grant authority.

Successful structured results contain `{status, data}`. A valid tool invocation whose database operation fails returns `isError: true` and `{error:{code,message}}`. Unknown tools and malformed arguments are protocol errors. Missing/invalid/revoked credentials return HTTP 401 before tool execution. Insufficient scope produces a structured tool error; another database appears not found to a scoped token.

Requests are limited to 1 MiB and 30 seconds. Whole serialized MCP tool results (including the SDK's text mirror of structured content) are limited to 1 MiB. Large results return `result_too_large`, never a silently truncated page; reduce the page limit. Shared REST/worker limits also apply, including 100 batch operations and bounded record/view sizes. Tools expose neither token issuance nor filesystem/exec/fetch/erase/watch/search capabilities. Token management remains owner-only REST.

## Reproducible acceptance

```sh
cargo test --locked -p bog-cloud-mcp --test protocol --test authorization
```

Tests start real local listeners and worker processes in disposable roots. They verify SDK initialization and exact discovery, legacy wire initialization/notification/discovery, creation retries/conflicts, nested JSON CRUD, actual schema, total updates, atomic batch rejection, independent REST reads, unknown tools/arguments, request/result bounds, per-Bog read scope, cross-Bog denial, revocation of a connected SDK client, exact Origin rules, Host validation, and rejection of session IDs.

For the runnable acceptance client, set `BOG_CLOUD_URL` to the gateway origin and `BOG_CLOUD_TOKEN` to an owner credential through a protected environment source. Never paste a secret into committed configuration or command arguments:

```sh
cargo run --locked -p bog-cloud-mcp --example mcp_acceptance
```

The example requires HTTPS except for loopback HTTP, creates a disposable Bog, obtains a temporary scoped token through REST, performs nested JSON write/read/replacement/count/batch/delete through MCP, independently checks records/counts through REST, then revokes the scoped token and checks that the existing client loses access. It prints a redacted PASS line with the database ID and negotiated version. It leaves the empty disposable database for inspection. A failed run may leave its fixture and temporary token; inspect the fixture and revoke that token through the operator's normal REST process. The owner credential is preserved.

Run this example on a real second machine for remote protocol evidence. Separately record the selected agent client's version, its reviewed bounded settings change, redacted tool results, REST cross-checks, and revocation behavior before marking the real-client milestone complete.

## Tested Codex CLI workflow

Codex CLI 0.145.0 with GPT-5.5 has completed the natural-language creation/CRUD/batch/count workflow against both the local service and the deployed Fly HTTPS endpoint, independently checked through REST. See the acceptance report for deployment-specific evidence.

For a saved direct HTTP connection, the bounded configuration is:

```toml
[mcp_servers.bog_cloud]
url = "https://flower-bog-cloud.fly.dev/mcp"
bearer_token_env_var = "BOG_CLOUD_TOKEN"
```

Supply that environment variable from your protected secret source before launching the client. This snippet has not been added to your saved settings automatically. Leave mutation confirmation at your preferred default for everyday use. Static bearer configuration follows the [official Codex configuration reference](https://developers.openai.com/codex/config-reference); this service does not offer OAuth login.

To repeat the isolated conversational acceptance run with an owner credential in the environment:

```sh
BOG_CODEX_MODEL=gpt-5.5 python3 scripts/cloud/codex_acceptance.py
```

It creates one disposable Bog, grants only the run's four fixture mutation tools process-local approval, leaves its final two records for independent verification, and changes no saved client settings. Do not run repeatedly against a nearly-full eight-Bog service; each successful run consumes one instance slot.
