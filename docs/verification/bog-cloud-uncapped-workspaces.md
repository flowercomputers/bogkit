# Uncapped organizations — September 17, 2026

Deployed runtime `6dd52a6` to the existing Fly machine and volume. Image `registry.fly.io/flower-bog-cloud:deployment-01M2R4C1PCZDN1H5XNDSM1GX04`, digest `sha256:5045d2921a3688a90ce052c35066c62a1f5d3dc20e5da30210c11fa86f5dbd2e`.

## Activated

- Existing verified account `060b9767-ffe9-4dde-a79a-c11d880a9a66` (GitHub ID `1195363`) has uncapped personal Bogs.
- Created shared **Flower** workspace `30a9f5ec-7f2d-42d8-acc4-54efc75b7d1d`, with that account as owner and its workspace flag enabled.
- Personal workspace remains `781503d9-f074-4c50-96f5-cb974e1bf5e4`. Default API selection remains personal; callers must specify Flower's ID.
- After the user's explicit confirmation, enabled platform allowance controls for the account. This permits account/workspace listing and quota changes, not access to other users' records or membership administration.
- No test Bogs or records were added. Flower starts empty and ready for projects and invitations.

## Verified

All 100 cloud/MCP tests passed, including migration, standard/uncapped quotas, revocation without data deletion, global limits, organization retry behavior, isolation, and actual session/CSRF/role enforcement. Strict lint checks passed. Independent review found no remaining actionable defects.

Live REST returned both personal and Flower workspaces with effective `bog_limit: null`, Flower owner membership, and an empty accessible Bog list. The existing delegated agent received 403 for platform administration both before and after its human owner received the operator flag.

Reloaded the signed-in production console, selected Flower, and verified the visible uncapped allowance, Create organization form, invitation controls, and Platform allowances section. The account flag and Flower flag displayed enabled. Personal displayed its inherited uncapped allowance. Existing legacy chat reads still succeeded and its record-data fingerprint was unchanged. Public discovery, authentication boundary and rate headers also passed the post-deploy checks.

## Recovery and bounds

Registry upgraded from version 3 to 4. Pre-upgrade Fly snapshot `vs_gqJNqZDADPRTVDlP2ZwK` completed at `2026-09-17T16:47:05Z`, five-day retention. Older binaries reject registry version 4; rollback requires a compatible registry or restoring the associated snapshot/data.

Only workspace Bog counts are uncapped. Global retained capacity remains 32, resident workers eight, simultaneous starts two, and logical JSON storage 16 MiB per Bog. No infrastructure expansion, new SaaS dependency, or automatic purchases.

Usage and administration: [Organizations and uncapped Bogs](../bog-cloud-workspaces.md).
