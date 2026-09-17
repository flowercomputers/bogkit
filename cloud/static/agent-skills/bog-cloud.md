---
name: bog-cloud
description: Create a small hosted JSON database and build server-side prototypes using Bog Cloud HTTP or MCP.
---

# Bog Cloud

Use Bog Cloud for chat prototypes, small shared JSON records, and personal tools. This is a prototype with bounded storage, not a production database guarantee.

1. Read `/auth.md` on this origin for the currently enabled authentication path. With GitHub login enabled, request Bog device approval, show only the approval link and user code to the person, and poll at the documented interval. Keep the private device code and returned token out of chat and logs.
2. Read `/v1/me` and `/v1/workspaces` using the approved bearer token. Omitting a workspace selects the personal workspace; specify `workspace_id` explicitly for a shared workspace.
3. Discover `/v1/templates`. Create with `POST /v1/bogs`, JSON `{"name":"my-app"}`, and a fresh `Idempotency-Key` header. Reuse that same key and body when retrying. The default template is `records-v1`; observe the returned allowance and capacity errors.
4. Write JSON with `PUT /v1/bogs/{id}/docs/{key}`. Read `GET /v1/bogs/{id}/views/docs?limit=100&offset=0`, paging until an empty page. View order is implementation-defined; sort by your application's keys when needed.
5. Preserve the opaque cursor returned by reads. Wait using `GET /v1/bogs/{id}/changes?cursor={cursor}&timeout=25`; refetch on `changed` or `reset`. A reset means rebuild local state. This is notification, not replayable history.
6. Give a server-side application a single-Bog read or write credential via `POST /v1/bogs/{id}/tokens`. Capture the HTTP response directly into a private file and install the secret without printing it. MCP token issuance returns secrets in tool results, which clients may retain in transcripts; structured output does not prevent this.

MCP clients connect to `/mcp` with the bearer token, then initialize and call `tools/list` for their permitted tools. OAuth-capable clients can use the self-hosted authorization server discovered through /.well-known/oauth-protected-resource. Request bog:read for reading only or bog:write for creation, writing and app credential management. The human explicitly approves through GitHub; never handle the resulting token in a chat transcript. Read `/docs` and `/openapi.json` for exact request and response formats. App credentials cannot provision Bogs or issue more credentials. Browser applications use a server proxy; the data API does not offer browser CORS.

Never delete existing data as a test. Use only user-authorized resources and clean up only resources created for that test.
