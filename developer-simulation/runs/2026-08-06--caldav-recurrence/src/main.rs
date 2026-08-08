use std::path::PathBuf;

use caldav_recurrence_prototype::{Config, format_diagnostics, run};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return;
    }
    let config = match parse_args(&args[1..]) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    };
    match run(&config) {
        Ok(result) => println!("{}", format_diagnostics(&result)),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_args(args: &[String]) -> Result<Config, String> {
    let mut events = None;
    let mut transitions = None;
    let mut from = None;
    let mut to = None;
    let mut output = None;
    let mut state_dir = None;
    let mut edits = None;
    let mut crash_after_uid = None;

    let mut index = 0;
    while index < args.len() {
        let name = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value for {name}"))?;
        let slot = match name.as_str() {
            "--events" => &mut events,
            "--transitions" => &mut transitions,
            "--from" => &mut from,
            "--to" => &mut to,
            "--output" => &mut output,
            "--state-dir" => &mut state_dir,
            "--edits" => &mut edits,
            "--crash-after" => &mut crash_after_uid,
            _ => return Err(format!("unknown argument {name}")),
        };
        if slot.is_some() {
            return Err(format!("argument {name} was supplied twice"));
        }
        *slot = Some(value.clone());
        index += 2;
    }

    Ok(Config {
        events: required_path(events, "--events")?,
        transitions: required_path(transitions, "--transitions")?,
        from: required_string(from, "--from")?,
        to: required_string(to, "--to")?,
        output: required_path(output, "--output")?,
        state_dir: required_path(state_dir, "--state-dir")?,
        edits: edits.map(PathBuf::from),
        crash_after_uid,
    })
}

fn required_path(value: Option<String>, name: &str) -> Result<PathBuf, String> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing required argument {name}"))
}

fn required_string(value: Option<String>, name: &str) -> Result<String, String> {
    value.ok_or_else(|| format!("missing required argument {name}"))
}

fn print_help() {
    println!(
        "caldav-recurrence-prototype\n\n\
         Usage:\n  caldav-recurrence-prototype --events EVENTS.jsonl --transitions ZONES.json \\\n         --from RFC3339 --to RFC3339 --output OCCURRENCES.jsonl --state-dir STATE [--edits EDITS.jsonl]\n\n\
         Event JSONL supports timed and all_day events, DAILY/WEEKLY/MONTHLY\n\
         rules, EXDATE values, and confirmed/cancelled occurrence overrides.\n\
         Diagnostics are emitted as one non-sensitive JSON object on stdout."
    );
}
