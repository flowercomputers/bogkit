# Upgrade the existing remote chat

The chat runs locally on another machine. It does not need to move to this checkout or to Fly.

Before rotating the current owner credential:

1. Issue a write app credential restricted to Bog `21214697-82be-44d6-9328-264cc344aedd`. Use the current authenticated token-issuance API or the new console after migration. Keep the returned secret in a private local file or password manager; never paste it into a chat.
2. On the machine running the chat, replace its server-side bearer credential with that app credential and restart the local app. The service URL and Bog ID stay the same. Do not put the credential in browser JavaScript.
3. Post a test message through the chat and reload it. Confirm the app still reads and writes. Verify the app credential cannot list all Bogs or issue credentials.
4. Only then rotate the exposed owner secret in Fly and the private operator environment. Retain private emergency administration; do not restore global owner access to the public team gateway.

The operator cannot confirm step 3 from this checkout. Credential replacement and chat continuity need to be checked on the machine that actually runs the app.

## Change waiting after the new API is deployed

The existing record writes can stay unchanged. Replace repeated whole-view polling with:

1. `GET /v1/bogs/{id}/views/docs?limit=100&offset=0`. Save its opaque `cursor` alongside the records.
2. `GET /v1/bogs/{id}/changes?cursor={url-encoded-cursor}&timeout=25` with the same bearer credential.
3. On `changed: true` or `reset: true`, fetch the view again and keep the cursor from that view response. On timeout, keep the returned cursor and wait again. Retry temporary capacity/server errors with backoff. Stop and request reconnection on denied authorization.

A reset means the worker restarted or the restored resource has a different identity. This is a signal to refetch, not a replayable event history. A view page is not an atomic snapshot across all pages; avoid depending on insertion order. Use explicit application timestamps or ordering fields when the chat needs chronological presentation.

The corresponding MCP operation is `wait_for_change`. Its cursor and limits match HTTP. New app credentials expire after 90 days; arrange replacement before expiry.
