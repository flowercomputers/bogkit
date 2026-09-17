# Native GitHub activation gates

Native authentication is a separate, undeployed followup to the repaired token-access runtime. Do not activate it until every cutover gate below passes. Mock-provider tests do not establish production GitHub interoperability.

## Authentication configuration

Use a dedicated organization-owned GitHub OAuth app with exact callback `https://flower-bog-cloud.fly.dev/auth/callback`. Configure `BOG_GITHUB_CLIENT_ID`, `BOG_GITHUB_CLIENT_SECRET`, and `BOG_GITHUB_REDIRECT_URI` together in protected runtime configuration. Partial configuration and simultaneous native/WorkOS variables are rejected. GitHub sign-in uses PKCE S256 and explicit empty scope; repository access is not requested. Account ownership uses issuer `https://github.com` and immutable numeric GitHub ID, never email or login name. Do not activate an authentication SaaS service.

Native mode accepts Bog-issued agent and scoped app credentials. It rejects public operator credentials and GitHub access tokens. HTTP and bearer-capable MCP clients share Bog credentials. Native mode does not provide an MCP OAuth authorization server or OAuth resource metadata. See the configured server's `/auth.md` for the device approval protocol.

Human sessions are opaque, private server-side records with 12-hour absolute expiry. Refresh rotates the secret without extending expiry. Sign-out revokes only the local Bog session. Account suspension is enforced locally. Agent credentials expire after 30 days; issuance/revocation is human-only, and current memberships are checked on each request. Existing single-Bog credentials remain valid under their existing rules.

## Device approval operation

An agent POSTs `/auth/device` with a short name, displays only the public user code and verification URI, and privately polls `/auth/device/token` no faster than every five seconds. The signed-in human reviews the exact name and access and explicitly approves or denies. Codes expire after ten minutes. Pending grants are bounded volatile process memory; a restart or a request reaching another process requires starting approval again. Approved grants are consumed before credential creation, so a crash or lost response can require reapproval but cannot duplicate issuance. Run one public manager process for this initial release; multi-instance shared device grants are not implemented.

Device creation is limited by actual peer address, ignoring forwarded headers (20 starts per ten minutes); a reverse proxy may share that allowance among users. Approval attempts are limited per session. Do not bypass these limits using unverified forwarding headers. Requests, audit events and browser storage must never contain raw credentials. Provider tokens are transient, server-side only; the private `auth-sessions` directory is excluded from backups.

## Data and operational cutover

- Inventory all consumers of the legacy owner credential, including the existing remote chat deployment. Replace chat access with a scoped credential and verify read/write before rotating the owner secret.
- Snapshot the existing registry and stores while safely quiesced, then rehearse registry migration and restore on copies. Registry version 3 adds account agent-credential hashes and metadata while preserving existing accounts, memberships, Bogs, scoped credentials and records. The version-2 repair binary rejects version 3. Rollback therefore requires restoring the pre-migration snapshot; never point the repair binary at migrated storage.
- The encrypted off-host backup format records the actual registry version. Version-2 archives still restore; opening them with the new binary migrates the restored registry to version 3. Version-3 archives preserve agent credential metadata, including revoked/expired states. Sessions and pending device grants intentionally do not survive backup restoration. Rehearse both archive generations before activation.
- Configure an organization-controlled backup destination, independent encryption recovery key, daily schedule, seven-day retention and successful off-host restore drill. A local archive or volume snapshot is insufficient.
- Measure host and disk headroom on the existing single-host configuration, including retained/resident/startup limits, nearly full records and concurrent batches. Lower limits if necessary; no infrastructure expansion is implicit.
- Verify fresh real GitHub sign-in with two independent accounts, renamed GitHub login, device approval/denial, HTTP and bearer MCP, invitation/removal, app isolation/revocation, idempotency, worker eviction/restart and credential-free logs.

Startup without configured authentication requires explicit `BOG_ALLOW_LEGACY_PUBLIC_OPERATOR=true` for the existing token-access service. This flag does not re-enable public operator access in native mode. Dormant WorkOS compatibility code remains, but is not the activation path.

Legacy ownership claim uses only the private Unix admin socket `POST /claim-legacy`, authenticated with the private operator secret. First configure `BOG_LEGACY_OWNER_GITHUB_ID` to the independently verified canonical numeric owner ID. Send the owner's valid native session secret as JSON `session_token` through a private local client; do not supply a GitHub token, username or email. The server verifies the session, account status and exact immutable identity. Keep both secrets out of shell arguments, history, chats and logs. This route is absent from the public listener. Do not claim legacy ownership until the consumer inventory, chat migration and restore gates pass.

Activation remains blocked until organization app configuration, real-provider proofs, owner/chat cutover, off-host recovery and host-capacity checks are complete.
