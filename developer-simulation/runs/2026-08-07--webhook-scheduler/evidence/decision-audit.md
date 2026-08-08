# Decision audit

## Decision

Retain the corrected scheduler as an exploratory follow-up only, not as
production behavior or a promoted dashboard candidate. Do not adopt a BogKit
Fold component as the core delivery loop in this trial: the public component
surface is a poor fit for timer deadlines, external HTTP acknowledgement, and
lease recovery. This is a component-level no-fit; the standalone algorithm
still needs real integration evidence.

## Choices made

- Kept the endpoint head in the queue while in flight, which makes ordering
  observable and prevents a later event from bypassing it.
- Limited each endpoint to one in-flight attempt and bounded its queue.
- Used round-robin tenant admission, with independent tenant and endpoint
  minimum intervals.
- Persisted the state machine's in-flight lease before emitting a send decision.
- Requeued persisted leases on restart; ignored old attempt outcomes after a
  crash to preserve at-least-once semantics without claiming exactly-once.
- Used capped exponential retry with stable event/attempt-derived jitter rather
  than a random source.
- Kept the implementation in the standard library so the scheduling evidence
  is not confounded by HTTP, PostgreSQL, or a new dependency.

## Rejected alternatives

- **Fold as the dispatch loop:** rejected for this boundary because the public
  examples document incremental materialized views, not external side-effect
  leases or timer ownership. Fold may still be useful for reporting after an
  adapter boundary is designed.
- **Global FIFO:** rejected because a noisy endpoint can remain ahead of
  unrelated tenants.
- **Random retry jitter:** rejected because replay and operations require stable
  decisions for identical traces.
- **Multiple concurrent sends per endpoint:** rejected because it weakens the
  stated ordering guarantee and makes crash duplicate analysis harder.
- **Real HTTP/PostgreSQL in this trial:** rejected because they exceed the
  prototype boundary and would obscure the scheduler invariants.

## Uncertainty and gates before production

- The baseline comparison is a declared four-worker slow-endpoint fixture, not
  a measurement of a deployed worker.
- The durable snapshot is an in-memory clone; it proves restart transitions,
  not disk or PostgreSQL durability.
- The fairness traces are small; no claim is made for the stated traffic shape.
- Payload sizes are carried as metadata but not allocated, streamed, or
  measured.
- Production adoption requires PostgreSQL lease/ack integration, HTTP outcome
  taxonomy, load/recovery tests at 200,000 events/hour, and operational metrics.

## Skeptical review correction

The reviewer reproduced retry jitter exceeding the configured hard cap: a
1,000 ms cap produced a 1,056 ms deadline. The coordinator applied the cap
after jitter and added a regression covering 1,024 deterministic event IDs.
Debug and release suites now pass 8 tests each, and the repeated release demo
still completes 1,000 repetitions. The reviewer also narrowed the baseline,
outage, durability, and performance claims to the small in-memory model. No
recurring BogKit finding or candidate improvement was promoted.
