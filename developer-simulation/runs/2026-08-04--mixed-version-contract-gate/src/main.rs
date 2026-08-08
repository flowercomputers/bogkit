use std::env;
use std::process::ExitCode;

use mixed_version_contract_gate::{GateStatus, run_files};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 4 {
        eprintln!(
            "usage: contract-gate <contracts.json> <topology.json> <fleet.json> <candidate.json>"
        );
        return ExitCode::from(2);
    }

    let result = run_files(&args[0], &args[1], &args[2], &args[3]);
    println!(
        "{}",
        serde_json::to_string_pretty(&result).expect("result is serializable")
    );
    match result.status {
        GateStatus::Allow => ExitCode::SUCCESS,
        GateStatus::Block => ExitCode::from(1),
        GateStatus::ReviewRequired => ExitCode::from(2),
    }
}
