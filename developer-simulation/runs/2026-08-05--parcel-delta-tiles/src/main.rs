use std::fs::File;
use std::io::{self, BufReader, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| "-".to_string());
    if args.next().is_some() {
        return Err("usage: parcel-delta-tiles [INPUT.ndjson|-]".to_string());
    }

    let tiles = if input == "-" {
        let stdin = io::stdin();
        parcel_delta_tiles::plan(BufReader::new(stdin.lock()))?
    } else {
        let file = File::open(&input).map_err(|error| format!("cannot open {input}: {error}"))?;
        parcel_delta_tiles::plan(BufReader::new(file))?
    };

    // Nothing is written until the entire input has parsed and validated.
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for line in parcel_delta_tiles::format_plan(&tiles) {
        writeln!(output, "{line}").map_err(|error| format!("cannot write plan: {error}"))?;
    }
    Ok(())
}
