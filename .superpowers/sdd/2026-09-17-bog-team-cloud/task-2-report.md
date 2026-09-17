# Task 2: WorkOS authentication

Implemented WorkOS Connect JWT verifier and browser-session module. Configuration validates HTTPS issuer/resource/callback URLs, separates API audience from browser client ID, redacts server secret in Debug, and offers an explicit numeric-loopback test configuration. Access JWTs require signature, RS256, fixed issuer/audience, nonempty subject/client ID, expiry, optional nbf, and no nonce. ID tokens require browser audience and matching login nonce. Only trusted verification creates VerifiedIdentity; issuer()/subject()/client_id()/expires_at() are read-only getters.

JWKS responses are capped at 64 KiB/32 keys, with 8s overall/3s connect timeouts, no redirects, 5-minute cache, 5-second refresh throttle, and no stale-key fallback after refresh failures. Key metadata is checked for signing/verification/RS256 suitability.

Browser login uses WorkOS /oauth2/authorize and /oauth2/token with PKCE S256, random state/nonce, and separate browser-binding cookie. State is consumed before code exchange, preventing replay. Login TTL is 10 minutes. Token exchange never sends secrets in URLs. Sessions store opaque-cookie hashes, provider tokens, CSRF token, verified subject/issuer, and 12-hour absolute expiry in a separate SQLite file with a private owner-only directory/file and secure deletion. Cookies always have __Host prefix, Secure, HttpOnly, Path=/, SameSite=Lax. HTTP fixture configuration does not relax cookies.

BrowserAuth exposes begin_login, complete_login, authenticate, csrf_token, check_csrf, refresh, logout. All cookie-authenticated mutations must call check_csrf (exact configured Origin plus session CSRF token); refresh/logout call it internally. Access-token expiry causes authenticate to fail until refresh is explicitly called. Refresh is serialized with logout; local logout deletes the session before network calls. Provider revocation is attempted only when WorkOS discovery advertises a same-origin revocation_endpoint; LogoutResult.provider_revoked records success rather than inventing a revocation endpoint.

WorkOsConfig.resource_metadata and auth_markdown publish only supported discovery and provider-directed flows. GitHub provider enablement belongs in the WorkOS dashboard. Device flow is delegated to provider documentation/discovery, not implemented as a Bog OAuth server.

## Verification

Initial three integration tests passed with a local mock issuer and signed RSA fixtures. Expanded final test result recorded below. Tests use no real provider credentials. The intentionally public test RSA private key is exclusively a fixture. Network binding requires sandbox escalation on this machine.

## Integration and limits

Root owns HTTP wiring and env config. Clear the __Host-bog_login cookie after callbacks; never serialize provider credentials. Store the session directory on private persistent storage, exclude it from public backups and logs. Storage uses filesystem access protection, not application-level encryption; host/storage encryption is an operational concern. Provider outage/revocation failure cannot prevent local logout; provider_revoked=false must not be described as remote revocation. Production WorkOS signup, hosted GitHub connection, real audience settings and redirect registration still require account setup and live verification. No production authentication claim is made.

Sources checked: https://workos.com/docs/reference/workos-connect/authorize ; https://workos.com/docs/reference/workos-connect/token ; https://workos.com/docs/reference/workos-connect/metadata ; https://workos.com/docs/authkit/connect/oauth . The official token reference documents separate Connect access-token aud/client_id and ID-token browser audience/nonce.

Device configuration update: WorkOsConfig::with_device_client_id accepts the operator's registered public CLI client ID. auth.md then publishes concrete WorkOS device authorization, polling/backoff/expiry, access and refresh instructions. Without that configuration, it explicitly says device authentication is unavailable. Root must load BOG_WORKOS_DEVICE_CLIENT_ID. Official device reference: https://workos.com/docs/reference/workos-connect/cli-auth/authorize-device and https://workos.com/docs/reference/workos-connect/cli-auth .

Release blockers: provider revocation remains unverified against live WorkOS metadata, and cannot be called complete if the selected environment does not support it. WorkOS application registration, GitHub enablement, matching audience, browser callback and public CLI client registration must be verified live before production rollout. Local mock test success is not a substitute.

Final verification: `cargo test -p bog-cloud --test oauth_security` passed all 7 tests (0 failures), including real local HTTP mocks and real RSA signatures. `git diff --check` passed for owned files. Access/ID token separation and multi-audience ID-token authorized-party checks are enforced. Shared module exports are left to root/foundation's lib.rs commit.
