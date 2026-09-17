# Bog Cloud repair retest request

Please revisit https://flower-bog-cloud.fly.dev/ using the same environment as your previous chat test. Begin with the homepage and published discovery; do not assume GitHub signup is active. Report the deployed behavior rather than relying on this checklist as documentation.

Recheck your original failures:

1. With your existing management credential, inspect `/v1/me`, list workspaces, list chat-Bog token metadata, and select the legacy workspace explicitly. Verify the documentation accurately describes the allowance and available authentication path.
2. Create at most one clearly named disposable Bog using a stable idempotency key, retry the identical creation, issue a scoped credential, and verify its restrictions. Delete only the disposable Bog you created, using exact-ID confirmation. Do not delete the chat or existing test Bogs.
3. Try an invalid template, an unknown field and missing creation requirements. Do the responses tell you how to correct them?
4. Through a real MCP client, inspect required creation arguments, `workspace_id`, and `wait_for_change`. Test canonical `timeout`, the compatibility spelling `timeout_seconds`, and a request with both (which should fail clearly). Record whether successes and errors supply usable request IDs.
5. Confirm the existing chat still reads/writes and waits for changes. Report whether it is actually running with a scoped write credential or still using the owner credential; do not paste either secret.

Preserve the existing chat records. Avoid capacity/load tests on the shared live host. Revoke credentials you create solely for this retest and remove your disposable resources. If an operation fails, leave its exact resource/token ID for targeted cleanup, not its secret.

Please send a concise verdict followed by remaining findings with timestamp, interface, sanitized request, expected/actual behavior and request ID. Separate confirmed service bugs, documentation gaps, and client/tool limitations. Include what now works and whether a first-time agent can proceed using only the public instructions. GitHub signup activation is a separate release; don't count its clearly documented absence as a regression.
