# Bounded local production-image observation — 2026-09-17

**PASS for the measured workload only.** The actual release manager and worker binaries ran in a local Linux amd64 Docker container under a verified 1 GiB memory limit and one CPU. The image was `sha256:4c0743e32fcb284aaa448f12582cb48f1e8a95b0c0e8303906fa3908c5f2a95e` (integration build `8181651`). This was local Docker on OrbStack, not a Fly VM benchmark; architecture/emulation and operating-system overhead differ.

The harness provisioned nine Bogs across three synthetic ordinary accounts, preserving the three-Bog workspace quota and eight resident-worker setting. Two Bogs used records-v1 and seven used the checked-in composable todo, text-search and semantic definitions. Each received 100 small records: 900 acknowledged records total. One populated todo was upgraded additively to include text and semantic search. The run observed eight resident workers, admitted a ninth Bog, cycled all nine twice, issued concurrent reads against eight warmed Bogs, restarted the container, and checked all 900 records plus maintained counts/search results after reopening. No real accounts, credentials, production service calls or paid resources were used.

| Observation | Result |
| --- | --- |
| Cgroup memory limit | 1,073,741,824 bytes |
| CPU quota | `100000 100000` (one CPU) |
| Highest cgroup memory peak across phases | 131,461,120 bytes (125.37 MiB) |
| Highest sampled cgroup current memory | 130,060,288 bytes |
| Highest sampled sum of manager/worker RSS | 150,252 KiB |
| Maximum observed resident workers | 8 |
| Cgroup OOM / OOM-kill / memory-max events | 0 / 0 / 0 |
| HTTP requests / metric samples | 1,106 / 18 |
| Elapsed first-to-last sample | 12.01 seconds |
| Final result | PASS |

The RSS sum double-counts shared pages and is not comparable directly to cgroup memory usage. One-second RSS sampling misses short-lived process peaks; cgroup peak retains aggregate peaks. The setup stop/start occurs before the workload: it safely seeds synthetic SQLite fixtures while the manager is stopped. A separate measured restart occurs after the cycling/concurrent-read stages and verifies persistence. Restart resets cgroup accounting; the peak above is the maximum of all recorded phases.

The first semantic queries after population and subsequent warm queries succeeded. Population itself can initialize embedding state, so these are not isolated cold-model latency measurements. The bounded run is too small and short to establish throughput, maximum records/vectors, full-store resource counts, behavior near a 10,000-vector ceiling, or a supported user count. It does not certify production capacity or justify enlarging limits.

Before the successful fresh run, harness-only issues were corrected: host SQLite writes while the Linux manager held the bind-mounted database open were not reliably visible; fixture seeding now stops the manager first. Startup readiness now tolerates a connection closing during startup, and a successful job response with `error: null` is accepted. These were setup/parser failures, not observed resource exhaustion. The complete successful sanitized report is `/tmp/bog-composable-capacity.json`; the dedicated local test container `bog-composable-capacity` was stopped and removed after verification; its synthetic temporary data and sanitized report were retained.
