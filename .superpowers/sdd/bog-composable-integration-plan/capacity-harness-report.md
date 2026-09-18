# Bounded production-image capacity harness

Implemented `scripts/cloud/composable_capacity.py` with Python standard library only. Syntax compilation and `--help` succeeded. This authoring step did not build or run Docker, exercise the live API, or establish capacity results. The integration owner runs the harness after building the final release image.

The script drives actual HTTP endpoints on a loopback-only service. It requires a container labeled `bog.capacity=local`, the literal synthetic owner token `capacity-local-fixture-not-a-production-secret`, composable enabled, eight resident slots, and an exact bind mount of a fresh temporary root to `BOG_CLOUD_ROOT`. It checks cgroup v2 limits of 1 GiB and one CPU. It rejects any existing Bogs/accounts in the initialized registry. Three ordinary synthetic accounts and personal workspaces are inserted in a short local SQLite transaction; each provisions three Bogs through HTTP. This permits nine retained Bogs without overriding either the normal three-Bog account quota or the eight resident-worker limit. These credentials are fixtures, never external account credentials, and are omitted from reports.

The run creates one records-v1 Bog and eight composable Bogs using the checked-in todo, text-search and semantic definitions. Each receives 100 records by default (configurable 100–1000). It upgrades a populated todo to the additive semantic definition, checks count and search results, distinguishes first semantic query after population from warm query, reaches eight resident workers, admits and cycles a ninth, and issues eight concurrent reads against warmed workers. First query after population is not a claim that embedding/model initialization happens at query time: population may already initialize semantic state. The optional `--restart` restarts the dedicated container, checks all acknowledged records, and checks rebuilt indexes after reopening.

A background sampler reads cgroup memory current/peak/events, CPU stats and actual manager/worker RSS from `/proc`, once per second. Stage boundaries also sample. No packages are installed in the runtime image. Cgroup peak catches short memory peaks between RSS samples; RSS sampling cannot attribute every instantaneous peak. Reports include image identity and architecture, per-request elapsed times, stage samples, sanitized errors, and PASS/FAIL. HTTP errors are fatal, including capacity responses; no hidden retries change workload results. Only restart readiness polls retry. OOM counters and missing metrics fail the run. A restart resets cgroup accounting; retain all before/after samples when summarizing.

## Integration-owner invocation

Build the production `runtime` target separately. Create a fresh directory under `/tmp`, then start the image with a direct bind mount to `/data/bog` (not its parent):

```sh
fixture_root=$(mktemp -d /tmp/bog-composable-capacity.XXXXXX)
docker run -d --name bog-composable-capacity --label bog.capacity=local \
  --memory=1g --memory-swap=1g --cpus=1 \
  -p 127.0.0.1:18080:8080 \
  --mount "type=bind,src=$fixture_root,dst=/data/bog" \
  -e BOG_CLOUD_OWNER_TOKEN=capacity-local-fixture-not-a-production-secret \
  -e BOG_CLOUD_COMPOSABLE=true \
  -e BOG_CLOUD_ALLOWED_HOSTS=127.0.0.1:18080 \
  YOUR_RELEASE_IMAGE
# Wait for GET http://127.0.0.1:18080/v1 to return 200 and registry initialization.
python3 scripts/cloud/composable_capacity.py \
  --container bog-composable-capacity --fixture-root "$fixture_root" \
  --records 100 --restart --output /tmp/bog-composable-capacity.json
```

Bind mount ownership may be changed by the image entrypoint to UID 10001. The host user must have permission to write the fresh SQLite fixture; address permissions only within the dedicated temporary root. The script never creates/removes containers or deletes data; `--restart` is its only Docker mutation. Container setup and cleanup remain the integration owner's responsibility. Use a fresh root/container for each run.

The Docker bridge is needed for the host HTTP driver; no GitHub login or external credentials are used. Local architecture/emulation, one CPU, and cgroup memory limits do not reproduce Fly VM OS overhead or latency. This is a small representative data set, not proof of the 10,000-vector ceiling, full-store capacity, all 32 retained resources, production traffic, or a user-count guarantee. Compare results with `docs/verification/bog-cloud-capacity.md` only with these scope differences explicit.
