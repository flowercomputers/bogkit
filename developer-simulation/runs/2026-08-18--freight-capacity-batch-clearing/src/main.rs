use std::{path::PathBuf, time::Instant};

use freight_clearing::{FailurePoint, FixtureSpec, RunPaths, generate_fixture, run_files};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().ok_or_else(usage)?;
    match command.as_str() {
        "clear" => {
            let paths = RunPaths {
                orders: PathBuf::from(arguments.next().ok_or_else(usage)?),
                manifest: PathBuf::from(arguments.next().ok_or_else(usage)?),
                proposal: PathBuf::from(arguments.next().ok_or_else(usage)?),
            };
            if arguments.next().is_some() {
                return Err(usage());
            }
            let result =
                run_files(&paths, FailurePoint::None).map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|error| error.to_string())?
            );
        }
        "generate" | "benchmark" => {
            let directory = PathBuf::from(arguments.next().ok_or_else(usage)?);
            let remaining: Vec<String> = arguments.collect();
            let spec = parse_fixture_spec(&remaining)?;
            let fixture = generate_fixture(&directory, spec)?;
            if command == "generate" {
                println!(
                    "{}",
                    serde_json::to_string(&fixture.manifest).map_err(|error| error.to_string())?
                );
            } else {
                let started = Instant::now();
                let result = run_files(&fixture.paths, FailurePoint::None)
                    .map_err(|error| error.to_string())?;
                let elapsed_ms = u64::try_from(started.elapsed().as_millis())
                    .map_err(|_| "elapsed duration does not fit u64".to_owned())?;
                let verified_gates_pass = elapsed_ms <= 20_000
                    && result.proposal_bytes <= 160 * 1024 * 1024
                    && result.fill_count <= fixture.manifest.max_fill_count;
                let measured_acceptance_pass = result
                    .peak_resident_memory_bytes
                    .map(|peak| verified_gates_pass && peak <= 512 * 1024 * 1024);
                let output = serde_json::json!({
                    "order_count": result.order_count,
                    "market_count": result.market_count,
                    "buy_count": fixture.manifest.buy_count,
                    "sell_count": fixture.manifest.sell_count,
                    "fill_count": result.fill_count,
                    "maximum_fill_count": fixture.manifest.max_fill_count,
                    "gross_value_cents": result.gross_value_cents,
                    "elapsed_ms": elapsed_ms,
                    "peak_resident_memory_bytes": result.peak_resident_memory_bytes,
                    "peak_resident_memory_note": "Linux reads VmHWM; other hosts require the documented OS harness",
                    "scratch_bytes": result.proposal_bytes,
                    "scratch_files_open_max": 1,
                    "proposal_bytes": result.proposal_bytes,
                    "proposal_sha256": result.proposal_sha256,
                    "verified_gates_pass": verified_gates_pass,
                    "measured_acceptance_pass": measured_acceptance_pass,
                    "acceptance_status": "target Linux, Ruby baseline, and peak-RSS gates not available on this host",
                    "host_os": std::env::consts::OS,
                });
                println!("{output}");
            }
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn parse_fixture_spec(arguments: &[String]) -> Result<FixtureSpec, String> {
    if arguments.is_empty() {
        return Ok(FixtureSpec {
            market_count: 5_000,
            buy_count: 400_000,
            sell_count: 250_000,
            seed: 0x5eed_2026_0818,
        });
    }
    if arguments.len() != 4 {
        return Err(usage());
    }
    Ok(FixtureSpec {
        market_count: arguments[0]
            .parse()
            .map_err(|_| "invalid market count".to_owned())?,
        buy_count: arguments[1]
            .parse()
            .map_err(|_| "invalid buy count".to_owned())?,
        sell_count: arguments[2]
            .parse()
            .map_err(|_| "invalid sell count".to_owned())?,
        seed: arguments[3]
            .parse()
            .map_err(|_| "invalid seed".to_owned())?,
    })
}

fn usage() -> String {
    "usage: freight-clearing clear ORDERS MANIFEST PROPOSAL | generate DIR [MARKETS BUYS SELLS SEED] | benchmark DIR [MARKETS BUYS SELLS SEED]".to_owned()
}
