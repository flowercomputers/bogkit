mod archive;
mod fixtures;
mod preflight;
mod sha256;

use std::env;
use std::path::{Path, PathBuf};

use preflight::Report;

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        return Err(usage());
    };
    match command.to_string_lossy().as_ref() {
        "generate-fixtures" => {
            let output = arguments.next().ok_or_else(usage)?;
            let include_huge = arguments.any(|argument| argument == "--include-huge");
            fixtures::generate(Path::new(&output), include_huge)
                .map_err(|error| error.to_string())?;
            println!("generated fixtures in {}", Path::new(&output).display());
            Ok(())
        }
        "check" => check_command(&arguments.collect::<Vec<_>>()),
        "demo" => demo_command(&arguments.collect::<Vec<_>>()),
        _ => Err(usage()),
    }
}

fn check_command(arguments: &[std::ffi::OsString]) -> Result<(), String> {
    let bundle = arguments.first().ok_or_else(usage).map(PathBuf::from)?;
    let mut tools = None;
    let mut staging = None;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].to_string_lossy().as_ref() {
            "--tools" => {
                index += 1;
                tools = arguments.get(index).map(PathBuf::from);
            }
            "--staging" => {
                index += 1;
                staging = arguments.get(index).map(PathBuf::from);
            }
            other => return Err(format!("unknown argument {other:?}\n{}", usage())),
        }
        index += 1;
    }
    let tools = tools.ok_or_else(usage)?;
    let report = preflight::check(&bundle, &tools, staging.as_deref());
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    if report.ready {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn demo_command(arguments: &[std::ffi::OsString]) -> Result<(), String> {
    if arguments.len() != 2 {
        return Err(usage());
    }
    let fixtures = PathBuf::from(&arguments[0]);
    let staging = PathBuf::from(&arguments[1]);
    let tools = fixtures.join("tools.json");
    let expected = [
        ("valid.zip", true),
        ("truncated.zip", false),
        ("checksum-mismatch.zip", false),
        ("undeclared.zip", false),
        ("missing.zip", false),
        ("duplicate-case.zip", false),
        ("absolute-path.zip", false),
        ("parent-traversal.zip", false),
        ("missing-tool.zip", false),
        ("multi-error.zip", false),
    ];
    let mut reports: Vec<Report> = Vec::new();
    let mut wrong = Vec::new();
    for (name, should_be_ready) in expected {
        let stage = staging.join(name.trim_end_matches(".zip"));
        let report = preflight::check(&fixtures.join(name), &tools, Some(&stage));
        if report.ready != should_be_ready {
            wrong.push(name);
        }
        reports.push(report);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&reports).map_err(|error| error.to_string())?
    );
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected classifications: {}", wrong.join(", ")))
    }
}

fn usage() -> String {
    "usage:\n  cnc-job-bundle-preflight generate-fixtures DIR [--include-huge]\n  cnc-job-bundle-preflight check BUNDLE --tools INVENTORY [--staging DIR]\n  cnc-job-bundle-preflight demo FIXTURE_DIR STAGING_ROOT"
        .to_owned()
}
