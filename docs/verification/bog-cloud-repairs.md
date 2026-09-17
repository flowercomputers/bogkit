# Bog Cloud repair release verification

Status: repair release deployed and verified at https://flower-bog-cloud.fly.dev/. Direct GitHub onboarding is separate and not activated.

## Preservation and deployment baseline

On 2026-09-17, the live service had seven Bogs, including chat `21214697-82be-44d6-9328-264cc344aedd` and tester disposable `93629d91-ca1a-471b-ba5b-c29c649de310`. A private pre-deployment digest of the chat view was recorded without logging records or credentials.

Previous image: `registry.fly.io/flower-bog-cloud:deployment-01M2PQNE2NWPPA58XHGBGG4K20` (runtime commit `7a0ef09`). Host: one shared CPU, 1 GiB RAM, 3 GB encrypted volume in iad. No registry-version change is planned for this repair. No paid host expansion is authorized.

## Separate GitHub activation prerequisites

Flower WorkOS dashboard access was established using `ed@flowercomputer.com`; organization onboarding completed. No team invitations were sent and no paid feature was enabled. Production selection requires billing information, which was left for the user in the open browser tab. No WorkOS secrets were copied into the repository or deployed to Fly.

The [WorkOS pricing page](https://workos.com/pricing) was rechecked on 2026-09-17: AuthKit lists up to one million monthly active users free, but this alone does not establish Connect production entitlement or all applicable charges. The dashboard says charges begin when a paid production feature is enabled. Production setup and actual provider/client interoperability remain unverified.

Remote chat access and an organization-owned S3-compatible backup destination have been requested. The chat's owner credential has not been rotated. GitHub signup remains disabled until the existing production gates pass.

User subsequently questioned the WorkOS dependency. Further provider setup is paused; no billing action is needed for this repair release. Bog already issues scoped credentials internally without WorkOS. The provider choice for future self-service onboarding will be reconsidered separately.

## Repair verification

- Full relevant Linux suite on `3636f08`: 160 passed, one existing ignored test (Fold, serve, records, cloud, MCP). macOS cloud/MCP suite and strict Clippy also passed.
- Chrome inspected the actual locally served homepage and console: legacy allowance, unavailable signup/sharing, management versus app access, and expiry distinctions are visible. Homepage guidance is server-rendered, so it also reaches clients without JavaScript.
- Independent review found one MCP schema mismatch: the accepted legacy timeout argument was not advertised. Correction503f75f and independent re-review passed; authorization, quota, and deletion review found no additional concrete issue.

## Live repair release

Runtime commit `503f75f`, image `registry.fly.io/flower-bog-cloud:deployment-01M2QX8PBSKYG3GRTB0E82C2K3`, digest `sha256:cdce628611822b7632876c33ab0ead7045114344c104b3a672c1ab1a889ab541`. Pushed repair commits to `codex/personal-bog-cloud`. The existing single machine remains healthy with one shared CPU and 1 GiB RAM; no storage or hosting expansion. Registry remains version2; previous deployed image remains a compatible rollback for this repair. Do not reuse that rollback after a later registry migration.

The final MCP schema correction passed all seven Linux protocol tests. Live checks passed for legacy workspace discovery and explicit selection, credential listing, aggregated creation errors, template discovery, MCP schema/timeout aliases/request identifiers, idempotent creation of an eighth Bog, read/write permissions, app isolation, revocation, authorization-before-delete-confirmation, confirmed deletion and immediate credential invalidation. Temporary probe Bog and credentials were removed.

After successful live checks, the specifically authorized tester disposable `93629d91-ca1a-471b-ba5b-c29c649de310` was deleted. Six original Bogs remain. The chat view digest matched before deployment, after the acceptance probe, and after cleanup; no chat records were changed. The owner credential was not rotated. Sampled live logs contained correlated MCP requests and no occurrence of the owner secret. This is a sampled check, not a comprehensive historical log audit.

Chrome reloaded the actual public homepage and verified the repaired legacy guidance. Retest handoff is documented in `docs/bog-cloud-retest-request.md`; the remote tester has not yet rerun this release.
