# Native GitHub authentication verification

> Historical implementation checks. The prototype is now deployed and real GitHub/HTTP/MCP tests passed; see [prototype rollout](bog-cloud-prototype.md). The owner explicitly deferred production safeguards and enabled legacy-workspace compatibility for the existing chat.

## Deployment boundary

The live repair release remains `503f75f`, with registry version 2. Native authentication is implemented separately in `e3bab67` and is not deployed or activated. Its registry migration produces version 3; never run the repair binary against a migrated registry. Rollback requires a compatible pre-migration snapshot.

GitHub supplies identity. Bog supplies sessions, account-level agent credentials and individual-Bog app credentials. WorkOS is not configured or required for this path. MCP clients use bearer credentials; automatic MCP OAuth is not claimed.

## Verified locally

- All 86 cloud and MCP tests pass on macOS and Linux, including migration, backup-copy restoration, permission boundaries and existing change waits.
- A mock GitHub service verifies the real HTTP login, approval, one-time credential delivery, REST provisioning, MCP writes, REST reads and immediate revocation flow.
- Browser checks against the actual static console and approval page with fake endpoint data verified issuing, hiding and revoking a credential, explicit approval and rendered styling. These are UI checks, not real-provider acceptance.
- No production secrets, owner rotation, chat configuration or live authentication settings were changed.

## GitHub application handoff

The agent on the owner's accessible Mac reports creating the organization-owned OAuth application at https://github.com/organizations/flowercomputers/settings/applications/3864757, with the exact production callback `/auth/callback`. Wildcard callbacks and GitHub Device Flow are disabled; expiring GitHub tokens are enabled. The credential file remains private on that Mac. Transfer to the build host has not succeeded; no secret is recorded here.

## Remaining activation gates

Independent review approved the backend and, after fixing an approval-page race in `e259383`, approved the complete change. Strict Clippy passed after `ad0ac70`; four approval-page regression tests pass. Real GitHub login remains unverified until secure credential transfer. Remote chat migration to a scoped credential, owner-consumer inventory, production migration rehearsal, organization-owned off-host backup/restore and measured host capacity remain cutover prerequisites. Mock tests do not satisfy those gates.
