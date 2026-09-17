# Bog Cloud repair release

Spec: user-approved plan in this task, 2026-09-17. Repairs ship before GitHub activation.

## Task 1: Service repair

Implement explicit legacy public operator compatibility scoped to the legacy workspace: workspace and credential discovery, revocation, confirmed deletion; clear /v1/me identity. Preserve private operator access and existing app/human/agent restrictions. Authorize deletion before parsing confirmation. Preserve configured legacy allowance (currently eight) independently of ordinary workspace allowance three, despite workspace assignment. No registry migration.

MCP: canonical timeout with timeout_seconds compatibility alias; reject both; required non-null name/idempotency_key schemas with aggregated missing errors; explicit workspace discovery/execution tests; server-generated request IDs in successes, operation errors and invalid-argument errors, correlated with sanitized logs.

Shared creation validation must explain missing, unknown and wrong-type fields and valid templates. Templates endpoint returns dedicated templates collection. Preserve status conventions and existing response fields.

Public guidance must fully gate unavailable signup/invitations/workspace sharing in legacy mode, explain privately supplied management versus app credentials, and accurately distinguish new expiring credentials from historical non-expiring credentials. No additional database features, infrastructure expansion or comprehensive OpenAPI rewrite.

## Task 2: Verification and release

Regression tests for legacy management and allowance; ordinary quotas; app isolation; agent ownership restrictions; MCP schemas/transport/request IDs/aliases/workspace selection; invalid creation; both documentation modes. Cloud/MCP and relevant persistence/deletion suites plus lint and rendered-page verification. Independently review changes and resolve substantive findings. Deploy, run safe live read/write/change-wait probe, then delete exactly 93629d91-ca1a-471b-ba5b-c29c649de310; preserve chat 21214697-82be-44d6-9328-264cc344aedd and other Bogs. Supply original tester a follow-up prompt. Keep schema-compatible rollback image.

## Task 3: Separately gated GitHub cutover

Remote chat must use scoped write credential and pass read/write/wait before owner rotation. Inventory owner consumers and revoke specifically identified unused credentials. WorkOS production entitlement/pricing/GitHub/Connect/device/revocation must be verified, immutable operator identity claimed privately. Real two-account, MCP, device, invitation/removal and app isolation proofs required. Organization off-host backup destination, daily encrypted backups/restore and measured host capacity remain gates. Disable public legacy owner only at verified cutover. No secrets in chat, logs or command arguments; no paid expansion.

## User amendment: direct GitHub authentication

WorkOS is no longer the chosen provider. Use GitHub only to establish identity; Bog owns sessions, workspace permissions and API token issuance. Do not activate WorkOS or another authentication SaaS. Finish and ship the repair release first. Replace the former WorkOS-specific cutover prerequisites with direct GitHub setup and verification; preserve remote-chat migration, isolation, backup and capacity gates. Agent credentials must not require secrets in chat.
