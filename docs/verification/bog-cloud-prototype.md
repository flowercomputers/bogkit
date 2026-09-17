# Bog Cloud prototype rollout — 2026-09-17

The owner explicitly requested a usable demo first. Scheduled cloud backups, capacity certification, owner-secret rotation and two-person sharing acceptance are deferred; they do not block this prototype.

## Recovery and preservation

- Captured the closed production registry and stores before migration and verified an encrypted local recovery copy.
- Fly snapshot `vs_zx21xaDgDZQfOBeOm2AP` completed at 2026-09-17T15:29:25Z.
- Booted an isolated copy with the new binary: registry version 2 upgraded to 3, all six Bog record digests and existing credential/permission tables matched.
- Restored the old runtime after capture and verified all six live record digests, including the chat, unchanged.

## Prototype compatibility

Commit `67871e8` adds an explicitly enabled preview switch allowing the existing legacy credential only within the legacy workspace while GitHub users receive separate workspaces. Native defaults remain closed. Eight focused authentication tests, strict cloud lint and independent review pass. No WorkOS service is used.

## Existing chat acceptance

Extracted the supplied bog-chat-work.zip into a private temporary test directory. Ran the app locally against the existing chat Bog with a fresh single-Bog write credential. Reading, posting, blank-name normalization and change waiting passed; the credential could not list Bogs or mint credentials. Removed the temporary message and revoked the test credential; original records matched afterward. This verifies the supplied app, not replacement of the credential on the remote Mac.

## Live activation

Deployed runtime `67871e8` to the existing single Fly machine, image `registry.fly.io/flower-bog-cloud:deployment-01M2R03FQVF0SKKD44YBVR7GTP`, digest `sha256:5f9a54ff1fac3c55d64f9273f712d1011c583147d2db8e26fe28bf9f94e0a9bc`. Health checks pass and `/v1` reports `github_native`. No paid expansion or AWS resources.

- Real GitHub browser sign-in completed and the authenticated console loaded its personal workspace.
- The owner approved the device request from their own machine. Polling delivered one Bog agent credential privately; `/v1/me` identifies an agent in its personal workspace.
- Created `browser-demo` through the live console: `8134e8b9-577c-4544-bed2-200dc681e94d`. The approved agent can use the same Bog through HTTP.
- A real Codex MCP client (`0.154.0-alpha.6.2`) made 12 successful tool calls to create and exercise Bog `6dec482b-660a-4348-b2b5-ddfd89b55a98`; independent REST reads verified the final records. The older Homebrew CLI initially failed before any service calls because it could not use its configured model; the installed current app CLI resolved that client issue.
- Live HTTP checks passed for scoped app issuance, read/write, change waiting, read-only write denial, app management denial, agent administration denial and immediate app-token revocation.
- Verified both directions of isolation: the legacy operator cannot access the new personal Bog, and the new agent cannot access the legacy chat Bog.
- Ran the supplied chat after deployment against both the legacy chat Bog and the new personal Bog, using temporary single-Bog credentials. Reads, writes and long-polling passed. Temporary messages removed and credentials revoked; original records unchanged.
- Sampled Fly logs and MCP test artifacts contained none of the known operator, GitHub or agent secrets. No full historical-log audit is claimed.

Two small demo Bogs remain in the signed-in personal workspace, leaving one of its three slots free. The named approved test-agent connection remains available in the console for revocation. The remote Mac's existing chat configuration and owner secret remain unchanged under the explicit preview compatibility switch.

For another agent, share the homepage and [retest request](../bog-cloud-retest-request.md). No token needs to be pasted into chat. This prototype supports bearer-capable MCP after device approval, not automatic MCP OAuth discovery.
