//! bog-bench — agent tooling churn, as a live fold view.
//!
//! Every tool call an agent makes is a delta. Fold them into materialized
//! views and the churn numbers are always current: ingest a session and the
//! views move, retract one and they roll back — exactly, and without ever
//! rescanning what came before.
//!
//!     bog-bench reset
//!     bog-bench ingest session-a
//!     bog-bench show
//!     bog-bench ingest session-b     # views move
//!     bog-bench retract session-b    # views roll back
//!
//! The state between those commands lives on disk. Nothing is recomputed.

mod toolcall;
mod transcript;

use fold::pipeline::{Aggregate, Filter, KeyBy, terminal};
use fold::stream::Stream;
use std::path::{Path, PathBuf};
use std::time::Instant;
use toolcall::{ToolCall, ToolStats, tool_step};

/// Where Claude Code keeps its transcripts.
fn corpus_root() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude/projects")
}

fn db_path() -> std::path::PathBuf {
    std::env::temp_dir().join("bog-bench.db")
}

/// Rendering has to be a macro, not a function: `open!()` returns
/// `impl Push<ToolCall>`, which erases the pipeline's `Reader` associated
/// type, so nothing outside this crate can name the tuple `rtx` hands back.
/// bogkit's own `timeseries` example hits the same wall and solves it the
/// same way.
macro_rules! show {
    ($st:expr) => {
        $st.rtx(|(total, by_tool, failures, by_session)| {
            let mut tools: Vec<(String, ToolStats)> = by_tool.iter().collect();
            tools.sort_by(|a, b| b.1.context_tokens.cmp(&a.1.context_tokens));
            let mut sessions: Vec<(String, ToolStats)> = by_session.iter().collect();
            sessions.sort_by(|a, b| a.0.cmp(&b.0));
            print_snapshot(total.get(), failures.get(), &tools, &sessions);
        })
    };
}

/// The pipeline, as a macro rather than a function for the same reason as
/// `show!`: returning `impl Push<ToolCall>` makes the `Reader` type opaque
/// even inside this crate, and then `rtx` can no longer be destructured.
/// Expanding inline keeps the concrete type visible at every call site.
///
/// Every expansion must build the identical shape — that is how fold resumes
/// prior state from disk instead of rebuilding it.
macro_rules! open {
    () => {
        Stream::new(
            db_path(),
            (
                terminal::Count::new("calls_total"),
                KeyBy::new(
                    |c: &ToolCall| c.tool.clone(),
                    Aggregate::new(
                        "stats_by_tool",
                        tool_step,
                        terminal::Table::new("tool_stats"),
                    ),
                ),
                Filter::new(
                    |c: &ToolCall| !c.ok,
                    terminal::Count::new("failures_total"),
                ),
                KeyBy::new(
                    |c: &ToolCall| c.session.clone(),
                    Aggregate::new(
                        "stats_by_session",
                        tool_step,
                        terminal::Table::new("session_stats"),
                    ),
                ),
            ),
        )
    };
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("demo");

    match cmd {
        "reset" => {
            let _ = std::fs::remove_dir_all(db_path());
            println!("reset — {} cleared", db_path().display());
        }
        "show" => {
            let st = open!();
            show!(st);
        }
        "ingest" => {
            let Some(session) = args.get(1) else {
                eprintln!("usage: bog-bench ingest <session>");
                std::process::exit(2);
            };
            let calls = calls_for(session);
            if calls.is_empty() {
                eprintln!("no calls found for session '{session}'");
                std::process::exit(1);
            }
            let n = calls.len();
            let mut st = open!();
            st.wtx(|tx| {
                for call in &calls {
                    tx.insert(call);
                }
            });
            println!("ingested {n} calls from {session}\n");
            show!(st);
        }
        "retract" => {
            let Some(session) = args.get(1) else {
                eprintln!("usage: bog-bench retract <session>");
                std::process::exit(2);
            };
            // Re-deriving the calls is deterministic, so we can hand fold the
            // exact deltas to reverse without having stored them ourselves.
            let calls = calls_for(session);
            if calls.is_empty() {
                eprintln!("no calls found for session '{session}'");
                std::process::exit(1);
            }
            let n = calls.len();
            let mut st = open!();
            st.wtx(|tx| {
                for call in &calls {
                    tx.remove(call);
                }
            });
            println!("retracted {n} calls from {session}\n");
            show!(st);
        }
        "recent" => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(25);
            let root = corpus_root();
            let found = Instant::now();
            let paths = transcript::discover(&root, n);
            if paths.is_empty() {
                eprintln!("no transcripts under {}", root.display());
                std::process::exit(1);
            }
            let discovered_ms = found.elapsed().as_millis();

            let parsed_at = Instant::now();
            let calls: Vec<ToolCall> =
                paths.iter().flat_map(|p| transcript::parse_session(p)).collect();
            let parse_ms = parsed_at.elapsed().as_millis();

            let fold_at = Instant::now();
            let mut st = open!();
            st.wtx(|tx| {
                for call in &calls {
                    tx.insert(call);
                }
            });
            let fold_ms = fold_at.elapsed().as_millis();

            println!(
                "{} sessions discovered in {discovered_ms}ms · {} calls parsed in {parse_ms}ms · folded in {fold_ms}ms\n",
                paths.len(),
                calls.len()
            );
            show!(st);
        }
        "demo" => demo(),
        other => {
            eprintln!("unknown command '{other}'");
            eprintln!(
                "usage: bog-bench [reset|recent <n>|ingest <path>|retract <path>|show|demo]"
            );
            std::process::exit(2);
        }
    }
}

/// The whole story, non-interactively, on one persistent stream.
fn demo() {
    let _ = std::fs::remove_dir_all(db_path());
    let mut st = open!();

    let a = calls_for("session-a");
    st.wtx(|tx| {
        for c in &a {
            tx.insert(c);
        }
    });
    println!("== ingested session-a ==");
    show!(st);

    let b = calls_for("session-b");
    st.wtx(|tx| {
        for c in &b {
            tx.insert(c);
        }
    });
    println!("\n== appended session-b — views moved, nothing rescanned ==");
    show!(st);

    st.wtx(|tx| {
        for c in &b {
            tx.remove(c);
        }
    });
    println!("\n== retracted session-b — every counter rolled back ==");
    show!(st);
}

fn print_snapshot(
    total: i64,
    failures: i64,
    tools: &[(String, ToolStats)],
    sessions: &[(String, ToolStats)],
) {
    let pct = if total == 0 {
        0.0
    } else {
        failures as f64 / total as f64 * 100.0
    };
    println!("{total} tool calls · {failures} failed ({pct:.1}%)");
    println!();
    println!(
        "  {:<26}{:>7}{:>7}{:>8}{:>12}{:>9}",
        "TOOL", "CALLS", "FAIL", "FAIL%", "TOKENS", "AVG"
    );
    for (tool, s) in tools {
        // MCP tool names run to 60+ chars and wreck the columns.
        let name = if tool.chars().count() > 25 {
            let head: String = tool.chars().take(24).collect();
            format!("{head}…")
        } else {
            tool.clone()
        };
        println!(
            "  {:<26}{:>7}{:>7}{:>7.0}%{:>12}{:>9.0}",
            name,
            s.calls,
            s.failures,
            s.failure_rate() * 100.0,
            s.context_tokens,
            s.avg_tokens()
        );
    }

    // Churn worth acting on: things that fail often enough to cost retries.
    let mut churn: Vec<&(String, ToolStats)> = tools
        .iter()
        .filter(|(_, s)| s.failures > 0 && s.calls >= 3)
        .collect();
    churn.sort_by(|a, b| b.1.failure_rate().total_cmp(&a.1.failure_rate()));
    if !churn.is_empty() {
        println!();
        println!("  churn — highest failure rates:");
        for (tool, s) in churn.iter().take(5) {
            println!(
                "    {:>5.0}%  {tool}  ({}/{} failed)",
                s.failure_rate() * 100.0,
                s.failures,
                s.calls
            );
        }
    }

    if !sessions.is_empty() {
        let mut heaviest: Vec<&(String, ToolStats)> = sessions.iter().collect();
        heaviest.sort_by(|a, b| b.1.context_tokens.cmp(&a.1.context_tokens));
        println!();
        println!(
            "  {} sessions in view — heaviest by context:",
            sessions.len()
        );
        for (name, s) in heaviest.iter().take(5) {
            println!("    {name}  {} calls, {} tokens", s.calls, s.context_tokens);
        }
    }
}

/// Source of deltas. A real transcript path parses for real; anything else
/// falls back to the built-in fixtures so `demo` runs on a machine with no
/// corpus at all.
fn calls_for(session: &str) -> Vec<ToolCall> {
    let path = Path::new(session);
    if path.is_file() {
        return transcript::parse_session(path);
    }
    fixture(session)
}

fn fixture(session: &str) -> Vec<ToolCall> {
    let mk = |tool: &str, ok: bool, chars: u64, ms: u64, at: u64| ToolCall {
        session: session.to_string(),
        tool: tool.to_string(),
        ok,
        result_chars: chars,
        duration_ms: ms,
        at_ms: at,
    };
    match session {
        "session-a" => vec![
            mk("Read", true, 8200, 120, 1_000),
            mk("Read", true, 15400, 140, 2_000),
            mk("Bash", false, 2100, 3400, 3_000),
            mk("Bash", true, 640, 900, 4_000),
            mk("Edit", true, 180, 80, 5_000),
        ],
        "session-b" => vec![
            mk("Read", true, 44000, 210, 6_000),
            mk("Bash", false, 1900, 5200, 7_000),
            mk("Bash", false, 2000, 5100, 8_000),
            mk("Grep", true, 3300, 190, 9_000),
        ],
        _ => vec![],
    }
}
