use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

use carrier_label_ambiguity::{
    prepare_crash_scenario, resume_crash_scenario, run_crash_child, run_fixture,
};

fn usage() -> &'static str {
    "usage:\n  carrier-label-ambiguity demo --dir PATH\n  carrier-label-ambiguity acceptance --dir PATH [--shipments 20000] [--seeds 30]\n  carrier-label-ambiguity crash-demo --dir PATH"
}

fn option(args: &[String], name: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return Ok(None);
    };
    args.get(index + 1)
        .cloned()
        .ok_or_else(|| format!("{name} requires a value"))
        .map(Some)
}

fn required_dir(args: &[String]) -> Result<PathBuf, String> {
    option(args, "--dir")?
        .map(PathBuf::from)
        .ok_or_else(|| "--dir is required".to_string())
}

fn parse_or<T>(args: &[String], name: &str, default: T) -> Result<T, String>
where
    T: std::str::FromStr,
{
    option(args, name)?.map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|_| format!("invalid {name} value {value:?}"))
    })
}

fn run_and_print(root: &Path, shipments: usize, seeds: u64) -> Result<(), String> {
    let started = Instant::now();
    let metrics = run_fixture(root, shipments, seeds)?;
    let mut total_shipments = 0;
    let mut total_paid = 0;
    let mut total_review = 0;
    let mut total_records = 0;
    let mut total_bytes = 0;
    let mut max_final_at = 0;
    for seed in &metrics {
        println!(
            "seed {:02}: PASS shipments={} purchased={} failed={} needs_review={} ambiguous={} paid_labels={} callbacks={} restarts={} decisions={} max_final_at={}s",
            seed.seed,
            seed.shipments,
            seed.purchased,
            seed.failed,
            seed.needs_review,
            seed.ambiguous_timeouts,
            seed.paid_labels,
            seed.callbacks,
            seed.injected_restarts,
            seed.decision_records,
            seed.max_final_at
        );
        total_shipments += seed.shipments;
        total_paid += seed.paid_labels;
        total_review += seed.needs_review;
        total_records += seed.decision_records;
        total_bytes += seed.journal_bytes;
        max_final_at = max_final_at.max(seed.max_final_at);
    }
    println!(
        "ACCEPTANCE PASS seeds={} shipments={} paid_labels={} needs_review={} decisions={} max_final_at={}s journal_mib={:.2} elapsed_seconds={:.3}",
        metrics.len(),
        total_shipments,
        total_paid,
        total_review,
        total_records,
        max_final_at,
        total_bytes as f64 / 1_048_576.0,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn crash_demo(root: &Path) -> Result<(), String> {
    std::fs::create_dir(root).map_err(|error| format!("create {}: {error}", root.display()))?;
    let executable =
        env::current_exe().map_err(|error| format!("find current executable: {error}"))?;
    for stage in [
        "before-network",
        "after-carrier",
        "after-confirm",
        "after-callback",
    ] {
        let dir = root.join(stage);
        prepare_crash_scenario(&dir)?;
        let status = Command::new(&executable)
            .arg("__crash-child")
            .arg("--dir")
            .arg(&dir)
            .arg("--stage")
            .arg(stage)
            .status()
            .map_err(|error| format!("run crash child: {error}"))?;
        if status.code() != Some(86) {
            return Err(format!(
                "crash child for {stage} exited with {status}, expected code 86"
            ));
        }
        let metrics = resume_crash_scenario(&dir, stage)?;
        println!(
            "crash {stage}: PASS final={:?} durable_attempts={} carrier_purchases={}",
            metrics.final_state, metrics.attempts, metrics.carrier_purchases
        );
    }
    println!("CRASH/RESTART PASS scenarios=4 automatic_retries=0");
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().ok_or_else(|| usage().to_string())?;
    match command.as_str() {
        "demo" => run_and_print(&required_dir(&args)?, 100, 1),
        "acceptance" => {
            let root = required_dir(&args)?;
            let shipments = parse_or(&args, "--shipments", 20_000_usize)?;
            let seeds = parse_or(&args, "--seeds", 30_u64)?;
            run_and_print(&root, shipments, seeds)
        }
        "crash-demo" => crash_demo(&required_dir(&args)?),
        "__crash-child" => {
            let dir = required_dir(&args)?;
            let stage = option(&args, "--stage")?
                .ok_or_else(|| "--stage is required for crash child".to_string())?;
            run_crash_child(&dir, &stage)?;
            std::process::exit(86);
        }
        _ => Err(usage().to_string()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
