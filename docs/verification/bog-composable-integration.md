# Composable Bog Cloud integration verification

## Scope and status

Integrated deployed-cloud source `d760757` and composable source `7ed53ea` in `codex/integrated-cloud-capabilities`, preserving both histories and both original worktrees. This report separates source integration from live deployment. No live registry migration, feature activation, customer-record changes, or hosting expansion was performed during integration.

The combined service gains declarative resources, maintained transformations/aggregates, text and semantic search, additive definition rebuilds, and definition-aware backup/restore through the existing permission layer. Existing records-v1 stores retain their compatibility path.

## Integration corrections

- Worker pressure admission now also handles definition builds and configured restores. It reclaims eligible idle workers, reserves source/candidate capacity, and protects journaled builds under the same exclusive gate used for eviction. Unknown surviving processes still count against physical capacity.
- A known unsupported pause endpoint on an adopted old worker fails cleanly; it no longer invents a persistent write freeze. Ambiguous failures remain conservative. The regression uses the actual unwrapped records router and verifies subsequent writes.
- Retained-resource quotas are independent of the eight running-worker slots. Ordinary workspace allowance remains three; legacy configured allowance and uncapped flags are preserved; platform retained ceiling remains 32.
- Migration tests start from genuine v4 migrations, preserve every prior table and permissions, exercise real persisted records, and retain a byte-identical recovery copy. Feature-off startup still upgrades the registry to v5; an old binary correctly rejects v5.

## Verified locally

- `scripts/cloud/verify_composable.sh`: 285 Rust tests and 17 JavaScript tests passed. One optional `checkpoint_latency_sample` benchmark remains ignored by its existing annotation.
- Strict Clippy for bog-definition, bog-runtime, bog-cloud-records, bog-cloud and bog-cloud-mcp across all targets, with warnings denied: passed after semantics-preserving cleanup (`7672bdc`). Afterwards, 16 runtime and 12 definition tests passed again.
- Independent review approved the integration fixes, migration boundaries and lint cleanup, with no remaining actionable findings. The initially identified old-worker freeze issue was fixed and regression-tested.
- Additional preserved-old-executable migration probe passed. This proves registry-format recovery compatibility, not a full old-worker HTTP recovery drill. See [migration evidence](bog-composable-migration.md).

## Production-image measurement

The Fly remote builder produced and pushed `registry.fly.io/flower-bog-cloud:composable-integration-8181651`, digest `sha256:d94d2fe268d8d7340fbe930f2bb41a6815b6cbb8e30af59acf2d21059bd071e6`. Build-only mode did not deploy it. Later source changes comprise tests, documentation and independently reviewed semantics-preserving lint cleanup.

The isolated image run passed: nine Bogs × 100 records; a populated semantic rebuild; eight resident workers; ninth-Bog cycling; eight concurrent reads; and restart recovery of all 900 acknowledged records and indexes. It issued 1,106 HTTP requests and collected 18 samples. Cgroup memory peak was 131,461,120 bytes (125.37 MiB), with zero memory-limit or out-of-memory events. See [measured capacity evidence](bog-composable-capacity-results.md). The test is limited to one CPU, 1 GiB, nine retained Bogs and 100 records each, on Linux amd64 under local OrbStack emulation. It is not a Fly-host latency certification, maximum-store measurement, or supported-user-count claim.

## Rollout boundary

Hosted activation remains separate: take a current quiescent whole-store recovery copy, deploy a matching final image with composable creation disabled, verify canonical discovery/auth and the existing chat, then run a disposable hosted composition/build canary while measuring actual host memory. Enable only after that check. No paid expansion is authorized.

Do not run an old binary against the migrated v5 registry or lower its version marker. Restoring a pre-upgrade copy after new writes loses those writes; prefer a compatible forward fix. See [integration plan](bog-composable-integration-plan.md), [migration evidence](bog-composable-migration.md), [quota regressions](bog-composable-quota-integration.md), and [capacity harness](bog-composable-capacity-harness.md).
