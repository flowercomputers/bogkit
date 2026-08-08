# Verification record

All commands below were run from the sanitized current-main checkout after the
prototype was written. The workspace's pre-existing `examples/search` source
is not formatted by this trial; package-scoped formatting was used so the
trial did not rewrite unrelated files.

## Checks

| Check | Command | Observed result |
| --- | --- | --- |
| Focused formatting | `cargo fmt --package webhook-scheduler -- --check` | exit 0 |
| Strict lint | `cargo clippy -p webhook-scheduler --all-targets -- -D warnings` | exit 0, no warnings |
| Debug tests | `cargo test -p webhook-scheduler` | 8 passed, 0 failed |
| Release tests | `cargo test --release -p webhook-scheduler` | 8 passed, 0 failed |
| Repeated release demo | `cargo run --release -p webhook-scheduler -- --repeat 1000` | completed; 1,000 repetitions |

## Demonstration output

The final release demonstration reported:

```text
baseline_healthy_p99_ms=60000 prototype_healthy_p99_ms=0 improvement_ms=60000
noisy_endpoint_occupancy_max=8 budget=8 healthy_status=Delivered
outage_attempts_before_recovery=1 retry_times_ms=[0] retry_deadlines_ms=[1071] first_retry_at_ms=1071 recovery_send_at_ms=3600000 recovered_active=1 final_status=Delivered
crash_old_attempt=1 crash_retry_attempt=2 attempts=2 final_status=Delivered ignored_old_outcome=1
repeat_count=1000 elapsed_us=15193 mode=release-recommended
```

The elapsed time is a harness repeat time, not a throughput claim. Payload
bytes are represented in the event contract but are not allocated or sent.
