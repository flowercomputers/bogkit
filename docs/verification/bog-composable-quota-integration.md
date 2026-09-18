# Retained Bog quotas during composable integration

## Problem and correction

Composable creation used the configured resident-worker allowance (normally eight) as the maximum number of retained Bogs across all workspaces. That prevented creating another Bog when the platform already retained nine or more, even for an uncapped account. Private restore and ordinary legacy creation shared the same accidental platform limit.

The registry now checks platform retention against 32 independently. Ordinary workspaces retain their three-Bog allowance; account/workspace uncapped flags retain their existing behavior; the legacy workspace keeps its supplied allowance (normally eight). Resident worker admission remains unchanged. Restores use the same corrected registry path without changing backup behavior.

## Focused regression evidence

New `registry::quota_tests` covers:

- Nine retained Bogs in three existing workspaces, then three successful configured creations in another workspace; its fourth is rejected.
- Nine retained Bogs followed by an uncapped account creating through platform total 32; creation 33 and an additional private restore are rejected.
- Platform total already nine, then private restore, plain records creation, and configured creation filling the legacy workspace's eight slots; its ninth creation/restore are rejected.

No deployment or live state modification is involved. Tests use temporary registries and synthetic identities.

Passed: `cargo test -p bog-cloud --lib registry::quota_tests` (3); `cargo test -p bog-cloud --lib workspace::tests` (16, including account/workspace uncapped flag revocation and stopped/deleted retention); `cargo test -p bog-cloud --test registry --test limits` (7, including configured legacy allowances five/eight and idempotent capacity checks). Total: 26 focused tests.
