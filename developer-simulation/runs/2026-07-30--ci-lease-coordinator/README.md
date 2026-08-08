# CI lease coordinator trial

This is a clean-room evaluation of BogKit's `fold` crate for durable CI
coordinator state. It is deliberately a prototype, not a production
coordinator.

## Decision

**BogKit is not a fit for the complete scenario.** After explicit
coordinator-time deadline checks, Fold handled the modeled single-active-
coordinator subset: one transaction can fence an attempt, commit an immutable
result, and make dependent jobs ready without exposing partial state. However,
Fold opens an embedded single-writer database and rejected a second concurrent
opener in this trial. It does not provide the consensus, leader fencing, or
replicated compare-and-swap needed by three concurrently running coordinator
replicas.

The prototype therefore demonstrates only the useful single-writer subset. It
does not suggest placing an unsupported replication layer around Fold.

## What it covers

- Durable job, dependency, lease, attempt, terminal-result, and explanation
  fields.
- Incrementally maintained ready and leased indexes.
- Coordinator-time lease acquisition, renewal, expiry, and reassignment.
- Exact-deadline and far-past-deadline rejection for heartbeats and
  completions, without reviving the expired attempt.
- Attempt fencing and immutable terminal results.
- Atomic dependency readiness after a parent commits.
- Explicit reasons for blocked, retried, and rejected operations.
- A deterministic test that exits 100 child coordinator processes immediately
  after a committed acknowledgment, then reopens state and injects duplicate
  and reordered messages.
- A 100,000-job recovery, storage, JSON-baseline, and batched-update benchmark.

## Run

Run from `developer-simulation/`:

```console
cargo run --release -p ci-lease-coordinator -- demo
cargo run --release -p ci-lease-coordinator -- bench /tmp/bogkit-ci-lease-benchmark.db
cargo test -p ci-lease-coordinator
cargo fmt -p ci-lease-coordinator -- --check
cargo clippy -p ci-lease-coordinator --all-targets --no-deps -- -D warnings
```

The subcommands can also be run separately:

```console
cargo run --release -p ci-lease-coordinator -- baseline
cargo run --release -p ci-lease-coordinator -- fault /tmp/bogkit-ci-fault.db
cargo run --release -p ci-lease-coordinator -- multi-writer /tmp/bogkit-ci-writer.db
```

`demo` uses and resets fixed directories under the system temporary directory.
`bench` resets the database path passed on its command line and overwrites the
same path with a `.json` extension for the baseline measurement. Do not point
either command at valued data.

## State-machine notes

Every lease assigns a new attempt number. Heartbeats and completions must match
both the worker and attempt, and the coordinator must observe them strictly
before the stored deadline. Once a completion wins, its attempt and immutable
object-store key are stored in the terminal record; later messages return a
reason naming that winner and make no state change.

A completion and all readiness changes it unlocks occur in one Fold
transaction. A child remains pending until every dependency has a terminal
record in that same transaction's view.

The heartbeat replay test assumes the existing worker request already has a
stable, increasing message identifier within an attempt. The problem brief did
not specify the existing protocol's fields, and changing that protocol is a
non-goal. If the real protocol lacks such an identifier, exact heartbeat replay
idempotence is an unresolved protocol mismatch and this prototype is not
deployable.

The benchmark groups at most 32 heartbeats into one durable transaction and
acknowledges them after that transaction commits. Its reported latency is the
batch commit time assigned to each message; it excludes time waiting to form a
batch, networking, request parsing, and replica coordination. Treat it as an
embedded-store upper bound.

See [EVIDENCE.md](EVIDENCE.md) for the complete trail, observed output, limits,
and decision audit.
