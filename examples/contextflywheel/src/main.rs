use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use contextflywheel::{
    DEFAULT_TOKEN_BUDGET, EventKind, Ledger, MissionRecord, load_context_snapshots, parse_kind,
};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "contextflywheel",
    about = "Versioned working memory for autonomous agents"
)]
struct Cli {
    #[arg(long, env = "CONTEXTFLYWHEEL_HOME", default_value = ".contextflywheel")]
    data: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(name = "init")]
    Init {
        #[arg(long)]
        objective: String,
        #[arg(long)]
        constraint: Vec<String>,
        #[arg(long, default_value = "fixture-only; no production access")]
        permission: String,
    },
    #[command(name = "context_record_evidence", visible_alias = "record")]
    Record {
        #[arg(long)]
        kind: String,
        #[arg(long)]
        text: String,
    },
    #[command(name = "context_revise_belief", visible_alias = "belief")]
    Belief {
        #[arg(long)]
        key: String,
        #[arg(long)]
        statement: String,
        #[arg(long)]
        confidence: f32,
        #[arg(long, required = true)]
        evidence: Vec<String>,
        #[arg(long)]
        expected_version: Option<u64>,
    },
    #[command(name = "context_record_failure", visible_alias = "failure")]
    Failure {
        #[arg(long)]
        text: String,
    },
    #[command(name = "context_get", visible_alias = "context")]
    Context {
        #[arg(long, default_value = "")]
        query: String,
        #[arg(long, default_value_t = DEFAULT_TOKEN_BUDGET)]
        max_tokens: usize,
        #[arg(long)]
        hybrid: bool,
        #[arg(long)]
        rank: bool,
    },
    #[command(name = "context_history", visible_alias = "history")]
    History {
        #[arg(long)]
        step: u64,
    },
    Timeline,
    Hybrid {
        #[arg(long)]
        query: String,
        #[arg(long, default_value_t = 30)]
        candidates: usize,
        #[arg(long)]
        rank: bool,
    },
    Serve {
        #[arg(long, default_value = "127.0.0.1:8787")]
        bind: String,
    },
    DemoSeed,
    Hook {
        #[arg(long)]
        event: String,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut ledger = Ledger::open(&cli.data)?;
    match cli.command {
        Command::Init {
            objective,
            constraint,
            permission,
        } => {
            ledger.append(MissionRecord::plain(EventKind::Objective, objective))?;
            for text in constraint {
                ledger.append(MissionRecord::plain(EventKind::Constraint, text))?;
            }
            ledger.append(MissionRecord::plain(EventKind::Permission, permission))?;
            println!("initialized {}", cli.data.display());
        }
        Command::Record { kind, text } => {
            print_record(ledger.append(MissionRecord::plain(parse_kind(&kind)?, text))?)
        }
        Command::Belief {
            key,
            statement,
            confidence,
            evidence,
            expected_version,
        } => print_record(ledger.revise_belief(
            &key,
            &statement,
            confidence,
            &evidence,
            expected_version,
        )?),
        Command::Failure { text } => {
            print_record(ledger.append(MissionRecord::plain(EventKind::FailedApproach, text))?)
        }
        Command::Context {
            query,
            max_tokens,
            hybrid,
            rank,
        } => {
            if hybrid {
                print!(
                    "{}",
                    ledger.render_hybrid_context(
                        &query,
                        max_tokens,
                        rank,
                        std::env::var("ANTHROPIC_BASE_URL").ok().as_deref(),
                        std::env::var("ANTHROPIC_API_KEY").ok().as_deref(),
                        &std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "sonnet".into())
                    )
                )
            } else {
                print!("{}", ledger.render_context(&query, max_tokens))
            }
        }
        Command::History { step } => print!("{}", ledger.render_history(step)?),
        Command::Timeline => {
            for r in ledger.records() {
                println!("Step {:>3}  {:?}: {}", r.step, r.kind, r.text);
            }
        }
        Command::Hybrid {
            query,
            candidates,
            rank,
        } => {
            let candidates = ledger.hybrid_candidates(&query, candidates);
            if rank {
                let result = contextflywheel::ranker::select(
                    &ledger,
                    &query,
                    &candidates,
                    std::env::var("ANTHROPIC_BASE_URL").ok().as_deref(),
                    std::env::var("ANTHROPIC_API_KEY").ok().as_deref(),
                    &std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "sonnet".into()),
                );
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&candidates)?);
            }
        }
        Command::Serve { bind } => {
            let root = ledger.root().to_path_buf();
            drop(ledger);
            contextflywheel::server::serve(root, &bind)?
        }
        Command::DemoSeed => seed_demo(&mut ledger)?,
        Command::Hook { event } => run_hook(&mut ledger, &event)?,
    }
    Ok(())
}

fn print_record(record: MissionRecord) {
    println!(
        "step={} id={} kind={:?}",
        record.step, record.id, record.kind
    );
}

fn seed_demo(ledger: &mut Ledger) -> Result<()> {
    if !ledger.records().is_empty() {
        bail!("demo-seed requires an empty ledger")
    }
    ledger.append(MissionRecord::plain(
        EventKind::Objective,
        "Diagnose and fix the timeout in the controlled fixture",
    ))?;
    ledger.append(MissionRecord::plain(
        EventKind::Constraint,
        "Work only inside fixture/; do not access production",
    ))?;
    ledger.append(MissionRecord::plain(
        EventKind::Permission,
        "Read and modify the disposable fixture; run local tests",
    ))?;
    let symptom = ledger.append(MissionRecord::plain(
        EventKind::Observation,
        "Timeout occurs while loading an account; database is initially suspicious",
    ))?;
    ledger.revise_belief(
        "timeout-cause",
        "The database query is causing the timeout",
        0.55,
        &[symptom.id],
        None,
    )?;
    let profile = ledger.append(MissionRecord::plain(
        EventKind::ToolResult,
        "Profiler: database query 20ms; retry backoff waits 500ms and exhausts the 200ms deadline",
    ))?;
    ledger.revise_belief(
        "timeout-cause",
        "The retry-backoff loop is causing the timeout",
        0.92,
        &[profile.id],
        Some(1),
    )?;
    ledger.append(MissionRecord::plain(
        EventKind::FailedApproach,
        "Adding a database index did not improve end-to-end latency",
    ))?;
    println!("seeded {} records", ledger.records().len());
    Ok(())
}

fn run_hook(ledger: &mut Ledger, event: &str) -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let input: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw).context("parse Claude Code hook JSON")?
    };
    let text = hook_text(event, &input);
    let kind = match event {
        "UserPromptSubmit" => EventKind::UserPrompt,
        "PreToolUse" => EventKind::ToolCall,
        "PostToolUse" => EventKind::ToolResult,
        "PostToolUseFailure" => EventKind::ToolFailure,
        "PreCompact" => EventKind::Compaction,
        "Stop" | "SessionEnd" => EventKind::AgentStop,
        _ => EventKind::Checkpoint,
    };
    let recorded = if text.is_empty() {
        None
    } else {
        Some(ledger.append(MissionRecord::plain(kind, text.clone()))?)
    };
    if event == "UserPromptSubmit" {
        inject_compiled_context(
            ledger,
            "UserPromptSubmit",
            &text,
            recorded.as_ref().map_or(0, |record| record.step),
        )?;
    } else {
        if event == "PreToolUse" {
            ledger.maybe_record_redirect(&text)?;
        }
        if event == "PostToolUse" {
            let latest_belief_step = ledger
                .records()
                .iter()
                .rev()
                .find(|record| record.kind == EventKind::BeliefRevision)
                .map(|record| record.step);
            let latest_snapshot_step = load_context_snapshots(ledger.root())?
                .last()
                .map(|snapshot| snapshot.event_step)
                .unwrap_or(0);
            if latest_belief_step.is_some_and(|step| step > latest_snapshot_step) {
                inject_compiled_context(
                    ledger,
                    "PostToolUse",
                    &text,
                    recorded.as_ref().map_or(0, |record| record.step),
                )?;
                return Ok(());
            }
        }
        println!("{}", json!({"continue": true}));
    }
    Ok(())
}

fn inject_compiled_context(
    ledger: &mut Ledger,
    hook_event: &str,
    query: &str,
    request_step: u64,
) -> Result<()> {
    let hybrid = std::env::var("CONTEXTFLYWHEEL_MODE").is_ok_and(|v| v == "hybrid");
    let rank = std::env::var("CONTEXTFLYWHEEL_RANK").is_ok_and(|v| v == "1");
    let model = std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "sonnet".into());
    let context = if hybrid {
        ledger.render_hybrid_context(
            query,
            DEFAULT_TOKEN_BUDGET,
            rank,
            std::env::var("ANTHROPIC_BASE_URL").ok().as_deref(),
            std::env::var("ANTHROPIC_API_KEY").ok().as_deref(),
            &model,
        )
    } else {
        ledger.render_context(query, DEFAULT_TOKEN_BUDGET)
    };
    let snapshot = ledger.record_context_snapshot(
        request_step,
        &context,
        if hybrid { "hybrid" } else { "deterministic" },
        rank,
        &model,
        hook_event,
    )?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "hookSpecificOutput": {
                "hookEventName": hook_event,
                "additionalContext": snapshot.context
            }
        }))?
    );
    Ok(())
}

fn hook_text(event: &str, input: &Value) -> String {
    match event {
        "UserPromptSubmit" => input
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("user prompt")
            .to_owned(),
        "PreToolUse" => format!(
            "{} {}",
            input
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("tool"),
            input.get("tool_input").cloned().unwrap_or(Value::Null)
        ),
        "PostToolUse" => format!(
            "{} result: {}",
            input
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("tool"),
            input.get("tool_response").cloned().unwrap_or(Value::Null)
        ),
        "PostToolUseFailure" => format!(
            "{} failed: {}",
            input
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("tool"),
            input.get("error").cloned().unwrap_or(Value::Null)
        ),
        _ => event.to_owned(),
    }
}
