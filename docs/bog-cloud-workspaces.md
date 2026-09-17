# Organizations and uncapped Bogs

Sign in at `/console`. Use **Create organization** to make a shared workspace, select it in the workspace menu, and invite people through **People**. Organization creation is human-only and requires a stable `Idempotency-Key` when using `POST /v1/workspaces` directly. Each account may create up to twenty shared workspaces. Workspace names do not grant privileges.

Platform operators see **Platform allowances** in the console. The controls apply to existing accounts and workspaces immediately:

| Flag | Effect |
| --- | --- |
| Account: Uncapped Bogs | Removes the count allowance from that account's personal workspace only. |
| Workspace: Uncapped Bogs | Removes the count allowance for all members of that shared workspace. |

An uncapped account does not uncap organizations it joins or creates. Set the organization's flag separately. Membership and normal permissions still apply. An uncapped flag never grants platform-operator access. Disabling it preserves existing records and resources; creation is refused while the workspace is at or above its standard allowance.

“Uncapped” means no per-workspace Bog-count limit. The shared host still admits at most 32 retained Bogs, at most eight resident workers and two simultaneous starts, and keeps a disk reserve. Each Bog still allows 16 MiB of logical JSON records. No automatic infrastructure purchases or scaling are enabled.

`GET /v1/workspaces` and the equivalent MCP discovery return `personal`, `uncapped_bogs` (the workspace's explicit flag), and `bog_limit` (the effective count limit, including a personal account flag). A null `bog_limit` means uncapped; legacy workspaces report their configured finite limit. Agents must explicitly select the shared workspace's ID; the API default remains personal.

Human platform operators can use `GET /v1/platform`, `PUT /v1/platform/accounts/{id}/quota`, or `PUT /v1/platform/workspaces/{id}/quota`. The PUT body is exactly `{"uncapped_bogs":true}` or false. These require the signed-in human session and its normal CSRF protection. App credentials, delegated-agent credentials, ordinary accounts, and the public legacy operator path cannot change these flags or list platform accounts.

Platform-operator assignment itself stays on the private administration channel. On the existing Fly machine, using its already configured private operator credential:

```
bog-cloud-admin platform-operator <existing-account-uuid> true
bog-cloud-admin bootstrap-workspace <existing-account-uuid> Flower <stable-bootstrap-key>
```

The second command creates the named shared workspace idempotently, makes the existing account its owner, and uncaps both that workspace and the account's personal workspace. It does not create an identity or grant operator status. Public signup captures the stable GitHub ID; the private command targets that already verified account. Security-relevant changes are recorded without secrets or record contents.

This release upgrades the registry from version 3 to 4. Older binaries reject version 4. Roll back code only with a compatible registry, or restore the pre-upgrade snapshot with the associated data; do not manually lower the registry version.
