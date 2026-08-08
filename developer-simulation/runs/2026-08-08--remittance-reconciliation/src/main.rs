use std::env;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use remittance_reconciliation::engine::{greedy_baseline, reconcile};
use remittance_reconciliation::fixture::{generate_fixture, shuffle_inputs};
use remittance_reconciliation::verify::verify_results;

type AnyError = Box<dyn Error + Send + Sync>;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), AnyError> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().map(String::as_str) else {
        print_help();
        return Err("a command is required".into());
    };
    match command {
        "generate" => {
            let out = required_path(&args, "--out")?;
            let claim_count = optional_usize(&args, "--claim-count", 62_000)?;
            let remittance_count = optional_usize(&args, "--remittance-count", 50_000)?;
            let started = Instant::now();
            generate_fixture(&out, claim_count, remittance_count)?;
            println!(
                "generated {claim_count} claim records and {remittance_count} remittance records in {:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
        "reconcile" => {
            let claims = required_path(&args, "--claims")?;
            let remittances = required_path(&args, "--remittances")?;
            let out = required_path(&args, "--out")?;
            let started = Instant::now();
            let summary = reconcile(&claims, &remittances, &out)?;
            println!(
                "reconciled {} remittance records: {} accepted, {} review, {} links in {:.3}s",
                summary.input_remittance_records,
                summary.accepted_remittance_lines,
                summary.review_remittance_lines,
                summary.accepted_links,
                started.elapsed().as_secs_f64()
            );
        }
        "baseline" => {
            let claims = required_path(&args, "--claims")?;
            let remittances = required_path(&args, "--remittances")?;
            let out = required_path(&args, "--out")?;
            let started = Instant::now();
            let summary = greedy_baseline(&claims, &remittances, &out)?;
            println!(
                "baseline processed {} remittance records: {} accepted, {} review, {} links in {:.3}s",
                summary.input_remittance_records,
                summary.accepted_remittance_lines,
                summary.review_remittance_lines,
                summary.accepted_links,
                started.elapsed().as_secs_f64()
            );
        }
        "shuffle" => {
            let claims = required_path(&args, "--claims")?;
            let remittances = required_path(&args, "--remittances")?;
            let out = required_path(&args, "--out")?;
            let seed = required_value(&args, "--seed")?.parse::<u64>()?;
            shuffle_inputs(&claims, &remittances, &out, seed)?;
            println!("wrote deterministic shuffle for seed {seed}");
        }
        "verify" => {
            let claims = required_path(&args, "--claims")?;
            let remittances = required_path(&args, "--remittances")?;
            let truth = required_path(&args, "--ground-truth")?;
            let results = required_path(&args, "--results")?;
            let allow_failure = args.iter().any(|arg| arg == "--allow-failure");
            let report = verify_results(&claims, &remittances, &truth, &results)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed && !allow_failure {
                return Err("verification failed".into());
            }
        }
        "demo" => {
            let work_dir = required_path(&args, "--work-dir")?;
            run_demo(&work_dir)?;
        }
        "help" | "--help" | "-h" => print_help(),
        _ => {
            print_help();
            return Err(format!("unknown command: {command}").into());
        }
    }
    Ok(())
}

fn run_demo(work_dir: &Path) -> Result<(), AnyError> {
    let fixture = work_dir.join("fixture");
    let results = work_dir.join("results");
    let baseline = work_dir.join("baseline");
    generate_fixture(&fixture, 400, 200)?;
    let improved_summary = reconcile(
        &fixture.join("claims.jsonl"),
        &fixture.join("remittances.jsonl"),
        &results,
    )?;
    let improved_report = verify_results(
        &fixture.join("claims.jsonl"),
        &fixture.join("remittances.jsonl"),
        &fixture.join("ground-truth.jsonl"),
        &results,
    )?;
    let baseline_summary = greedy_baseline(
        &fixture.join("claims.jsonl"),
        &fixture.join("remittances.jsonl"),
        &baseline,
    )?;
    let baseline_report = verify_results(
        &fixture.join("claims.jsonl"),
        &fixture.join("remittances.jsonl"),
        &fixture.join("ground-truth.jsonl"),
        &baseline,
    )?;
    println!(
        "demo improved: accepted={} review={} precision={:.4}% recall={:.4}% passed={}",
        improved_summary.accepted_remittance_lines,
        improved_summary.review_remittance_lines,
        improved_report.precision * 100.0,
        improved_report.recall * 100.0,
        improved_report.passed
    );
    println!(
        "demo greedy baseline: accepted={} review={} precision={:.4}% recall={:.4}% passed={}",
        baseline_summary.accepted_remittance_lines,
        baseline_summary.review_remittance_lines,
        baseline_report.precision * 100.0,
        baseline_report.recall * 100.0,
        baseline_report.passed
    );
    if !improved_report.passed {
        return Err("the improved demo did not pass verification".into());
    }
    Ok(())
}

fn required_path(args: &[String], flag: &str) -> Result<PathBuf, AnyError> {
    Ok(PathBuf::from(required_value(args, flag)?))
}

fn required_value<'a>(args: &'a [String], flag: &str) -> Result<&'a str, AnyError> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|position| args.get(position + 1))
        .map(String::as_str)
        .ok_or_else(|| format!("missing required {flag}").into())
}

fn optional_usize(args: &[String], flag: &str, default: usize) -> Result<usize, AnyError> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|position| args.get(position + 1))
        .map_or(Ok(default), |value| Ok(value.parse()?))
}

fn print_help() {
    println!(
        "remittance-reconciliation\n\
         commands:\n\
           generate --out DIR [--claim-count N] [--remittance-count N]\n\
           reconcile --claims FILE --remittances FILE --out DIR\n\
           baseline --claims FILE --remittances FILE --out DIR\n\
           shuffle --claims FILE --remittances FILE --seed N --out DIR\n\
           verify --claims FILE --remittances FILE --ground-truth FILE --results DIR [--allow-failure]\n\
           demo --work-dir DIR"
    );
}
