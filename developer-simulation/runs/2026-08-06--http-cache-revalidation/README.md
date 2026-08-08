# HTTP cache revalidation trial

This is a standalone, dependency-free state-machine prototype for the assigned
cache scenario. It is intentionally not an HTTP parser, proxy, origin client,
distributed cache, CDN API, or production performance claim.

## Decision

The BogKit fit is partial but not sufficient for the acceptance-critical
prototype, so this trial uses no BogKit component at runtime.

The public examples and Fold source show useful building blocks:

- `Stream`/`KeyedStream` and `Table` provide durable, single-writer,
  incrementally maintained records.
- `InvertedIndex` can maintain tag postings.
- `Ranked` can order records for a ranking-based eviction helper.
- `Retain` is a processing-time window based on a wall clock, not a logical
  trace-time freshness/stale-if-error policy.
- The chat example's ingest thread makes the single-owner write model explicit;
  it does not provide 32-worker per-key leases.

None of those public surfaces atomically commits a Fold/fjall metadata
transaction together with a separate content-addressed body file. They also do
not supply per-key single-flight leases or tenant sequence-aware purge
semantics. Adding a custom BogKit node for those rules would make the trial a
new cache implementation rather than an evaluation of an existing component.

## Baseline model

The baseline is modeled first in `baseline_reproduction()` and is deliberately
limited to the stated behavior:

| Baseline behavior | Reproduced consequence | Reference-model correction |
| --- | --- | --- |
| Key is method plus URL | Vary variants and tenants collide | Tenant, normalized method/URL, and Vary fingerprint form the key |
| Exact-URL invalidation | A tag purge leaves a tagged response servable | Tenant-scoped tag postings invalidate matching entries immediately |
| No per-key lease | Concurrent expiry starts two revalidations | One active lease per canonical key; later workers wait |
| Metadata/body writes are separate | A crash can expose an unverified body or delete an old referenced body | Journal phases are `Prepared`, `BodyCommitted`, and `MetadataCommitted`; the in-memory model rolls back or retains references |

The baseline output is a failure reproducer, not a statement about an
unavailable production gateway.

## Reference model

The reference engine covers only the requested boundary:

- canonical key normalization, including tenant and Vary fingerprint;
- fresh, expired, stale-if-error, miss, and revalidation decisions using
  logical trace time;
- one active lease per key in a single-process serialized model and explicit
  completion events; lease expiry, worker loss, and distributed concurrency are
  outside the model;
- tenant tag indexes, duplicate purges, reordered purges, and purge fencing of
  older revalidation results;
- digest/size/verification labels and modeled body/metadata commit points;
- in-memory reachability cleanup and deterministic LRU eviction under a byte
  quota; no body bytes, filesystem deletion, SQLite transaction, fsync, or
  process-restart behavior;
- SHA-256 identifiers and stable reason codes in output.

The body is represented by a digest, size, and verification bit. No body bytes
are read, generated, or emitted, so the model cannot prove body-file deletion
safety. Origin responses are supplied by `origin` trace records.

## Running it

From the repository root:

```console
cargo fmt --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- --check
cargo test --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml
cargo clippy --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml --all-targets -- -D warnings
cargo run --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- demo
cargo run --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml -- run developer-simulation/runs/2026-08-06--http-cache-revalidation/demo.trace
```

The compact shape workload defaults to the stated 2,000,000 objects,
1,000,000 requests, and 100,000 purges:

```console
cargo build --release --manifest-path developer-simulation/runs/2026-08-06--http-cache-revalidation/Cargo.toml
/usr/bin/time -l developer-simulation/target/release/http-cache-revalidation workload
```

The workload stores compact logical records and does not allocate URLs, body
bytes, tag postings, leases, or one output record per request. It is only a
memory/quota shape check; it is not semantic evidence for 2 million objects,
1 million requests, or 100,000 purge events. The semantic tests and trace demo
are the correctness evidence.

## Trace format

The parser is intentionally small and whitespace-delimited:

```text
quota <bytes>
blob <digest-label> <size> verified|unverified
entry <tenant> <method> <url> <vary> <fresh-until> <stale-until> <digest> <size> <last-access> <tags-or-> <validator>
origin <id> error <code>
origin <id> not_modified <fresh-for> <stale-for> <validator>
origin <id> modified <digest> <size> <fresh-for> <stale-for> <tags-or-> <validator> verified|unverified
request <id> <time> <worker> <tenant> <method> <url> <vary> allow|deny <origin-id>
complete <request-id> <time> none|after_prepare|after_body|after_metadata
purge <time> <tenant> <sequence> <tag>
recover <time>
```

The command output never prints URLs, bodies, tenant names, tags, or raw
labels. Dynamic identifiers are SHA-256 hashes; reason codes are fixed strings.
Trace lines with surplus fields and records containing more than 16 distinct
tags are rejected rather than silently truncated.

## Finishing criteria

- [x] Baseline reproduced before component selection.
- [x] Reference decisions, modeled leases, purges, quota, and recovery modeled.
- [x] Unit tests cover every requested semantic area and all modeled crash
  points.
- [x] Formatting and linting are clean with warnings denied.
- [x] A deterministic demo and the stated-count compact workload run without
  network access.
- [x] Production-scale latency/throughput is intentionally not claimed; the
  actual gateway, SQLite, filesystem, body bytes, lease expiry, distributed
  workers, and process restart are outside this prototype.
