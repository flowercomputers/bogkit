# Bog Cloud prototype retest

Please try https://flower-bog-cloud.fly.dev/ again, starting as a fresh agent with only that URL. GitHub sign-in and Bog-issued agent credentials are now deployed. This is a small prototype, not a production durability assessment.

1. Discover how to connect from the homepage and published instructions. Request an agent approval code, show the human the approval link and public code, and keep the private code and resulting token out of chat and logs. Ask the human to sign in with GitHub and approve. Do not use the old owner token for this onboarding test.
2. Discover your personal workspace and create one Bog with a stable idempotency key. Retry creation and confirm you get the same Bog. Write and read records, replace a record, and check Fold's total view.
3. Issue a single-Bog write credential and use it in the supplied chat app's private server configuration. Verify reads, writes and change waiting. Confirm this app credential cannot provision Bogs or issue credentials.
4. Connect a real bearer-capable MCP client using the Bog-issued agent credential. Exercise the same Bog through HTTP and MCP. Automatic MCP OAuth login is not implemented; the documented device approval provides the bearer credential.
5. Recheck actionable errors, required MCP creation arguments, explicit workspace selection, timeout/timeout_seconds compatibility and request IDs. Report any mismatch you actually observe.

Keep the existing chat Bog (21214697-82be-44d6-9328-264cc344aedd) and its records untouched during fresh onboarding. The remote old chat remains compatible with its existing credential; do not rotate it or change that installation as part of this test. The previously reported disposable Bog 93629d91-ca1a-471b-ba5b-c29c649de310 has already been removed.

Create at most one new demo Bog and avoid load tests on the shared host. Leave its ID in the report; deletion requires the human's console session. Revoke test app credentials you no longer need, but retain any credential actively used by the demo. Never include secrets in the report.

Report whether you could go from the homepage link to a working app, what required guessing, and remaining failures with timestamp, HTTP/MCP interface, sanitized request, expected/actual behavior and request ID. Separate service bugs from client limitations. Do not expand the exercise into production hardening or new database features.
