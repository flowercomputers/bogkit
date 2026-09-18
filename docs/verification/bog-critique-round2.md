# Critique round two follow-up — 2026-09-18

Source: the user's pasted independent retest of runtime 012384a; linked report https://claude.ai/artifact/WVucHHqpKzGCU3LvvKL6aD.

Release source: `9bacd28`.

## Fixed

- The private installer distinguishes omitted `--auth-file` from explicit cache selection. No flag always requires a new device approval, even when its standard cache exists. An explicitly selected, owned, private cache still permits reuse. The default-cache regression failed before the fix and passed afterward.
- App-token management authority is checked before JSON parsing for provisioning, token issuance, private handoff preparation, definition validation, planning and application. Equivalent MCP malformed-argument calls are also denied first. The HTTP regression reproduced 400 instead of 403 before the fix.
- HTTP and MCP share the same recovery-hint function. HTTP resource-operation 404s now include `error.next_action` and their request identifier.
- Hosted metadata labels the abstract response schema as internal runtime data. Hosted clients should use `hosted.request_schema` and `hosted.response_schema`. The internal mutation acknowledgement is not advertised as an HTTP response.
- Guidance explicitly documents supported deprecated wait timeout aliases and their mutual exclusion.

## Preserved boundaries

- `describe_bog` retains its full capabilities payload to avoid a response-shape change in this focused repair. A compact description remains a potential follow-up.
- Existing timeout aliases remain accepted rather than breaking older clients.
- No records, credentials or test resources from either external critique run were removed. The installer connection name alone does not safely identify a single grant to revoke.
- No fresh human device approval or independent Claude run is claimed by these checks.

## Verification

- 168 cloud/MCP tests passed, including actual transport, permission, persistence and contract tests; documentation tests completed.
- 15 private-helper tests passed, including omitted-cache versus explicit-cache selection and piped approval visibility.
- Deployed `registry.fly.io/flower-bog-cloud:critique-round2-9bacd28` to the existing machine without capacity changes.
- Live scoped app calls to app-access, definition plan/apply and token issuance with malformed or empty bodies returned 403. Representative request IDs: `089a3b98-5c55-4337-9e65-50e844257958`, `11703bcf-c31c-42d5-a7c3-22567333b9c0`, `2d45f0f6-bc4f-4618-bf4a-085736bfe06a`.
- Live unavailable-resource-operation 404 included the recovery hint (`144a29d1-703a-472e-a8ff-a946250cbb3c`). MCP resource metadata included the schema-scope clarification.
- Downloaded helper bytes exactly matched the locally tested source. No additional human approval was requested for this verification.
- Disposable Bog `59996c61-16df-4418-a6c4-ffe84ce8aa5c` and its test credential were cleaned up.
- Existing six legacy Bogs remained present. The chat was readable with an unchanged record fingerprint; request `97764a9e-9eb2-4f57-9ee7-b1910e8e49f3`.
