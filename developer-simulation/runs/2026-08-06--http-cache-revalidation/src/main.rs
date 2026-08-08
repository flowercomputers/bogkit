use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use http_cache_revalidation::{
    Event, ReferenceEngine, WorkloadReport, baseline_reproduction, parse_trace, run_shape_workload,
};

const MEMORY_LIMIT_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct TimeVal {
    seconds: i64,
    microseconds: i32,
    padding: i32,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct Usage {
    user_time: TimeVal,
    system_time: TimeVal,
    max_rss: i64,
    integral_shared_memory: i64,
    integral_unshared_data: i64,
    integral_unshared_stack: i64,
    page_reclaims: i64,
    page_faults: i64,
    swaps: i64,
    block_input: i64,
    block_output: i64,
    messages_sent: i64,
    messages_received: i64,
    signals_received: i64,
    voluntary_context_switches: i64,
    involuntary_context_switches: i64,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn getrusage(who: i32, usage: *mut Usage) -> i32;
}

#[cfg(target_os = "macos")]
fn max_rss_bytes() -> Option<u64> {
    let mut usage = Usage::default();
    let result = unsafe { getrusage(0, &mut usage) };
    (result == 0).then_some(usage.max_rss as u64)
}

#[cfg(not(target_os = "macos"))]
fn max_rss_bytes() -> Option<u64> {
    None
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("demo") | None => match run_demo() {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::from(1)
            }
        },
        Some("run") => match args.next() {
            Some(path) => match run_file(Path::new(&path)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("error: {message}");
                    ExitCode::from(1)
                }
            },
            None => {
                eprintln!("error: run requires a trace path");
                ExitCode::from(2)
            }
        },
        Some("workload") => match parse_workload_args(args.collect()) {
            Ok((objects, requests, purges)) => {
                let report = run_shape_workload(objects, requests, purges);
                print_workload(&report, max_rss_bytes());
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("error: {message}");
                ExitCode::from(2)
            }
        },
        Some("help") | Some("--help") | Some("-h") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(_) => {
            eprintln!("error: unknown command; use help");
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("http-cache-revalidation demo");
    println!("  demo                         run the baseline comparison and reference trace");
    println!("  run <trace>                  run a line-oriented reference trace");
    println!("  workload [objects requests purges]  run the compact shape workload");
}

fn run_demo() -> Result<(), String> {
    let baseline = baseline_reproduction();
    println!(
        "mode=baseline wrong_variant={} cross_tenant_collision={} duplicate_revalidations={} purge_left_servable={} unverified_body_after_crash={} missing_body_after_crash={}",
        baseline.wrong_variant_served,
        baseline.cross_tenant_collision,
        baseline.duplicate_revalidations,
        baseline.purge_left_entry_servable,
        baseline.unverified_body_served_after_crash,
        baseline.missing_body_served_after_crash
    );
    let (quota, events) = parse_trace(demo_trace()).map_err(|error| error.to_string())?;
    let mut engine = ReferenceEngine::new(quota);
    run_events(&mut engine, &events)?;
    print_summary(&engine);
    if engine.active_lease_count() != 0 {
        return Err("demo ended with an active lease".to_string());
    }
    if engine.committed_usage_bytes() > engine.quota_bytes() {
        return Err("demo exceeded quota".to_string());
    }
    if !engine.modeled_reference_integrity_holds() {
        return Err("demo recovery invariant failed".to_string());
    }
    println!("demo=PASS modeled_invariants=true");
    Ok(())
}

fn run_file(path: &Path) -> Result<(), String> {
    let input = fs::read_to_string(path).map_err(|_| "could not read trace file".to_string())?;
    let (quota, events) = parse_trace(&input).map_err(|error| error.to_string())?;
    let mut engine = ReferenceEngine::new(quota);
    run_events(&mut engine, &events)?;
    print_summary(&engine);
    Ok(())
}

fn run_events(engine: &mut ReferenceEngine, events: &[Event]) -> Result<(), String> {
    let mut finalized_initial = false;
    for event in events {
        if !finalized_initial
            && matches!(
                event,
                Event::Request { .. }
                    | Event::Complete { .. }
                    | Event::Purge { .. }
                    | Event::Recover { .. }
            )
        {
            engine.finalize_initial(0);
            finalized_initial = true;
            for decision in engine.drain_decisions() {
                println!("{}", decision.line());
            }
        }
        apply_one(engine, event)?;
        for decision in engine.drain_decisions() {
            println!("{}", decision.line());
        }
    }
    if !finalized_initial {
        engine.finalize_initial(0);
        for decision in engine.drain_decisions() {
            println!("{}", decision.line());
        }
    }
    Ok(())
}

fn apply_one(engine: &mut ReferenceEngine, event: &Event) -> Result<(), String> {
    match event {
        Event::Blob(blob) => engine.add_blob(blob.clone()),
        Event::Entry { at, key, entry } => {
            engine.add_initial_entry(key.clone(), entry.clone(), *at)
        }
        Event::Origin { id, outcome } => engine.add_origin(id.clone(), outcome.clone()),
        Event::Request {
            id,
            at,
            worker,
            key,
            allow_stale_if_error,
            origin_id,
        } => engine.request(
            id.clone(),
            *at,
            worker.clone(),
            key.clone(),
            *allow_stale_if_error,
            origin_id.clone(),
        ),
        Event::Complete {
            request_id,
            at,
            crash,
        } => engine.complete(request_id, *at, *crash)?,
        Event::Purge {
            at,
            tenant,
            seq,
            tag,
        } => engine.purge(*at, tenant.clone(), *seq, tag.clone()),
        Event::Recover { at } => engine.recover(*at),
    }
    Ok(())
}

fn print_summary(engine: &ReferenceEngine) {
    let metrics = &engine.metrics;
    println!(
        "summary requests={} fresh_hits={} stale_responses={} misses={} revalidation_starts={} revalidation_waits={} purges_applied={} purges_ignored={} recovery_rollbacks={} recovery_commits={} quota_evictions={} unsafe_body_serves={} committed_usage_bytes={} quota_bytes={}",
        metrics.requests,
        metrics.fresh_hits,
        metrics.stale_responses,
        metrics.misses,
        metrics.revalidation_starts,
        metrics.revalidation_waits,
        metrics.purges_applied,
        metrics.purges_ignored,
        metrics.recovery_rollbacks,
        metrics.recovery_commits,
        metrics.quota_evictions,
        metrics.unsafe_body_serves,
        engine.committed_usage_bytes(),
        engine.quota_bytes()
    );
    for entry in engine.final_index() {
        println!(
            "event=final_entry tenant_id={} key_id={} body_id={} fresh_until={} stale_until={} body_size={} tag_count={}",
            entry.tenant_id,
            entry.key_id,
            entry.body_id,
            entry.fresh_until,
            entry.stale_until,
            entry.body_size,
            entry.tags.len()
        );
    }
}

fn parse_workload_args(args: Vec<String>) -> Result<(usize, usize, usize), String> {
    if args.len() > 3 {
        return Err("workload accepts at most three numbers".to_string());
    }
    let defaults = [2_000_000usize, 1_000_000usize, 100_000usize];
    let mut values = defaults;
    for (index, value) in args.iter().enumerate() {
        values[index] = value
            .parse()
            .map_err(|_| "invalid workload number".to_string())?;
    }
    Ok((values[0], values[1], values[2]))
}

fn print_workload(report: &WorkloadReport, max_rss: Option<u64>) {
    let max_rss_text = max_rss.map_or_else(|| "-".to_string(), |bytes| bytes.to_string());
    let within_memory = max_rss.is_some_and(|bytes| bytes <= MEMORY_LIMIT_BYTES);
    println!(
        "workload objects={} requests={} purges={} request_hits={} request_misses={} committed_usage_bytes={} quota_bytes={} within_quota={} max_rss_bytes={} memory_limit_bytes={} within_memory={}",
        report.objects,
        report.requests,
        report.purges,
        report.request_hits,
        report.request_misses,
        report.committed_usage_bytes,
        report.quota_bytes,
        report.committed_usage_bytes <= report.quota_bytes,
        max_rss_text,
        MEMORY_LIMIT_BYTES,
        within_memory
    );
}

fn demo_trace() -> &'static str {
    r#"
# The fixture contains labels and URLs, but the CLI output emits only hashes.
quota 1000000
blob body-a 10 verified
blob body-d 10 verified
blob body-e 10 verified
entry tenant-a GET https://cache.test/item en 10 30 body-a 10 0 article etag-a
entry tenant-b GET https://cache.test/profile en 5 15 body-d 10 0 profile etag-d
entry tenant-c GET https://cache.test/crash en 1 100 body-e 10 0 crash etag-e
origin replace modified body-b 11 100 200 article etag-b verified
origin replace-new modified body-c 11 100 200 article etag-c verified
origin origin-error error origin_down
origin crash modified body-f 11 100 200 crash etag-f verified
origin bad modified body-g 11 100 200 crash etag-g unverified
request r1 10 worker-1 tenant-a GET https://cache.test/item EN allow replace
request r2 10 worker-2 tenant-a GET https://cache.test/item en allow replace
purge 11 tenant-a 2 article
complete r1 12 none
purge 13 tenant-a 1 article
purge 14 tenant-a 2 article
request r3 15 worker-3 tenant-a GET https://cache.test/item en allow replace-new
complete r3 16 none
request r4 20 worker-4 tenant-a GET https://cache.test/item en allow replace-new
request r5 10 worker-5 tenant-b GET https://cache.test/profile en allow origin-error
complete r5 11 none
request r6 16 worker-6 tenant-b GET https://cache.test/profile en allow origin-error
complete r6 17 none
request r7 2 worker-7 tenant-c GET https://cache.test/crash en allow crash
complete r7 3 after_metadata
request r8 4 worker-8 tenant-d GET https://cache.test/unverified en allow bad
complete r8 5 after_body
"#
}
