#![forbid(unsafe_code)]

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use lab_unit_gate::{FailPoint, GenerationConfig, InputPaths, generate_fixture, run};
use serde_json::json;

const MAX_PEAK_RSS_BYTES: u64 = 256 * 1024 * 1024;

const fn benchmark_acceptance(peak_rss_bytes: Option<u64>) -> &'static str {
    match peak_rss_bytes {
        Some(bytes) if bytes > MAX_PEAK_RSS_BYTES => "failed",
        Some(_) | None => "unverified",
    }
}

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match execute(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

fn execute(arguments: &[String]) -> Result<(), String> {
    match arguments {
        [command, directory, ordinary, boundary, rejected, seed] if command == "generate" => {
            let config = GenerationConfig {
                seed: parse_number(seed, "seed")?,
                ordinary: parse_number(ordinary, "ordinary")?,
                boundary: parse_number(boundary, "boundary")?,
                rejected: parse_number(rejected, "rejected")?,
            };
            let manifest = generate_fixture(Path::new(directory), config)
                .map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string(&manifest).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        [command, directory, output] if command == "run" => {
            let summary = run(
                &paths(Path::new(directory)),
                Path::new(output),
                FailPoint::None,
            )
            .map_err(|error| error.to_string())?;
            println!(
                "{}",
                json!({
                    "converted": summary.converted,
                    "output_digest": summary.output_sha256,
                    "rejected": summary.rejected,
                    "total": summary.total,
                })
            );
            Ok(())
        }
        [command, directory, output] if command == "bench" => {
            let started = Instant::now();
            let summary = run(
                &paths(Path::new(directory)),
                Path::new(output),
                FailPoint::None,
            )
            .map_err(|error| error.to_string())?;
            let peak_rss_bytes = peak_rss_bytes();
            println!(
                "{}",
                json!({
                    "acceptance": benchmark_acceptance(peak_rss_bytes),
                    "converted": summary.converted,
                    "elapsed_ms": started.elapsed().as_millis(),
                    "missing_gates": [
                        "signed_fixture_parity",
                        "java_comparison",
                        "declared_two_core_linux",
                    ],
                    "output_digest": summary.output_sha256,
                    "peak_rss_bytes": peak_rss_bytes,
                    "rejected": summary.rejected,
                    "run_status": "complete",
                    "total": summary.total,
                })
            );
            Ok(())
        }
        _ => Err("usage: lab-unit-gate generate DIR ORDINARY BOUNDARY REJECTED SEED | run DIR REPORT | bench DIR REPORT".to_string()),
    }
}

fn parse_number<T: std::str::FromStr>(input: &str, name: &str) -> Result<T, String> {
    input.parse().map_err(|_| format!("invalid {name}"))
}

fn paths(directory: &Path) -> InputPaths {
    InputPaths {
        units: directory.join("units.ndjson"),
        analytes: directory.join("analytes.ndjson"),
        mappings: directory.join("mappings.ndjson"),
        observations: directory.join("observations.ndjson"),
    }
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kilobytes = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    kilobytes.checked_mul(1_024)
}

#[cfg(not(target_os = "linux"))]
const fn peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::{MAX_PEAK_RSS_BYTES, benchmark_acceptance};

    #[test]
    fn measured_over_limit_memory_can_never_emit_acceptance_pass() {
        assert_eq!(benchmark_acceptance(Some(MAX_PEAK_RSS_BYTES + 1)), "failed");
        assert_eq!(benchmark_acceptance(None), "unverified");
        assert_eq!(benchmark_acceptance(Some(MAX_PEAK_RSS_BYTES)), "unverified");
    }
}
