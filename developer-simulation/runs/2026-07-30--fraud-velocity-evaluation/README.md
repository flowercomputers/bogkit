# Fraud velocity evaluation

This is a dependency-free reference prototype for deterministic payment-velocity
decisions. It evaluates the existing Redis-counter design first, then exercises:

- deduplication by event ID and merchant event ID;
- event-time windows over account, salted card fingerprint, device, and IP prefix;
- linked corrections when an event arrives late;
- canonical decision bytes across a normal close/reopen replay;
- latest decisions checked against a naïve reference and retained alert
  records checked for internal reconstructability;
- customer-key scrubbing while shared device/IP contributions remain;
- a repeated release-mode benchmark with 1% duplicates and 5% late events.

It deliberately does not use a BogKit component. Fold's durable,
transactional materializations and ranked event-time range scans are useful,
but the inspected public contracts do not provide the complete maintained
semantics for linked corrections, two-ID deduplication, deletion, partition
reassignment, and atomic broker-offset ownership that this scenario requires.

## Data and deterministic rules

The fixture is tab-separated and contains only the supplied fields. Card values
are already salted fingerprints; no raw card or address data is accepted.
Arrivals are ordered by `(arrival_time_ms, event_id)`. Event-time ties are ordered
by `event_id`. Windows are `(target_time - window, target_time]`, partitioned by
currency, and count every authorization attempt. The demonstration rules are:

| Rule | Window | Alert threshold |
| --- | ---: | --- |
| account | 1 minute | count at least 3 |
| salted card fingerprint | 10 minutes | count at least 2 and amount at least 100,000 minor units |
| device | 10 minutes | count at least 4 |
| IP prefix | 24 hours | count at least 6 |

Every alert records the rule, window, count, amount, currency, and canonical
contributing event IDs. A correction links to the immediately preceding revision.

## Reproduce

Run from `developer-simulation/`:

```console
cargo run -p fraud-velocity-evaluation -- baseline
cargo test -p fraud-velocity-evaluation
cargo fmt -p fraud-velocity-evaluation -- --check
cargo clippy -p fraud-velocity-evaluation --all-targets -- -D warnings
cargo run --release -p fraud-velocity-evaluation -- demo
cargo run --release -p fraud-velocity-evaluation -- benchmark 100000 3
```

The benchmark's first argument is the number of unique generated events and
its second is the number of repeated rounds. Use at least two rounds. It is a
sparse-key in-memory upper bound, checks latest state against a bounded naïve
reference before timing, reports correction and alert counts, and rejects any
digest drift between rounds.

## Important boundary

This is a reference boundary, not a production service. It does not implement
four-replica partition transfer, atomic stream-offset commits, live Redis network
measurements, a durable customer-deletion compaction, a 30-minute load run, or a
20-million-event state measurement. `EVIDENCE.md` records the resulting no-fit
decision without attributing those missing capabilities to a demonstrated
BogKit defect.
