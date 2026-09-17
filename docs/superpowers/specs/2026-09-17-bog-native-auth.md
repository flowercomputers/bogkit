# Direct GitHub identity and Bog-owned access

User amendment supersedes WorkOS: GitHub proves identity; Bog owns sessions, memberships, and API credentials. No authentication SaaS activation. Repair release must ship independently first. The optional scope question offered native approval-link/bearer-first versus full MCP OAuth; default is native bearer-first. Do not advertise native OAuth authorization-server support.

## User journey

A visitor signs in with GitHub and receives one idempotent personal workspace. A human may create/revoke a named account-level agent credential in the console. HTTP and bearer-capable MCP clients use the same Bog credential and select workspaces explicitly; omitted selection is personal. Agents may create Bogs and issue scoped app credentials but cannot administer members or delete Bogs. Existing app credentials and chat data remain valid.

An HTTP agent can POST /auth/device with {name}; receive a private device_code, public user_code, verification_uri, expires_in and interval; show only public approval instructions; poll POST /auth/device/token with device_code. The approval page requires GitHub login, shows requested credential name/access, requires explicit CSRF-protected approval, and offers denial. After approval, polling returns one Bog-owned agent credential exactly once. No GitHub token is accepted as a public API credential or handed to an agent. No secrets in URLs, chat, request logs, or page storage.

## Identity and sessions

Use dedicated organization-owned GitHub OAuth app, exact callback https://flower-bog-cloud.fly.dev/auth/callback, random state bound to a secure login cookie, PKCE S256, and no repository scopes (explicit empty scope). Exchange code on server, fetch https://api.github.com/user on every completed login, key account by immutable numeric GitHub id with issuer https://github.com. Never use login/email for ownership. Reject denied/error/wrong-shape/outbound failures and mismatched/expired/replayed state. Bound outbound time/body sizes and do not follow token-bearing redirects. GitHub credentials stay transient server-side and are not stored or logged.

Native session cookie: Secure, HttpOnly, SameSite=Lax, host-only, 12-hour absolute expiry, cryptographic random opaque secret stored hashed; preserve existing Origin+CSRF mutation protections. Session checks enforce local account suspension. Logout revokes local session only; do not imply GitHub account access is revoked. Refresh route, if retained for console compatibility, rotates session without extending absolute expiry. Bounded pending logins and device grants, cleanup expired state. Keep auth session storage private, excluded from backups as before.

## Bog agent credentials

Use distinct prefix and random identifier/secret, store only hash and metadata in registry with an explicit versioned migration. Record account id, name, created/expires/revoked timestamps; 30-day expiry. Scope is account delegation checked against current workspace membership on every request. API tokens cannot mint more account-level tokens. Human-only GET/POST /v1/agent-tokens and DELETE /v1/agent-tokens/{id}, with session+CSRF on writes. Revocation/expiry/suspension immediate. Tokens do not bypass workspace selection. Existing app credentials remain per-Bog with existing expiry and removal behavior. Audit issuance/revocation/approval without secret or record contents.

Device grants: 10-minute expiry, 5-second minimum poll interval, high entropy private code (hashed), cryptographic 8-character public code; per-source request limits plus global bounded storage, per-session approval attempt limit. Ignore untrusted forwarded IP headers. Pending/slow_down/expired/denied errors documented; no credential before explicit approval, no replay/double issuance under races. Approval/token issuance must recheck account suspension. Public code is not a credential; private code never placed in browser URL. Login must preserve only a validated internal public approval target.

## Configuration and rollout

Native env BOG_GITHUB_CLIENT_ID, BOG_GITHUB_CLIENT_SECRET, BOG_GITHUB_REDIRECT_URI. Reject partial configuration and simultaneous native/WorkOS configuration. Add explicit startup support for native mode; keep legacy startup opt-in while unconfigured. Native configured mode denies public operator access. WorkOS implementation may remain dormant temporarily to avoid mixing its deletion with this release, but public guidance and production instructions use direct GitHub only. Auth metadata must not advertise WorkOS or unsupported OAuth endpoints in native mode. /auth.md describes native device approval and bearer MCP. /v1 accurately reports available native authentication, account and token operations.

Before activation: remote chat credential migration and owner consumer inventory, snapshot and registry migration rehearsal, organization backup+restore and host capacity gates still apply. Private legacy claim must use verified native account and explicit configured immutable GitHub owner id, never email/username. Do not deploy native configuration or rotate owner until gates pass. Repair runtime remains separately deployable at503f75f.

## Required verification

Mock GitHub integration with real local transport: state/binding/PKCE, denied/malformed/token errors, immutable identity despite renamed login, independent accounts, idempotent personal workspace, CSRF/Origin and sessionexpiry/logout, no repo scopes, no leaked credentials. Real HTTP device start->pending->signed-inapproval->pollone-time credential->REST/MCP operations, denial/expiry/slowdown/replay/races/suspension. Agent token cannot administer/mint delegatedtokens; scoped app cannot provision. Revocation and membership removal immediate. Native registry migration and backup restore preserve existingrecords/scopedcredentials and new credential metadata; old binary versionfailclosed. Configured vs unconfigured docsexamples truthful and exercised. Root handles live provider setup and separate activation gates; do not claim mocktests prove actualGitHub interoperability.
