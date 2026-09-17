# Team preview activation gates

This release must not replace the live personal service until these checks pass. Local mock-provider tests are not evidence of production GitHub/MCP/device interoperability.

## Authentication configuration

Use an organization-controlled WorkOS production environment. Enable GitHub only; request no repository access. Configure a confidential Connect application for the browser callback, a public Connect application for device login, client metadata documents and dynamic registration for MCP clients. Register the exact public MCP URL as a resource indicator and validate that exact audience. Do not fall back to accepting the environment client ID for production resource tokens. The browser client ID and API resource audience serve different purposes.

Required runtime configuration: BOG_WORKOS_ISSUER, BOG_WORKOS_RESOURCE, BOG_WORKOS_AUDIENCE, BOG_WORKOS_CLIENT_ID, BOG_WORKOS_CLIENT_SECRET, BOG_WORKOS_REDIRECT_URI, BOG_WORKOS_DEVICE_CLIENT_ID. Keep secrets in Fly secrets; do not put them in source, shell history, transcripts, or page URLs. Pin the verified operator issuer and immutable provider subject before claiming the legacy workspace. Email or GitHub username is not proof of that identity.

Verify provider revocation works for agent connections. If provider discovery does not expose supported revocation, resolve the provider-supported alternative before activation. Local sign-out alone does not prove remote agent credentials were revoked.

## Pricing check (2026-09-17)

The public [WorkOS pricing page](https://workos.com/pricing) lists AuthKit free up to one million monthly active users and a custom domain at $99/month. It says production may require a credit card. Connect-specific production entitlement and applicable charges still require confirmation in the organization's dashboard. Use the provider domain initially; no custom-domain purchase is authorized by this plan. Do not equate the AuthKit free tier with confirmed zero cost for every enabled feature.

Sources: [MCP setup and resource indicators](https://workos.com/docs/authkit/mcp), [Connect device flow](https://workos.com/docs/authkit/cli-auth).

## Data and operational cutover

- Snapshot existing registry and stores while safely quiesced; rehearse migration and restore on a copy. Record registry version and compatible binary. Never start the old binary against version 2.
- Locate the existing chat deployment, replace its global-owner credential with a scoped credential, and verify read/write before rotating the exposed owner secret. Disable public global-owner access; retain private admin access.
- Configure a private organization-controlled S3-compatible backup destination, independent encryption recovery key, daily schedule, seven-day retention and successful off-host restore drill. A local archive or Fly volume snapshot is insufficient.
- Measure the current single-host configuration under the 32 retained / 8 resident / 2 startup limits, including nearly full records and concurrent batches. Lower the retained ceiling if it fails. Report actual host and disk headroom; do not infer a user count.
- Verify fresh MCP and HTTP/device clients, two independent accounts, invitations and removal, app isolation/revocation, quota races, idempotency, worker eviction/restart, and credential-free logs.

Current activation status: blocked pending organization WorkOS configuration, real-client proofs, legacy/chat cutover, off-host backup destination/schedule/restore proof and host capacity measurement. No automatic infrastructure expansion.

The server refuses to start without WorkOS unless `BOG_ALLOW_LEGACY_PUBLIC_OPERATOR=true` explicitly enables a temporary legacy migration deployment. Never set this opt-in for the public team preview. Both server binaries run idle maintenance; library-only tests must invoke maintenance explicitly.

Legacy ownership claim uses the private Unix admin socket `POST /claim-legacy`, authenticated with the operator secret. Pass the operator's newly verified WorkOS access token in the JSON body `access_token`; configure `BOG_LEGACY_OWNER_ISSUER` and `BOG_LEGACY_OWNER_SUBJECT` first. The server verifies the token and exact identity match. Supply these secrets from private files/environment through a local client, never command arguments or chat. This route is not mounted on the public listener.
