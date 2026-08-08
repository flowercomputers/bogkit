use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use homecare_gap_fill::model::{Outcome, Scale, State, UnfilledReason, VisitId, generate};
use homecare_gap_fill::scheduler::{
    CandidateIndex, IncrementalScheduler, Mode, build_schedule, counts, digest,
};
use homecare_gap_fill::store::{load_state, open_store, persist_cancellation, persist_initial};
use homecare_gap_fill::validator::{PublishedAssignments, validate};

const SEED: u64 = 0x5eed_cafe;
const BURST_CHANGES: usize = 200;
const BASELINE_LATENCY_SAMPLES: usize = 12;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--crash-after-commit") {
        let path = args.get(2).ok_or("missing crash database path")?;
        let visit_id = args
            .get(3)
            .ok_or("missing crash visit id")?
            .parse::<VisitId>()
            .map_err(|error| error.to_string())?;
        crash_after_commit(Path::new(path), visit_id);
    }
    run_demo()
}

fn run_demo() -> Result<(), String> {
    let generated_at = Instant::now();
    let seed_state = generate(SEED, Scale::Representative);
    let generation_time = generated_at.elapsed();
    let index = CandidateIndex::new(&seed_state);

    let baseline_started = Instant::now();
    let baseline_initial = build_schedule(&seed_state, &index, Mode::Baseline);
    let baseline_initial_time = baseline_started.elapsed();
    validate(&seed_state, &baseline_initial, None)?;

    let incremental_started = Instant::now();
    let incremental_initial = build_schedule(&seed_state, &index, Mode::ContinuityAware);
    let incremental_initial_time = incremental_started.elapsed();
    validate(&seed_state, &incremental_initial, None)?;

    let cancellation_ids = choose_cancellations(&seed_state, &incremental_initial);
    if cancellation_ids.len() != BURST_CHANGES {
        return Err(format!(
            "needed {BURST_CHANGES} assigned visits for burst, found {}",
            cancellation_ids.len()
        ));
    }

    let (baseline_p95, baseline_preservation) = baseline_latency_sample(
        &seed_state,
        &index,
        &baseline_initial,
        &cancellation_ids[..BASELINE_LATENCY_SAMPLES],
    )?;

    let mut baseline_final_state = seed_state.clone();
    for visit_id in &cancellation_ids {
        baseline_final_state
            .visits
            .get_mut(visit_id)
            .unwrap()
            .canceled = true;
    }
    let baseline_final_started = Instant::now();
    let baseline_final = build_schedule(&baseline_final_state, &index, Mode::Baseline);
    let baseline_final_time = baseline_final_started.elapsed();
    validate(&baseline_final_state, &baseline_final, None)?;

    let demo_path = std::env::current_dir()
        .map_err(|error| error.to_string())?
        .join("target/caregiver-scheduler-demo-state");
    if demo_path.exists() {
        std::fs::remove_dir_all(&demo_path).map_err(|error| error.to_string())?;
    }
    let mut store = open_store(&demo_path);
    persist_initial(&mut store, &seed_state, &incremental_initial);
    store.checkpoint();

    let initial_published = published(&incremental_initial);
    let mut incremental_state = seed_state.clone();
    let mut incremental = IncrementalScheduler::new(&incremental_state, incremental_initial);
    let burst_started = Instant::now();
    let mut incremental_latencies = Vec::with_capacity(cancellation_ids.len());
    for visit_id in &cancellation_ids {
        let started = Instant::now();
        let delta = incremental.cancel(&mut incremental_state, *visit_id)?;
        persist_cancellation(
            &mut store,
            &incremental_state,
            incremental.outcomes(),
            delta,
        );
        store.checkpoint();
        incremental_latencies.push(started.elapsed());
    }
    let burst_time = burst_started.elapsed();
    let incremental_p95 = percentile_95(&incremental_latencies);
    let final_incremental_digest = digest(&incremental_state, incremental.outcomes());
    validate(
        &incremental_state,
        incremental.outcomes(),
        Some((&initial_published, cancellation_ids[0])),
    )?;
    let preservation = preservation_ratio(
        &initial_published,
        incremental.outcomes(),
        &cancellation_ids.iter().copied().collect(),
    );

    let baseline_counts = counts(&baseline_final, &baseline_final_state);
    let incremental_counts = counts(incremental.outcomes(), &incremental_state);
    if incremental_counts.0 < baseline_counts.0 || incremental_counts.1 < baseline_counts.1 {
        return Err(format!(
            "incremental coverage regressed: baseline {baseline_counts:?}, incremental {incremental_counts:?}"
        ));
    }
    if preservation < 0.995 {
        return Err(format!("preservation {preservation:.6} missed 99.5%"));
    }

    let replay_started = Instant::now();
    let mut replay_state = generate(SEED, Scale::Representative);
    let replay_index = CandidateIndex::new(&replay_state);
    let replay_initial = build_schedule(&replay_state, &replay_index, Mode::ContinuityAware);
    let mut replay = IncrementalScheduler::new(&replay_state, replay_initial);
    for visit_id in &cancellation_ids {
        replay.cancel(&mut replay_state, *visit_id)?;
    }
    let replay_digest = digest(&replay_state, replay.outcomes());
    let replay_time = replay_started.elapsed();
    if replay_digest != final_incremental_digest {
        return Err("deterministic replay digest mismatch".to_string());
    }

    drop(store);
    let recovery_started = Instant::now();
    let recovered_store = open_store(&demo_path);
    let (recovered_state, recovered_outcomes, recovered_metrics) = load_state(&recovered_store);
    let recovery_time = recovery_started.elapsed();
    validate(&recovered_state, &recovered_outcomes, None)?;
    if digest(&recovered_state, &recovered_outcomes) != final_incremental_digest {
        return Err("restart recovery digest mismatch".to_string());
    }
    if recovery_time >= Duration::from_secs(30) {
        return Err("restart recovery exceeded 30 seconds".to_string());
    }
    drop(recovered_store);

    let crash_visit = recovered_outcomes
        .iter()
        .find_map(|(id, outcome)| matches!(outcome, Outcome::Assigned(_)).then_some(*id))
        .ok_or("no assignment available for crash harness")?;
    let crash = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
        .arg("--crash-after-commit")
        .arg(&demo_path)
        .arg(crash_visit.to_string())
        .output()
        .map_err(|error| format!("failed to run crash child: {error}"))?;
    if crash.status.success() {
        return Err("crash harness child unexpectedly exited cleanly".to_string());
    }
    let crash_recovery_started = Instant::now();
    let crashed_store = open_store(&demo_path);
    let (crashed_state, crashed_outcomes, crashed_metrics) = load_state(&crashed_store);
    let crash_recovery_time = crash_recovery_started.elapsed();
    if !crashed_state.visits[&crash_visit].canceled {
        return Err("committed cancellation was lost after crash".to_string());
    }
    validate(&crashed_state, &crashed_outcomes, None)?;

    let reason_counts = reason_counts(incremental.outcomes());
    let change_rate = cancellation_ids.len() as f64 / burst_time.as_secs_f64();
    println!(
        "DATASET label=10%-representative caregivers={} visits={} horizon_days=14 generated_ms={:.3}",
        seed_state.caregivers.len(),
        seed_state.visits.len(),
        generation_time.as_secs_f64() * 1_000.0
    );
    println!(
        "BOGKIT component=fold role=atomic-keyed-persistence-and-materialized-counts ese=no-fit anny=no-fit"
    );
    println!(
        "BASELINE initial_ms={:.3} sampled_changes={} sampled_p95_ms={:.3} sampled_mean_preservation_pct={:.4} final_rescan_ms={:.3} filled={} urgent_filled={}/{}",
        baseline_initial_time.as_secs_f64() * 1_000.0,
        BASELINE_LATENCY_SAMPLES,
        baseline_p95.as_secs_f64() * 1_000.0,
        baseline_preservation * 100.0,
        baseline_final_time.as_secs_f64() * 1_000.0,
        baseline_counts.0,
        baseline_counts.1,
        baseline_counts.2,
    );
    println!(
        "INCREMENTAL initial_ms={:.3} burst_changes={} burst_ms={:.3} p95_ms={:.3} throughput_changes_per_s={:.1} preservation_pct={:.4} filled={} urgent_filled={}/{}",
        incremental_initial_time.as_secs_f64() * 1_000.0,
        cancellation_ids.len(),
        burst_time.as_secs_f64() * 1_000.0,
        incremental_p95.as_secs_f64() * 1_000.0,
        change_rate,
        preservation * 100.0,
        incremental_counts.0,
        incremental_counts.1,
        incremental_counts.2,
    );
    println!(
        "VALIDATOR status=ok constraint_violations=0 active_visits={} outcomes={} unfilled_reason_codes={:?}",
        incremental_state
            .visits
            .values()
            .filter(|visit| !visit.canceled)
            .count(),
        incremental.outcomes().len(),
        reason_counts,
    );
    println!(
        "REPLAY status=deterministic digest={final_incremental_digest:016x} replay_ms={:.3}",
        replay_time.as_secs_f64() * 1_000.0
    );
    println!(
        "RESTART status=ok recovery_ms={:.3} caregivers={} active_visits={} assignments={} unfilled={}",
        recovery_time.as_secs_f64() * 1_000.0,
        recovered_metrics.caregivers,
        recovered_metrics.active_visits,
        recovered_metrics.assignments,
        recovered_metrics.unfilled,
    );
    println!(
        "CRASH_RESTART status=ok child_status={} recovery_ms={:.3} committed_visit={} canceled_visits={}",
        crash.status,
        crash_recovery_time.as_secs_f64() * 1_000.0,
        crash_visit,
        crashed_metrics.canceled_visits,
    );
    println!("STATE path={}", demo_path.display());
    Ok(())
}

fn crash_after_commit(path: &Path, visit_id: VisitId) -> ! {
    let mut store = open_store(path);
    let (mut state, outcomes, _) = load_state(&store);
    let mut scheduler = IncrementalScheduler::new(&state, outcomes);
    let delta = scheduler.cancel(&mut state, visit_id).unwrap();
    persist_cancellation(&mut store, &state, scheduler.outcomes(), delta);
    store.checkpoint();
    eprintln!("crash harness: committed visit {visit_id}, aborting now");
    std::process::abort();
}

fn choose_cancellations(state: &State, outcomes: &BTreeMap<VisitId, Outcome>) -> Vec<VisitId> {
    outcomes
        .iter()
        .filter_map(|(id, outcome)| {
            let visit = &state.visits[id];
            (matches!(outcome, Outcome::Assigned(_)) && visit.start >= state.visits[&0].start + 360)
                .then_some(*id)
        })
        .take(BURST_CHANGES)
        .collect()
}

fn baseline_latency_sample(
    seed_state: &State,
    index: &CandidateIndex,
    initial: &BTreeMap<VisitId, Outcome>,
    cancellations: &[VisitId],
) -> Result<(Duration, f64), String> {
    let mut state = seed_state.clone();
    let mut schedule = initial.clone();
    let mut latencies = Vec::new();
    let mut preservation_sum = 0.0;
    for visit_id in cancellations {
        let before = published(&schedule);
        state.visits.get_mut(visit_id).unwrap().canceled = true;
        let started = Instant::now();
        schedule = build_schedule(&state, index, Mode::Baseline);
        latencies.push(started.elapsed());
        validate(&state, &schedule, None)?;
        preservation_sum += preservation_ratio(&before, &schedule, &BTreeSet::from([*visit_id]));
    }
    Ok((
        percentile_95(&latencies),
        preservation_sum / cancellations.len() as f64,
    ))
}

fn published(outcomes: &BTreeMap<VisitId, Outcome>) -> PublishedAssignments {
    outcomes
        .iter()
        .filter_map(|(visit, outcome)| match outcome {
            Outcome::Assigned(caregiver) => Some((*visit, *caregiver)),
            Outcome::Unfilled(_) => None,
        })
        .collect()
}

fn preservation_ratio(
    before: &PublishedAssignments,
    after: &BTreeMap<VisitId, Outcome>,
    affected: &BTreeSet<VisitId>,
) -> f64 {
    let mut eligible = 0;
    let mut preserved = 0;
    for (visit_id, caregiver_id) in before {
        if affected.contains(visit_id) {
            continue;
        }
        eligible += 1;
        if after.get(visit_id) == Some(&Outcome::Assigned(*caregiver_id)) {
            preserved += 1;
        }
    }
    if eligible == 0 {
        1.0
    } else {
        preserved as f64 / eligible as f64
    }
}

fn percentile_95(samples: &[Duration]) -> Duration {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted[((sorted.len() * 95).div_ceil(100)).saturating_sub(1)]
}

fn reason_counts(outcomes: &BTreeMap<VisitId, Outcome>) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for outcome in outcomes.values() {
        if let Outcome::Unfilled(reason) = outcome {
            *counts.entry(reason.code()).or_default() += 1;
        }
    }
    for reason in [
        UnfilledReason::NoCertification,
        UnfilledReason::NoRegionCoverage,
        UnfilledReason::OutsideAvailability,
        UnfilledReason::RequiredRest,
        UnfilledReason::HourLimit,
        UnfilledReason::TravelConflict,
    ] {
        counts.entry(reason.code()).or_insert(0);
    }
    counts
}
