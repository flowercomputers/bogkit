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

#[cfg(test)]
mod tests;

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
/// The set of sessions already folded in, read straight off `session_stats`.
///
/// Transcripts are immutable once a session ends, so a session that is already
/// in the views has nothing new to contribute — and `tx.insert` is a multiset
/// add, so re-ingesting one would silently double every figure it touched.
/// This is the guard. A row whose accumulator has fallen to zero calls was
/// retracted, so it is *not* counted as present and can be ingested again.
macro_rules! ingested_sessions {
    ($st:expr) => {
        $st.rtx(|(_, _, _, by_session)| {
            by_session
                .iter()
                .filter(|(_, s): &(String, ToolStats)| s.calls != 0)
                .map(|(k, _)| k)
                .collect::<std::collections::HashSet<String>>()
        })
    };
}

macro_rules! show {
    ($st:expr) => {
        $st.rtx(|(total, by_tool, failures, by_session)| {
            let mut tools: Vec<(String, ToolStats)> = by_tool.iter().collect();
            tools.sort_by(|a, b| b.1.result_chars.cmp(&a.1.result_chars));
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
        open!(db_path())
    };
    ($path:expr) => {
        Stream::new(
            $path,
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
            let mut st = open!();
            let seen = ingested_sessions!(st);
            let fresh: Vec<&ToolCall> =
                calls.iter().filter(|c| !seen.contains(&c.session)).collect();
            if fresh.is_empty() {
                println!("already ingested — nothing to add\n");
            } else {
                let n = fresh.len();
                st.wtx(|tx| {
                    for call in &fresh {
                        tx.insert(*call);
                    }
                });
                println!("ingested {n} calls from {session}\n");
            }
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
            let seen = ingested_sessions!(st);
            let fresh: Vec<&ToolCall> =
                calls.iter().filter(|c| !seen.contains(&c.session)).collect();
            st.wtx(|tx| {
                for call in &fresh {
                    tx.insert(*call);
                }
            });
            let fold_ms = fold_at.elapsed().as_millis();

            let skipped = calls.len() - fresh.len();
            println!(
                "{} sessions discovered in {discovered_ms}ms · {} calls parsed in {parse_ms}ms · {} folded in {fold_ms}ms{}\n",
                paths.len(),
                calls.len(),
                fresh.len(),
                if skipped > 0 {
                    format!(" · {skipped} already ingested, skipped")
                } else {
                    String::new()
                }
            );
            show!(st);
        }
        "bench" => bench(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100)),
        "window" => window(
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(24),
            args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400),
        ),
        "demo" => demo(),
        other => {
            eprintln!("unknown command '{other}'\n");
            eprintln!("usage: bog-bench <command>\n");
            eprintln!("  demo                  the whole story on built-in fixtures — start here");
            eprintln!("  recent <n>            ingest your n newest Claude sessions");
            eprintln!("  show                  print the current views");
            eprintln!("  ingest <path|name>    fold in one transcript");
            eprintln!("  retract <path|name>   pull one back out; every view rolls back");
            eprintln!("  bench <n>             incremental vs full rescan, cross-checked");
            eprintln!("  window <hours> <n>    churn over a rolling window");
            eprintln!("  reset                 clear the database");
            std::process::exit(2);
        }
    }
}

/// Churn over a rolling window of the last `hours` — the shape a CI gate
/// actually wants ("has churn risen *lately*", not "since the beginning").
///
/// `Retain` is processing-time: it stamps each record with the wall clock of
/// the transaction that commits it, ignoring any timestamp the record carries.
/// Replaying history through it verbatim would therefore stamp a year of
/// transcripts as all arriving "now". `Retain::with_clock` is the way out —
/// we drive a synthetic clock from the transcripts' own timestamps and commit
/// in event order, one transaction per hour, so expiry follows event time.
///
/// This runs on its own stream and its own database on purpose: adding a node
/// to the main pipeline changes its keyspaces, and there is no reason to risk
/// the working views for a secondary view.
fn window(hours: u64, n: usize) {
    use fold::pipeline::Retain;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    let root = corpus_root();
    let paths = transcript::discover(&root, n);
    let mut calls: Vec<ToolCall> = paths
        .iter()
        .flat_map(|p| transcript::parse_session(p))
        .filter(|c| c.at_ms > 0)
        .collect();
    if calls.is_empty() {
        eprintln!("no timestamped calls under {}", root.display());
        std::process::exit(1);
    }
    calls.sort_by_key(|c| c.at_ms);

    let span_h = (calls.last().unwrap().at_ms - calls[0].at_ms) / 3_600_000;
    let clock = Arc::new(AtomicU64::new(calls[0].at_ms));
    let tick = clock.clone();

    let db = std::env::temp_dir().join("bog-bench-window.db");
    let _ = std::fs::remove_dir_all(&db);

    let mut st = Stream::new(
        &db,
        Retain::with_clock(
            "window",
            Duration::from_secs(hours * 3600),
            move || tick.load(Ordering::Relaxed),
            (
                terminal::Count::new("win_total"),
                KeyBy::new(
                    |c: &ToolCall| c.tool.clone(),
                    Aggregate::new("win_by_tool", tool_step, terminal::Table::new("win_tool")),
                ),
            ),
        ),
    );

    // Replay in event order, one transaction per hour of corpus time. Each
    // commit advances the clock, which is what lets aged-out records expire.
    let mut i = 0;
    while i < calls.len() {
        let hour = calls[i].at_ms / 3_600_000;
        let start = i;
        while i < calls.len() && calls[i].at_ms / 3_600_000 == hour {
            i += 1;
        }
        clock.store(calls[i - 1].at_ms, Ordering::Relaxed);
        let batch = &calls[start..i];
        st.wtx(|tx| {
            for c in batch {
                tx.insert(c);
            }
        });
    }

    println!(
        "replayed {} calls spanning {span_h}h · window = last {hours}h\n",
        calls.len()
    );

    st.rtx(|(total, by_tool)| {
        let mut tools: Vec<(String, ToolStats)> = by_tool.iter().collect();
        tools.sort_by(|a, b| b.1.result_chars.cmp(&a.1.result_chars));
        println!("{} calls still inside the window", total.get());
        println!();
        // Ranks on measured characters, same as the main leaderboard — not tokens.
        println!("  {:<26}{:>7}{:>7}{:>12}", "TOOL", "CALLS", "FAIL", "CHARS");
        for (tool, s) in tools.iter().take(12) {
            let name = if tool.chars().count() > 25 {
                let head: String = tool.chars().take(24).collect();
                format!("{head}…")
            } else {
                tool.clone()
            };
            println!(
                "  {:<26}{:>7}{:>7}{:>12}",
                name, s.calls, s.failures, s.result_chars
            );
        }
    });

    let _ = std::fs::remove_dir_all(&db);
}

/// The claim under test: adding one session to an existing corpus costs only
/// that session, while a batch harness pays for the whole corpus again.
///
/// Both arms are timed end-to-end — parse *and* fold — because a batch tool
/// really does re-read every transcript. And both are checked against each
/// other first: a faster wrong answer is not an answer.
fn bench(n: usize) {
    let root = corpus_root();
    let paths = transcript::discover(&root, n + 1);
    if paths.len() < 2 {
        eprintln!("need at least 2 transcripts under {}", root.display());
        std::process::exit(1);
    }
    // `discover` returns newest first: treat the newest as the arriving
    // session and the rest as the corpus already on disk.
    let (arriving, existing) = paths.split_at(1);
    let arriving = &arriving[0];

    let base_db = std::env::temp_dir().join("bog-bench-inc.db");
    let full_db = std::env::temp_dir().join("bog-bench-full.db");
    let _ = std::fs::remove_dir_all(&base_db);
    let _ = std::fs::remove_dir_all(&full_db);

    // ---- setup (not measured): the corpus a running instance would already
    // have folded.
    let existing_calls: Vec<ToolCall> = existing
        .iter()
        .flat_map(|p| transcript::parse_session(p))
        .collect();
    {
        let mut st = open!(&base_db);
        st.wtx(|tx| {
            for c in &existing_calls {
                tx.insert(c);
            }
        });
    }

    // ---- arm 1: incremental. Parse and fold only what arrived.
    let t = Instant::now();
    let new_calls = transcript::parse_session(arriving);
    let inc_parse = t.elapsed();
    let t = Instant::now();
    let inc_total = {
        let mut st = open!(&base_db);
        st.wtx(|tx| {
            for c in &new_calls {
                tx.insert(c);
            }
        });
        st.rtx(|(total, _, _, _)| total.get())
    };
    let inc_fold = t.elapsed();

    // ---- arm 2: rescan. Parse and fold the entire corpus from nothing.
    let t = Instant::now();
    let all_calls: Vec<ToolCall> = paths
        .iter()
        .flat_map(|p| transcript::parse_session(p))
        .collect();
    let re_parse = t.elapsed();
    let t = Instant::now();
    let re_total = {
        let mut st = open!(&full_db);
        st.wtx(|tx| {
            for c in &all_calls {
                tx.insert(c);
            }
        });
        st.rtx(|(total, _, _, _)| total.get())
    };
    let re_fold = t.elapsed();

    // ---- correctness gate
    println!("corpus: {} sessions, {} tool calls", paths.len(), re_total);
    println!("arriving session: {} calls\n", new_calls.len());
    if inc_total != re_total {
        eprintln!("MISMATCH — incremental {inc_total} vs rescan {re_total}");
        std::process::exit(1);
    }
    println!("both arms agree: {inc_total} calls ✓\n");

    let inc_ms = (inc_parse + inc_fold).as_secs_f64() * 1000.0;
    let re_ms = (re_parse + re_fold).as_secs_f64() * 1000.0;
    println!("  {:<14}{:>11}{:>11}{:>11}", "", "PARSE", "FOLD", "TOTAL");
    println!(
        "  {:<14}{:>10.1}ms{:>10.1}ms{:>10.1}ms",
        "incremental",
        inc_parse.as_secs_f64() * 1000.0,
        inc_fold.as_secs_f64() * 1000.0,
        inc_ms
    );
    println!(
        "  {:<14}{:>10.1}ms{:>10.1}ms{:>10.1}ms",
        "rescan",
        re_parse.as_secs_f64() * 1000.0,
        re_fold.as_secs_f64() * 1000.0,
        re_ms
    );
    if inc_ms > 0.0 {
        println!("\n  {:.0}× cheaper to keep current than to recompute", re_ms / inc_ms);
    }

    let _ = std::fs::remove_dir_all(&base_db);
    let _ = std::fs::remove_dir_all(&full_db);
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
        "TOOL", "CALLS", "FAIL", "FAIL%", "CHARS", "~TOK"
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
            s.result_chars,
            s.est_tokens()
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
        heaviest.sort_by(|a, b| b.1.result_chars.cmp(&a.1.result_chars));
        println!();
        println!(
            "  {} sessions in view — heaviest by context:",
            sessions.len()
        );
        for (name, s) in heaviest.iter().take(5) {
            println!("    {name}  {} calls, {} chars", s.calls, s.result_chars);
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
