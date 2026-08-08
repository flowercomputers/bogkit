use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use offline_reconciliation::{IngestError, Model, Operation};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("ingest") => {
            let db = required_path(args.next(), "database path")?;
            let input = required_path(args.next(), "JSON Lines batch path")?;
            reject_extra(args)?;
            ingest(&db, &input)
        }
        Some("show") => {
            let db = required_path(args.next(), "database path")?;
            reject_extra(args)?;
            show(&db)
        }
        Some("demo") => {
            let db = required_path(args.next(), "database path")?;
            reject_extra(args)?;
            demo(&db)
        }
        Some("benchmark") => {
            let db = required_path(args.next(), "database path")?;
            let count = args
                .next()
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(20_000);
            reject_extra(args)?;
            benchmark(&db, count)
        }
        _ => {
            eprintln!(
                "usage:\n  offline-reconciliation ingest <db-dir> <batch.jsonl>\n  \
                 offline-reconciliation show <db-dir>\n  \
                 offline-reconciliation demo <db-dir>\n  \
                 offline-reconciliation benchmark <db-dir> [operation-count]"
            );
            Err("missing or unknown command".into())
        }
    }
}

fn required_path(value: Option<String>, description: &str) -> Result<PathBuf, String> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {description}"))
}

fn reject_extra(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    match args.next() {
        Some(extra) => Err(format!("unexpected argument: {extra}")),
        None => Ok(()),
    }
}

fn ingest(db: &Path, input: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let operations = read_json_lines(input)?;
    let mut model = Model::open(db);
    let report = model.ingest_batch(&operations)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    print_snapshot(&model)
}

fn show(db: &Path) -> Result<(), Box<dyn std::error::Error>> {
    print_snapshot(&Model::open(db))
}

fn demo(db: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut model = Model::open(db);
    let first = vec![
        operation("scanner-01", 1, "pallet-7", "arrive", "dock", "maya"),
        operation("scanner-01", 2, "pallet-7", "move", "cold-1", "maya"),
    ];
    let concurrent = vec![
        operation("scanner-02", 1, "pallet-7", "move", "freezer-9", "liam"),
        operation("scanner-03", 1, "pallet-8", "arrive", "dock", "noor"),
    ];

    let first_report = model.ingest_batch(&first)?;
    println!("first upload: {}", serde_json::to_string(&first_report)?);
    let replay_report = model.ingest_batch(&first)?;
    println!("exact replay: {}", serde_json::to_string(&replay_report)?);

    match model.simulate_interrupted_batch(&concurrent, 1) {
        Err(IngestError::SimulatedInterruption { .. }) => {
            println!(
                "interrupted upload: rolled back; operation_count={}",
                model.snapshot().operation_count
            );
        }
        Err(error) => return Err(error.into()),
        Ok(()) => unreachable!("demo requested a valid interruption"),
    }

    let retry_report = model.ingest_batch(&concurrent)?;
    println!(
        "whole-batch retry: {}",
        serde_json::to_string(&retry_report)?
    );
    print_snapshot(&model)
}

fn benchmark(db: &Path, count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let operations: Vec<_> = (0..count)
        .map(|index| {
            let device = index % 40;
            let sequence = (index / 40 + 1) as u64;
            operation(
                &format!("scanner-{device:02}"),
                sequence,
                &format!("pallet-{:04}", index % 1_000),
                "move",
                &format!("zone-{:02}", (index / 1_000) % 20),
                &format!("operator-{device:02}"),
            )
        })
        .collect();
    let mut model = Model::open(db);
    let started = Instant::now();
    let report = model.ingest_batch(&operations)?;
    let snapshot = model.snapshot();
    let elapsed = started.elapsed();

    println!(
        "received={} inserted={} duplicates={} stored={} pallets={} elapsed_ms={}",
        report.received,
        report.inserted,
        report.duplicate_replays,
        snapshot.operation_count,
        snapshot.pallets.len(),
        elapsed.as_millis()
    );
    if elapsed.as_secs_f64() >= 5.0 {
        return Err(format!("performance target missed: {elapsed:?}").into());
    }
    Ok(())
}

fn read_json_lines(path: &Path) -> Result<Vec<Operation>, Box<dyn std::error::Error>> {
    let reader = BufReader::new(File::open(path)?);
    let mut operations = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let operation = serde_json::from_str(&line)
            .map_err(|error| format!("{}:{}: {error}", path.display(), index + 1))?;
        operations.push(operation);
    }
    Ok(operations)
}

fn print_snapshot(model: &Model) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(&model.snapshot())?);
    Ok(())
}

fn operation(
    device_id: &str,
    sequence: u64,
    pallet_id: &str,
    action: &str,
    location: &str,
    operator: &str,
) -> Operation {
    Operation {
        device_id: device_id.to_string(),
        sequence,
        pallet_id: pallet_id.to_string(),
        action: action.to_string(),
        location: location.to_string(),
        operator: operator.to_string(),
        device_timestamp_ms: 1_700_000_000_000 + sequence as i64,
    }
}
