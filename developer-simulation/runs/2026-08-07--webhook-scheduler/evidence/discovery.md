# Ordered discovery and friction trail

This trial was run in a sanitized current-main checkout on 2026-08-07.

1. `rg --files -g 'README*' -g 'examples/**' | sort`
   - Found the public root README and four public examples: `starter`,
     `timeseries`, `chat`, and `search`.
2. `sed -n '1,260p' README.md` and the four example source files.
   - The README describes Fold as an incremental, persistent dataflow runtime
     and points to `Stream::new`, `wtx`, and `rtx` patterns.
   - `starter` shows atomic writes and durable materialized views.
   - `timeseries` shows deterministic keying and incremental aggregates.
   - `chat` shows one ingest owner and snapshot publication.
   - `search` shows keyed upsert/retraction and persistent indexes.
3. Workspace metadata was inspected only after the public surface:
   `Cargo.toml`, relevant `fold/src` API references, and the current checkout
   status. The checkout was detached at its starting `origin/main` commit and
   had no local changes.

## Friction observed

- No public example models a clock, delayed work, leases, worker crashes, HTTP
  outcomes, rate limits, or fair dispatch.
- Fold's persistence and transactional views are relevant to queue state, but
  the scheduler needs external side effects and timer decisions whose
  acknowledgement protocol is outside the shown dataflow API.
- The prototype therefore uses only the Rust standard library. This keeps the
  evidence runnable and tests the scheduling decision independently of a
  storage or networking choice.

## Baseline evaluation before component choice

The stated baseline is fixed per-endpoint queues with exponential retries and
dead-lettering. For a reproducible tail-latency comparison, this trial uses a
conservative fixture of four workers, four queued deliveries from one slow
endpoint, and a 60,000 ms response timeout. In that fixture the fixed queue
worker model can occupy all workers before a healthy tenant is admitted. It is
an explicit comparison model, not a claim that every production baseline has
the same timeout or endpoint concurrency.

The candidate scheduler's smallest meaningful change is strict one-at-a-time
endpoint dispatch plus tenant round-robin admission, endpoint queue budgets,
deterministic capped exponential retry, and durable in-flight leases. No
BogKit component is selected for the core decision loop because the public
surface does not supply the timer/lease/side-effect boundary needed here.
