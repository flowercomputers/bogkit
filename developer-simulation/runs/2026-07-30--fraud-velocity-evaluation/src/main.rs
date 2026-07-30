use std::path::{Path, PathBuf};
use std::time::Instant;

use fraud_velocity_evaluation::{
    Engine, IngestResult, PersistentEngine, baseline_observations, generated_event, load_fixture,
    percentile_nanos,
};

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("demo.tsv")
}

fn run_baseline() {
    println!("redis baseline");
    for observation in baseline_observations() {
        println!("  {observation}");
    }
}

fn run_demo() -> Result<(), String> {
    run_baseline();
    let events = load_fixture(&fixture_path())?;
    let mut engine = Engine::default();
    let mut duplicates = 0;
    let mut corrections_from_late_event = 0;
    for event in &events {
        match engine.ingest(event.clone()) {
            IngestResult::Accepted { correction_count } => {
                if event.event_id == 8 {
                    corrections_from_late_event = correction_count;
                }
            }
            IngestResult::ExactDuplicate => duplicates += 1,
            IngestResult::ConflictingDuplicate => {
                return Err(format!("fixture event {} conflicts", event.event_id));
            }
        }
    }
    engine.assert_matches_reference()?;
    engine.reconstruct_all_alerts()?;
    println!("deterministic fixture");
    println!(
        "  {} deliveries -> {} unique events; {} duplicate ignored",
        events.len(),
        engine.event_count(),
        duplicates
    );
    println!(
        "  {} linked corrections from late event 8; {} total corrections",
        corrections_from_late_event,
        engine.correction_count()
    );
    println!(
        "  {} canonical records; {} alert explanations; digest {:016x}",
        engine.record_count(),
        engine.alert_count(),
        engine.digest()
    );

    let recovery_root =
        std::env::temp_dir().join(format!("fraud-velocity-demo-{}", std::process::id()));
    let direct_path = recovery_root.join("direct");
    let restarted_path = recovery_root.join("restarted");
    let _ = std::fs::remove_dir_all(&recovery_root);
    let direct = {
        let mut persistent = PersistentEngine::open(&direct_path)?;
        for event in &events {
            persistent.ingest(event.clone())?;
        }
        persistent.decision_bytes()
    };
    let restarted = {
        let split = events.len() / 2;
        {
            let mut persistent = PersistentEngine::open(&restarted_path)?;
            for event in &events[..split] {
                persistent.ingest(event.clone())?;
            }
        }
        let mut persistent = PersistentEngine::open(&restarted_path)?;
        for event in &events[split..] {
            persistent.ingest(event.clone())?;
        }
        persistent.decision_bytes()
    };
    if direct != restarted {
        return Err("restart changed canonical decision bytes".to_string());
    }
    println!(
        "normal close/reopen replay\n  uninterrupted and reopened ledgers produced {} identical decision bytes",
        direct.len()
    );
    let _ = std::fs::remove_dir_all(&recovery_root);

    let before_device = engine.latest_rule_count(7, "device-10m-count-4");
    let before_ip = engine.latest_rule_count(7, "ip-prefix-24h-count-6");
    let receipt = engine.delete_customer(100)?;
    if before_device != engine.latest_rule_count(7, "device-10m-count-4")
        || before_ip != engine.latest_rule_count(7, "ip-prefix-24h-count-6")
    {
        return Err("customer deletion corrupted shared device/IP counts".to_string());
    }
    engine.scan_customer(&receipt)?;
    engine.reconstruct_all_alerts()?;
    println!(
        "customer deletion\n  scrubbed {} account-owned events and audited account/card removal; shared device/IP counts unchanged; retained alerts reconstruct",
        receipt.event_count()
    );
    println!("demo: PASS");
    Ok(())
}

fn run_benchmark(unique_events: usize, rounds: usize) -> Result<(), String> {
    if unique_events == 0 || rounds < 2 {
        return Err("benchmark needs at least one event and two rounds".to_string());
    }
    println!(
        "sparse-key in-memory upper-bound benchmark: {unique_events} unique events + 1% duplicate deliveries, {rounds} rounds"
    );
    let reference_probe_events = unique_events.min(2_000);
    let mut reference_probe = Engine::default();
    for index in 0..reference_probe_events {
        let event = generated_event(index as u64);
        reference_probe.ingest(event.clone());
        if index % 100 == 99 {
            reference_probe.ingest(event);
        }
    }
    reference_probe.assert_matches_reference()?;
    println!(
        "  sparse-key latest-state naive-reference comparison: PASS ({reference_probe_events} unique events)"
    );
    let mut throughputs = Vec::with_capacity(rounds);
    let mut p99_values = Vec::with_capacity(rounds);
    let mut final_digest = None;
    let mut payload_bytes = 0;
    for round in 0..rounds {
        let mut engine = Engine::default();
        let mut latencies = Vec::with_capacity(unique_events + unique_events / 100 + 1);
        let wall_start = Instant::now();
        for index in 0..unique_events {
            let event = generated_event(index as u64);
            let event_start = Instant::now();
            engine.ingest(event.clone());
            latencies.push(u64::try_from(event_start.elapsed().as_nanos()).unwrap_or(u64::MAX));
            if index % 100 == 99 {
                let duplicate_start = Instant::now();
                let result = engine.ingest(event);
                latencies
                    .push(u64::try_from(duplicate_start.elapsed().as_nanos()).unwrap_or(u64::MAX));
                if result != IngestResult::ExactDuplicate {
                    return Err("benchmark duplicate altered state".to_string());
                }
            }
        }
        let elapsed = wall_start.elapsed();
        engine.reconstruct_all_alerts()?;
        let deliveries = latencies.len();
        let throughput = deliveries as f64 / elapsed.as_secs_f64();
        let p99 = percentile_nanos(&mut latencies, 99);
        let digest = engine.digest();
        if let Some(expected) = final_digest
            && expected != digest
        {
            return Err("benchmark rounds produced different decision digests".to_string());
        }
        final_digest = Some(digest);
        payload_bytes = engine.approximate_payload_bytes();
        throughputs.push(throughput);
        p99_values.push(p99);
        println!(
            "  round {}: {:.0} deliveries/s, p99 {:.3} ms, {} corrections, {} alert outcomes, digest {digest:016x}",
            round + 1,
            throughput,
            p99 as f64 / 1_000_000.0,
            engine.correction_count(),
            engine.alert_count()
        );
    }
    throughputs.sort_by(f64::total_cmp);
    p99_values.sort_unstable();
    println!(
        "  median: {:.0} deliveries/s; median p99 {:.3} ms; measured payload lower bound {:.2} MiB",
        throughputs[rounds / 2],
        p99_values[rounds / 2] as f64 / 1_000_000.0,
        payload_bytes as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  scope: mixed stream includes 5% late events up to 15 minutes and 1% duplicates; this is not a 30-minute/20-million-event acceptance run"
    );
    Ok(())
}

fn parse_usize(value: Option<String>, default: usize, name: &str) -> Result<usize, String> {
    value.map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|error| format!("invalid {name}: {error}"))
    })
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("baseline") => {
            run_baseline();
            Ok(())
        }
        Some("demo") => run_demo(),
        Some("benchmark") => {
            let events = parse_usize(args.next(), 100_000, "event count")?;
            let rounds = parse_usize(args.next(), 3, "round count")?;
            run_benchmark(events, rounds)
        }
        _ => Err(
            "usage: fraud-velocity-evaluation <baseline|demo|benchmark [events] [rounds]>"
                .to_string(),
        ),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
