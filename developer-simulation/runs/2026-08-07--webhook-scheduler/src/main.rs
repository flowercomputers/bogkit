use std::time::Instant;

use webhook_scheduler::{
    Event, HttpOutcome, Observation, Scheduler, SchedulerConfig, TimestampMs,
    baseline_healthy_tail_latency_ms,
};

fn event(
    id: u64,
    tenant_id: u16,
    endpoint_id: u16,
    created_at_ms: TimestampMs,
    ttl_ms: TimestampMs,
    retryable: bool,
) -> Event {
    Event {
        id,
        tenant_id,
        endpoint_id,
        created_at_ms,
        ttl_ms,
        payload_bytes: 1024,
        retryable,
    }
}

fn config() -> SchedulerConfig {
    SchedulerConfig {
        worker_limit: 4,
        per_endpoint_in_flight_limit: 1,
        per_tenant_in_flight_limit: 1,
        endpoint_min_interval_ms: 0,
        tenant_min_interval_ms: 0,
        retry_base_ms: 1_000,
        retry_cap_ms: 60_000,
        max_attempts: 20,
        endpoint_queue_budget: 8,
    }
}

fn main() {
    let repetitions = std::env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .windows(2)
        .find(|pair| pair[0] == "--repeat")
        .and_then(|pair| pair[1].parse::<usize>().ok())
        .unwrap_or(1);

    let started = Instant::now();
    let mut last = String::new();
    for _ in 0..repetitions {
        last = demo();
    }
    print!("{last}");
    println!(
        "repeat_count={repetitions} elapsed_us={} mode=release-recommended",
        started.elapsed().as_micros()
    );
}

fn demo() -> String {
    let mut output = String::new();
    let baseline = baseline_healthy_tail_latency_ms(4, 4, 60_000);

    let mut scheduler = Scheduler::new(config(), [1, 2, 3], [10, 20, 30]);
    for id in 1..=8 {
        scheduler.enqueue(event(id, 1, 10, 0, 3_600_000, true));
    }
    scheduler.enqueue(event(100, 2, 20, 0, 3_600_000, true));
    let observations = scheduler.take_observations();
    let healthy_attempt = observations
        .iter()
        .find_map(|observation| match observation {
            Observation::SendDecision {
                attempt_id,
                event_id: 100,
                ..
            } => Some(*attempt_id),
            _ => None,
        })
        .expect("healthy event must be admitted");
    scheduler.on_outcome(healthy_attempt, HttpOutcome::Success);
    let prototype_tail = scheduler.metrics().p99_latency_ms(2).unwrap_or(0);
    output.push_str("== multi-tenant webhook scheduler ==\n");
    output.push_str(&format!(
        "baseline_healthy_p99_ms={baseline} prototype_healthy_p99_ms={prototype_tail} improvement_ms={}\n",
        baseline.saturating_sub(prototype_tail)
    ));
    output.push_str(&format!(
        "noisy_endpoint_occupancy_max={} budget={} healthy_status={:?}\n",
        scheduler.metrics().max_endpoint_occupancy[&10],
        config().endpoint_queue_budget,
        scheduler.delivery(100).unwrap().status
    ));

    let recovery_at = 3_600_000;
    let mut outage = Scheduler::new(config(), [1, 2], [10, 20]);
    outage.enqueue(event(200, 1, 10, 0, 7_200_000, true));
    let initial_outage_attempt = outage.active_attempt_ids()[0];
    outage.set_endpoint_available(10, false);
    outage.on_outcome(initial_outage_attempt, HttpOutcome::RetryableFailure);
    let first_retry_at = outage.delivery(200).unwrap().retry_at_ms.unwrap();
    outage.advance_to(recovery_at);
    let retry_observations = outage.take_observations();
    let retry_times: Vec<_> = retry_observations
        .iter()
        .filter_map(|observation| match observation {
            Observation::SendDecision { at_ms, .. } => Some(*at_ms),
            _ => None,
        })
        .collect();
    let retry_deadlines: Vec<_> = retry_observations
        .iter()
        .filter_map(|observation| match observation {
            Observation::RetryScheduled { retry_at_ms, .. } => Some(*retry_at_ms),
            _ => None,
        })
        .collect();
    outage.set_endpoint_available(10, true);
    let recovery_send_at = outage.now_ms();
    let recovered_attempts = outage.active_attempt_ids().len();
    let recovered_attempt = outage.active_attempt_ids()[0];
    outage.on_outcome(recovered_attempt, HttpOutcome::Success);
    output.push_str(&format!(
        "outage_attempts_before_recovery={} retry_times_ms={retry_times:?} retry_deadlines_ms={retry_deadlines:?} first_retry_at_ms={first_retry_at} recovery_send_at_ms={recovery_send_at} recovered_active={recovered_attempts} final_status={:?}\n",
        retry_times.len(),
        outage.delivery(200).unwrap().status
    ));

    let mut crash = Scheduler::new(config(), [1], [10]);
    crash.enqueue(event(300, 1, 10, 0, 10_000, true));
    let old_attempt = crash.active_attempt_ids()[0];
    crash.crash();
    crash.restart();
    let new_attempt = crash.active_attempt_ids()[0];
    crash.on_outcome(old_attempt, HttpOutcome::Success);
    crash.on_outcome(new_attempt, HttpOutcome::Success);
    output.push_str(&format!(
        "crash_old_attempt={old_attempt} crash_retry_attempt={new_attempt} attempts={} final_status={:?} ignored_old_outcome={}\n",
        crash.delivery(300).unwrap().attempt_count,
        crash.delivery(300).unwrap().status,
        crash.metrics().ignored_outcomes
    ));
    output
}
