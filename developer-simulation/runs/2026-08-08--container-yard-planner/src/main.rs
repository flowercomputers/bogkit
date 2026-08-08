use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use container_yard_planner::{demo_case, planner, run_plan_files, simulator, verification};
use serde::Serialize;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("plan") => {
            if args.len() != 5 {
                return Err(usage());
            }
            let (output, destination) = run_plan_files(
                Path::new(&args[2]),
                Path::new(&args[3]),
                Path::new(&args[4]),
                Duration::from_secs(10),
            )?;
            println!(
                "{} -> {}",
                match output {
                    container_yard_planner::model::PlanOutput::Moves(_) => "executable plan",
                    container_yard_planner::model::PlanOutput::Review(_) => "review required",
                },
                destination.display()
            );
            Ok(())
        }
        Some("verify") => {
            if args.len() != 5 {
                return Err(usage());
            }
            let (yard, wave) =
                container_yard_planner::read_inputs(Path::new(&args[2]), Path::new(&args[3]))?;
            let bytes = std::fs::read(&args[4])
                .map_err(|error| format!("cannot read {}: {error}", args[4]))?;
            let output: container_yard_planner::model::MovesOutput = serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid moves JSON: {error}"))?;
            verification::verify_moves_output(&yard, &wave, &output)?;
            println!(
                "verified {} legal moves and their executable metadata",
                output.moves.len()
            );
            Ok(())
        }
        Some("demo") => demo(),
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "usage:\n  container-yard-planner plan <yard.json> <pickups.json> <output-dir>\n  container-yard-planner verify <yard.json> <pickups.json> <moves.json>\n  container-yard-planner demo".to_string()
}

#[derive(Serialize)]
struct DemoSummary {
    baseline_relocations: usize,
    bounded_lookahead_relocations: usize,
    improvement_percent: usize,
    planner_replay_verified: bool,
    deterministic: bool,
}

fn demo() -> Result<(), String> {
    let (yard, wave) = demo_case(0);
    let timeout = Duration::from_secs(10);
    let baseline = planner::baseline(&yard, &wave, timeout);
    let planned = planner::plan(&yard, &wave, timeout);
    let baseline_relocations = baseline
        .relocations()
        .ok_or_else(|| "demo baseline unexpectedly failed".to_string())?;
    let bounded_lookahead_relocations = planned
        .relocations()
        .ok_or_else(|| "demo planner unexpectedly failed".to_string())?;
    let container_yard_planner::model::PlanOutput::Moves(moves) = &planned else {
        return Err("demo planner did not return moves".to_string());
    };
    simulator::replay(&yard, &wave, &moves.moves)?;
    let first = planned
        .canonical_json()
        .map_err(|error| error.to_string())?;
    let second = planner::plan(&yard, &wave, timeout)
        .canonical_json()
        .map_err(|error| error.to_string())?;
    let improvement_percent =
        100 * (baseline_relocations - bounded_lookahead_relocations) / baseline_relocations;
    let summary = DemoSummary {
        baseline_relocations,
        bounded_lookahead_relocations,
        improvement_percent,
        planner_replay_verified: true,
        deterministic: first == second,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).map_err(|error| error.to_string())?
    );
    Ok(())
}
