# Native authentication critique followup

Completed the narrow wording and discovery changes. Device errors now distinguish waiting for human approval, an expired/unavailable request, denial, and request throttling, with next steps. Name validation says “control characters.” Native `/llms.txt` includes the complete shared contract operation list and native GitHub/device/bearer guidance plus when to use HTTP or MCP. `/auth.md` explicitly warns that MCP results, including structuredContent, can enter model context, transcripts, and logs; it recommends HTTP credential issuance written directly into an owner-only private file without printing.

No authentication behavior, APIs, legacy compatibility, OAuth support, or guide styling changed.

Verification:
- `cargo test -p bog-cloud --lib native_auth::tests`: 4 passed.
- `cargo test -p bog-cloud --test native_auth`: 6 passed. Loopback mock-server tests required an escalated rerun after the sandbox denied socket binding.
- `cargo clippy -p bog-cloud --all-targets --no-deps -- -D warnings`: passed.
- `rustfmt --edition 2024` limited to owned Rust files; `git diff --check` passed.
- Added focused assertions for device guidance, control-character wording, complete native contract discovery, and credential handling guidance.
