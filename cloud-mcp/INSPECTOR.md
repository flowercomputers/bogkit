# Read-only inspector

The resource at `ui://bog-cloud/resource-inspector` is an MCP App for resource
inspection, side-by-side searches, and definition-update status. It is linked from
`list_resources`, `query_resource`, `search_resource`, and
`definition_update_status`. It is a supporting read interface, not a management
dashboard.

The implementation follows the [MCP Apps specification](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx):
`text/html;profile=mcp-app`, `_meta.ui.resourceUri`, extension identifier
`io.modelcontextprotocol/ui`, and the `ui/initialize` handshake. The server's
stateless transport does not retain client capabilities. It publishes static UI
links; the host decides whether to render them. Every tool still returns its full
structured result and text representation. The App enables calls only after the
host negotiates version `2026-01-26` and advertises `serverTools`. Without that
capability it can display the initial result, and ordinary tools remain usable.
UI-only resources are discovered through tool metadata, as the specification
permits, rather than added to the existing guide resource listing.

The App uses only four fixed read tools through the current host connection.
Other tools have model-only visibility. Each read still enters the existing
workspace, Bog, credential, and management checks. Definition-job access still
requires management authority. No credentials, cookies, extra permissions,
external resources, or network destinations are requested. Data and errors are
rendered as text; foreign-window messages are ignored. Shared workspace selection
is always explicit.

`bog_connection_status` is a separate WebMCP browser tool on `/console`. It reads
fresh session state and returns only signed-in status and public paths. Unknown
network/server failures return unknown status, rather than a false logout.

## Verification

- `node --test cloud/tests/js/inspector.test.cjs cloud/tests/js/webmcp.test.cjs`
  covers handshake capability fallback, read calls, two-resource comparison,
  explicit workspace selection, permission errors, hostile text, foreign frames,
  blocked writes, and safe session status.
- `cargo test -p bog-cloud-mcp --test inspector --test agent_workflows`
  exercises the real authenticated MCP transport and unchanged permissions.
- A real compatible MCP Apps host remains an external acceptance check. The
  JavaScript host harness is a protocol-shaped simulation, not proof of rendering
  in any particular host. Before claiming host compatibility, connect an actual
  Apps-capable host, open the inspector from a listed read tool, test one resource
  query, compare two searches, read a definition job, and repeat with insufficient
  permissions. Also verify the same tools in a host without Apps support.
