# Claimable Bogs

Claimable Bogs let a person or agent try the real Bog Cloud service before signing in. They are separate from the existing account-bound sandboxes. A temporary Bog and its data live for one hour. Claiming before the deadline keeps the same Bog ID and records in a chosen workspace; expiry has no recovery grace period.

The feature is off by default in the server. The hosted prototype sets `BOG_CLOUD_CLAIMABLE=true` to enable anonymous creation with native GitHub sign-in. `BOG_CLOUD_CLAIMABLE_LIMIT` defaults to four simultaneously active temporary Bogs, `BOG_CLOUD_CLAIMABLE_PER_SOURCE_HOUR` to six creations from one observed peer address, and `BOG_CLOUD_CLAIMABLE_DAILY_LIMIT` to 24 overall. The service uses the actual socket peer address for the source limit, not caller-provided forwarding headers. Shared reverse proxies may cause people to share that source bucket. Capacity responses include a retry time. The normal 16 MiB per-Bog record limit and host capacity still apply.

## Human path

When enabled, the signed-out home page offers **Try a temporary Bog**. Creating is explicit; loading the page does not allocate a Bog. The page can write and read an example note, shows the one-hour deadline, and requests a short-lived claim link. Keep the tab open until the Bog is claimed: the browser holds the temporary credential only in memory. A claim link contains no record or credential access. GitHub sign-in, workspace selection, and confirmation complete the claim. The old temporary credential stops working.

## Agent path

Use either supported `bog-cloud` CLI distribution on the machine where the private credential should live:

```sh
bog-cloud --origin https://cloud.bog.new try --name notes --output /private/path/temporary-bog.json
bog-cloud --config /private/path/temporary-bog.json claim-link
```

The first command writes an owned, mode-600 JSON file; it never prints the credential. It can take `--definition /private/path/notes.json` for a bounded custom Bog. If creation is interrupted, rerun the same command with the same output file: its pending state preserves the idempotency key and recovery secret. A successful retry rotates the former temporary credential. The second command returns only a claim URL and its expiry. Show that URL to the human, not the private file. Poll `GET /claim/CODE/status` for `active`, `claimed`, or `expired` if the host can keep waiting. An MCP client can use the temporary bearer after HTTP creation; OAuth or account approval remains available for broader workspace access.

Raw HTTP clients may call `POST /v1/claimable-bogs` with `Idempotency-Key` and `{"name":"notes","recovery_secret":"64 random hex characters"}`; an optional `definition` is validated against the same composable limits as ordinary Bogs. The response contains private credential material. Keep it out of transcripts, logs, URLs, and public directories. `POST /v1/claimable-bogs/{id}/claim` requires that one-Bog bearer and creates a claim link valid for no more than 15 minutes or the remaining Bog lifetime, whichever is shorter.

## Ownership and cleanup

The temporary bearer can use only its Bog's records, exposed resources, and additive definition operations. It cannot list workspaces, create permanent Bogs, issue app credentials, or administer accounts. A claim requires a signed-in human who can create in the destination workspace. The destination must have room under its normal allowance and no Bog with the requested name. Writes drain during the transfer; the Bog retains its identity and data, while all temporary tokens are revoked. Retrying the same completed claim returns its result.

At expiry, access fails immediately. The manager tombstones the Bog, revokes credentials, and resumes filesystem cleanup after restarts. Turning creation off later does not turn expiry enforcement off. The internal holding workspace has no members and must not appear as a user workspace. The one-hour deadline is not extended by retrying creation or renewing the claim link.

## Verification and release

Run `scripts/cloud/verify_claimable.py` against only its disposable local root, then check the browser flow from the signed-out home page and the supported Python/TypeScript CLIs. Confirm the original bearer fails after claim and after expiry. After deployment, check the running service, discovery documents, actual host capacity, and one disposable create/write/read/claim flow. No existing Bog should be selected for testing or cleanup.
