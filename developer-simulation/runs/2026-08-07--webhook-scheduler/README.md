# Webhook scheduler prototype

This is a deterministic in-memory state machine for the multi-tenant webhook
delivery scenario. It does not make HTTP requests, connect to PostgreSQL, or
claim production throughput. Inputs are timestamped enqueue, availability,
outcome, crash, restart, and clock-advance events. Outputs include durable send
decisions, retry deadlines, status transitions, queue occupancy, and fairness
latencies.

Run the focused checks from the `developer-simulation/` nested workspace root:

```console
cargo fmt --package webhook-scheduler -- --check
cargo clippy -p webhook-scheduler --all-targets -- -D warnings
cargo test -p webhook-scheduler
cargo test --release -p webhook-scheduler
cargo run --release -p webhook-scheduler -- --repeat 1000
```

The demo compares the stated fixed-queue baseline with the scheduler, holds an
endpoint unavailable for one simulated hour, exercises deterministic retry
deadlines and recovery, and simulates a crash before acknowledgement.
