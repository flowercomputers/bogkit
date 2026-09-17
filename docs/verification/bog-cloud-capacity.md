# Local capacity observation — 2026-09-17

Status: **incomplete; not a rollout gate or capacity certification**. The user rescoped to a usable prototype while this bounded local check was running. No production resources, settings, or data were changed.

The current release sources were built using `deploy/bog-cloud/Dockerfile` in local Linux Docker (OrbStack, ARM64). An isolated release-mode example used the same `CloudService`, supervisor and records worker. Docker ran with `--network none --cpus 1 --memory 1g --memory-swap 1g`; the harness asserted cgroup `cpu.max = 100000 100000` and `memory.max = 1073741824`. Eight resident workers and two start slots were configured. Idle timeout was shortened to 100 ms only in the harness; production is 600 seconds.

The harness provisioned 32 resources across 11 synthetic ordinary workspaces, verified rejection of a fourth resource in one workspace, and verified rejection of resource 33. Synthetic account fixtures bypassed external login; this was not an authentication test. Record operations used the local operator fixture.

The first test fixture exceeded the 256 KiB encoded record bound and was corrected. The corrected workload requested 64 records with 255 KiB payloads per Bog. Six Bogs reported 16,713,910 logical bytes each against a 16,777,216-byte quota (99.62% full). Two simultaneous cold-start requests returned `capacity: surviving workers occupy resident slots; retry when they recover or stop an unused database`. The harness stopped on those errors, with exit code 101 and Docker `OOMKilled=false`.

This observation establishes enforcement of the container limits and the resource-count boundaries. It does **not** establish peak memory, CPU throughput, disk amplification, preservation after eviction/restart, or successful operation of all 32 nearly-full Bogs. The run did not reach those measurements. A capacity response during startup does not by itself justify a lower memory/resource recommendation. No limit changes are recommended from this incomplete result.

If capacity work is resumed, distinguish retryable concurrent cold-start pressure from sustained resident-worker load, capture resource metrics even on an early failure, and complete acknowledged-record checks after eviction and manager restart. Local container memory is not the same as memory available to the application on a Fly VM; reserve space for the operating system and gateway. No user-count estimate follows from this test.
