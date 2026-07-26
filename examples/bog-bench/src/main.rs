//! bog-bench — agent tooling churn as persistent, retractable fold views.

mod toolcall;
mod transcript;

use fold::pipeline::{Aggregate, KeyBy, terminal};
use fold::stream::Stream;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use toolcall::{Outcome, ToolCall, ToolStats, tool_step};
use transcript::{ParseDiagnostics, ParsedSession};

const USAGE: &str = "\
usage: bog-bench <command>

  demo                  isolated ingest → append → retract fixtures
  recent <n>            reconcile your n newest Claude sessions
  show                  print the current persistent views
  ingest <path|name>    reconcile one transcript snapshot
  retract <path|name>   retract the exact snapshot previously ingested
  bench <n> <trials>    repeated incremental vs full-rescan benchmark
  window <hours> <n>    exact event-time rolling window
  reset                 clear the persistent database
";

fn corpus_root() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude/projects")
}

/// Versioned because the exact-snapshot branch changes the persistence schema.
///
/// Leaving the original `bog-bench.db` untouched is safer than opening old
/// aggregates that have no matching snapshot records.
fn db_path() -> PathBuf {
    std::env::temp_dir().join("bog-bench-v2.db")
}

fn scratch_db(label: &str, trial: usize) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bog-bench-{label}-{}-{trial}.db",
        std::process::id()
    ))
}

/// Every expansion builds the identical pipeline shape. The final `Bag` is the
/// exact set of deltas represented by the other materialized views, so a
/// replacement or retraction can update all six branches atomically.
macro_rules! open {
    () => {
        open!(db_path())
    };
    ($path:expr) => {
        fold::stream::Stream::new(
            $path,
            (
                fold::pipeline::terminal::Count::new("calls_total"),
                fold::pipeline::KeyBy::new(
                    |c: &$crate::toolcall::ToolCall| c.tool.clone(),
                    fold::pipeline::Aggregate::new(
                        "stats_by_tool",
                        $crate::toolcall::tool_step,
                        fold::pipeline::terminal::Table::new("tool_stats"),
                    ),
                ),
                fold::pipeline::Filter::new(
                    |c: &$crate::toolcall::ToolCall| {
                        c.outcome == $crate::toolcall::Outcome::ExplicitError
                    },
                    fold::pipeline::terminal::Count::new("explicit_errors_total"),
                ),
                fold::pipeline::Filter::new(
                    |c: &$crate::toolcall::ToolCall| {
                        c.outcome == $crate::toolcall::Outcome::Unknown
                    },
                    fold::pipeline::terminal::Count::new("unknown_outcomes_total"),
                ),
                fold::pipeline::KeyBy::new(
                    |c: &$crate::toolcall::ToolCall| c.session.clone(),
                    fold::pipeline::Aggregate::new(
                        "stats_by_session",
                        $crate::toolcall::tool_step,
                        fold::pipeline::terminal::Table::new("session_stats"),
                    ),
                ),
                fold::pipeline::terminal::Bag::<$crate::toolcall::ToolCall>::new("ingested_calls"),
            ),
        )
    };
}

#[derive(Debug, PartialEq, Eq)]
struct MaterializedSnapshot {
    total: i64,
    explicit_errors: i64,
    unknowns: i64,
    tools: Vec<(String, ToolStats)>,
    sessions: Vec<(String, ToolStats)>,
    exact_calls: Vec<(ToolCall, i64)>,
}

fn outcome_rank(outcome: Outcome) -> u8 {
    match outcome {
        Outcome::Success => 0,
        Outcome::ExplicitError => 1,
        Outcome::Unknown => 2,
    }
}

macro_rules! materialized_snapshot {
    ($st:expr) => {
        $st.rtx(
            |(total, by_tool, explicit_errors, unknowns, by_session, exact)| {
                let mut tools: Vec<(String, $crate::toolcall::ToolStats)> =
                    by_tool.iter().collect();
                tools.sort_by(|a, b| a.0.cmp(&b.0));
                let mut sessions: Vec<(String, $crate::toolcall::ToolStats)> =
                    by_session.iter().collect();
                sessions.sort_by(|a, b| a.0.cmp(&b.0));
                let mut exact_calls: Vec<($crate::toolcall::ToolCall, i64)> =
                    exact.iter().collect();
                exact_calls.sort_by(|(a, an), (b, bn)| {
                    (
                        &a.session,
                        &a.tool,
                        a.at_ms,
                        a.duration_ms,
                        a.result_chars,
                        $crate::outcome_rank(a.outcome),
                        an,
                    )
                        .cmp(&(
                            &b.session,
                            &b.tool,
                            b.at_ms,
                            b.duration_ms,
                            b.result_chars,
                            $crate::outcome_rank(b.outcome),
                            bn,
                        ))
                });
                $crate::MaterializedSnapshot {
                    total: total.get(),
                    explicit_errors: explicit_errors.get(),
                    unknowns: unknowns.get(),
                    tools,
                    sessions,
                    exact_calls,
                }
            },
        )
    };
}

macro_rules! stored_snapshot {
    ($st:expr, $session:expr) => {{
        let target: &str = $session;
        $st.rtx(
            |(_, _, _, _, _, exact)| -> Vec<$crate::toolcall::ToolCall> {
                exact
                    .iter()
                    .filter(|(call, _)| call.session == target)
                    .flat_map(|(call, multiplicity)| {
                        std::iter::repeat(call).take(multiplicity as usize)
                    })
                    .collect()
            },
        )
    }};
}

macro_rules! show {
    ($st:expr) => {
        $st.rtx(
            |(total, by_tool, explicit_errors, unknowns, by_session, _)| {
                let mut tools: Vec<(String, ToolStats)> = by_tool.iter().collect();
                tools.sort_by(|a, b| b.1.result_chars.cmp(&a.1.result_chars));
                let mut sessions: Vec<(String, ToolStats)> = by_session.iter().collect();
                sessions.sort_by(|a, b| a.0.cmp(&b.0));
                print_snapshot(
                    total.get(),
                    explicit_errors.get(),
                    unknowns.get(),
                    &tools,
                    &sessions,
                );
            },
        )
    };
}

fn same_call_multiset(left: &[ToolCall], right: &[ToolCall]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut counts: HashMap<&ToolCall, usize> = HashMap::new();
    for call in left {
        *counts.entry(call).or_default() += 1;
    }
    for call in right {
        let Some(count) = counts.get_mut(call) else {
            return false;
        };
        if *count == 1 {
            counts.remove(call);
        } else {
            *count -= 1;
        }
    }
    counts.is_empty()
}

#[derive(Debug, Clone, Copy)]
struct ReconcileResult {
    previous: usize,
    current: usize,
    changed: bool,
}

macro_rules! reconcile_snapshot {
    ($st:expr, $session:expr, $current:expr) => {{
        let previous = stored_snapshot!($st, $session);
        if $crate::same_call_multiset(&previous, $current) {
            $crate::ReconcileResult {
                previous: previous.len(),
                current: $current.len(),
                changed: false,
            }
        } else {
            $st.wtx(|tx| {
                for call in &previous {
                    tx.remove(call);
                }
                for call in $current {
                    tx.insert(call);
                }
            });
            $crate::ReconcileResult {
                previous: previous.len(),
                current: $current.len(),
                changed: true,
            }
        }
    }};
}

#[cfg(test)]
mod tests;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return;
    };

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
            let parsed = calls_for(session);
            report_diagnostics(Path::new(session), &parsed.diagnostics);
            if parsed.calls.is_empty() {
                eprintln!("no calls found for session '{session}'");
                std::process::exit(1);
            }

            let mut st = open!();
            let result = reconcile_snapshot!(st, &parsed.session, &parsed.calls);
            if !result.changed {
                println!("already current — nothing to reconcile\n");
            } else if result.previous == 0 {
                println!("ingested {} calls from {session}\n", result.current);
            } else {
                println!(
                    "reconciled {session}: {} stored calls → {} current calls\n",
                    result.previous, result.current
                );
            }
            show!(st);
        }
        "retract" => {
            let Some(session) = args.get(1) else {
                eprintln!("usage: bog-bench retract <session>");
                std::process::exit(2);
            };
            let key = session_key_for_arg(session);
            let mut st = open!();
            let previous = stored_snapshot!(st, &key);
            if previous.is_empty() {
                println!("not ingested — nothing to retract\n");
            } else {
                st.wtx(|tx| {
                    for call in &previous {
                        tx.remove(call);
                    }
                });
                println!("retracted {} stored calls from {session}\n", previous.len());
            }
            show!(st);
        }
        "recent" => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(25);
            recent(n);
        }
        "bench" => bench(
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100),
            args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5),
        ),
        "window" => window(
            args.get(1).and_then(|s| s.parse().ok()).unwrap_or(24),
            args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400),
        ),
        "demo" => demo(),
        other => {
            eprintln!("unknown command '{other}'\n");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn recent(n: usize) {
    let root = corpus_root();
    let found = Instant::now();
    let paths = transcript::discover(&root, n);
    if paths.is_empty() {
        eprintln!("no transcripts under {}", root.display());
        std::process::exit(1);
    }
    let discovered_ms = found.elapsed().as_millis();

    let parsed_at = Instant::now();
    let parsed: Vec<ParsedSession> = paths
        .iter()
        .map(|path| transcript::parse_session_report(path))
        .collect();
    let parse_ms = parsed_at.elapsed().as_millis();
    for (path, report) in paths.iter().zip(&parsed) {
        report_diagnostics(path, &report.diagnostics);
    }

    let fold_at = Instant::now();
    let mut st = open!();
    let mut changed = 0;
    let mut unchanged = 0;
    let mut current_calls = 0;
    for report in &parsed {
        if report.calls.is_empty() {
            continue;
        }
        current_calls += report.calls.len();
        let result = reconcile_snapshot!(st, &report.session, &report.calls);
        if result.changed {
            changed += 1;
        } else {
            unchanged += 1;
        }
    }
    let fold_ms = fold_at.elapsed().as_millis();

    println!(
        "{} sessions discovered in {discovered_ms}ms · {current_calls} current calls parsed in {parse_ms}ms · {changed} snapshots reconciled in {fold_ms}ms · {unchanged} already current\n",
        paths.len()
    );
    show!(st);
}

fn report_diagnostics(path: &Path, diagnostics: &ParseDiagnostics) {
    if diagnostics.has_issues() {
        eprintln!(
            "parse diagnostics for {}: {}",
            path.display(),
            diagnostics.summary()
        );
    }
}

fn calls_for(session: &str) -> ParsedSession {
    let path = Path::new(session);
    if path.is_file() {
        return transcript::parse_session_report(path);
    }
    ParsedSession {
        session: session.to_string(),
        calls: fixture(session),
        diagnostics: ParseDiagnostics::default(),
    }
}

fn session_key_for_arg(session: &str) -> String {
    if !fixture(session).is_empty() {
        session.to_string()
    } else {
        transcript::session_key(Path::new(session))
    }
}

/// Replay one transaction per exact event timestamp. `Retain` stamps every
/// call in a transaction with the synthetic clock, so coarser batching would
/// move early calls forward and make the boundary approximate.
fn window(hours: u64, n: usize) {
    use fold::pipeline::Retain;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    let root = corpus_root();
    let paths = transcript::discover(&root, n);
    let reports: Vec<ParsedSession> = paths
        .iter()
        .map(|path| transcript::parse_session_report(path))
        .collect();
    for (path, report) in paths.iter().zip(&reports) {
        report_diagnostics(path, &report.diagnostics);
    }
    let mut calls: Vec<ToolCall> = reports
        .into_iter()
        .flat_map(|report| report.calls)
        .filter(|call| call.at_ms > 0)
        .collect();
    if calls.is_empty() {
        eprintln!("no timestamped calls under {}", root.display());
        std::process::exit(1);
    }
    calls.sort_by_key(|call| call.at_ms);

    let first_at = calls.first().unwrap().at_ms;
    let last_at = calls.last().unwrap().at_ms;
    let span_h = (last_at - first_at) as f64 / 3_600_000.0;
    let clock = Arc::new(AtomicU64::new(first_at));
    let tick = clock.clone();
    let db = scratch_db("window", 0);
    let _ = std::fs::remove_dir_all(&db);

    let mut st = Stream::new(
        &db,
        Retain::with_clock(
            "window",
            Duration::from_secs(hours.saturating_mul(3600)),
            move || tick.load(Ordering::Relaxed),
            (
                terminal::Count::new("win_total"),
                KeyBy::new(
                    |call: &ToolCall| call.tool.clone(),
                    Aggregate::new("win_by_tool", tool_step, terminal::Table::new("win_tool")),
                ),
            ),
        ),
    );

    let mut index = 0;
    while index < calls.len() {
        let at_ms = calls[index].at_ms;
        let start = index;
        while index < calls.len() && calls[index].at_ms == at_ms {
            index += 1;
        }
        clock.store(at_ms, Ordering::Relaxed);
        st.wtx(|tx| {
            for call in &calls[start..index] {
                tx.insert(call);
            }
        });
    }

    println!(
        "replayed {} calls spanning {span_h:.2}h · exact window = last {hours}h\n",
        calls.len()
    );
    st.rtx(|(total, by_tool)| {
        let mut tools: Vec<(String, ToolStats)> = by_tool.iter().collect();
        tools.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.result_chars));
        println!("{} calls still inside the window", total.get());
        println!();
        println!(
            "  {:<26}{:>7}{:>7}{:>7}{:>12}",
            "TOOL", "CALLS", "ERROR", "UNK", "CHARS"
        );
        for (tool, stats) in tools.iter().take(12) {
            println!(
                "  {:<26}{:>7}{:>7}{:>7}{:>12}",
                truncate(tool, 25),
                stats.calls,
                stats.failures,
                stats.unknowns,
                stats.result_chars
            );
        }
    });

    drop(st);
    let _ = std::fs::remove_dir_all(&db);
}

#[derive(Debug, Clone, Copy)]
struct TrialTiming {
    incremental_parse_ms: f64,
    incremental_fold_ms: f64,
    rescan_parse_ms: f64,
    rescan_fold_ms: f64,
}

impl TrialTiming {
    fn incremental_total_ms(self) -> f64 {
        self.incremental_parse_ms + self.incremental_fold_ms
    }

    fn rescan_total_ms(self) -> f64 {
        self.rescan_parse_ms + self.rescan_fold_ms
    }
}

#[derive(Debug, Clone, Copy)]
struct Distribution {
    median: f64,
    min: f64,
    max: f64,
    stddev: f64,
}

fn distribution(mut values: Vec<f64>) -> Distribution {
    values.sort_by(f64::total_cmp);
    let len = values.len();
    let median = if len & 1 == 0 {
        (values[len / 2 - 1] + values[len / 2]) / 2.0
    } else {
        values[len / 2]
    };
    let mean = values.iter().sum::<f64>() / len as f64;
    let variance = values
        .iter()
        .map(|value| {
            let distance = value - mean;
            distance * distance
        })
        .sum::<f64>()
        / len as f64;
    Distribution {
        median,
        min: values[0],
        max: values[len - 1],
        stddev: variance.sqrt(),
    }
}

fn bench(n: usize, trials: usize) {
    let trials = trials.max(1);
    let root = corpus_root();
    let paths = transcript::discover(&root, n + 1);
    if paths.len() < 2 {
        eprintln!("need at least 2 transcripts under {}", root.display());
        std::process::exit(1);
    }
    let (arriving, existing) = paths.split_at(1);
    let arriving = &arriving[0];
    let existing_calls: Vec<ToolCall> = existing
        .iter()
        .flat_map(|path| transcript::parse_session(path))
        .collect();

    let mut timings = Vec::with_capacity(trials);
    let mut verified_snapshot: Option<MaterializedSnapshot> = None;
    let mut arriving_count = 0;

    for trial in 0..trials {
        let incremental_db = scratch_db("incremental", trial);
        let full_db = scratch_db("full", trial);
        let _ = std::fs::remove_dir_all(&incremental_db);
        let _ = std::fs::remove_dir_all(&full_db);

        {
            let mut st = open!(&incremental_db);
            st.wtx(|tx| {
                for call in &existing_calls {
                    tx.insert(call);
                }
            });
        }

        let started = Instant::now();
        let new_calls = transcript::parse_session(arriving);
        let incremental_parse_ms = started.elapsed().as_secs_f64() * 1000.0;
        arriving_count = new_calls.len();

        let started = Instant::now();
        let incremental_snapshot = {
            let mut st = open!(&incremental_db);
            st.wtx(|tx| {
                for call in &new_calls {
                    tx.insert(call);
                }
            });
            materialized_snapshot!(st)
        };
        let incremental_fold_ms = started.elapsed().as_secs_f64() * 1000.0;

        let started = Instant::now();
        let all_calls: Vec<ToolCall> = paths
            .iter()
            .flat_map(|path| transcript::parse_session(path))
            .collect();
        let rescan_parse_ms = started.elapsed().as_secs_f64() * 1000.0;

        let started = Instant::now();
        let full_snapshot = {
            let mut st = open!(&full_db);
            st.wtx(|tx| {
                for call in &all_calls {
                    tx.insert(call);
                }
            });
            materialized_snapshot!(st)
        };
        let rescan_fold_ms = started.elapsed().as_secs_f64() * 1000.0;

        if incremental_snapshot != full_snapshot {
            eprintln!(
                "MISMATCH in trial {} — incremental and rescan materialized views differ\nincremental: {incremental_snapshot:#?}\nrescan: {full_snapshot:#?}",
                trial + 1
            );
            std::process::exit(1);
        }
        verified_snapshot = Some(full_snapshot);
        timings.push(TrialTiming {
            incremental_parse_ms,
            incremental_fold_ms,
            rescan_parse_ms,
            rescan_fold_ms,
        });

        let _ = std::fs::remove_dir_all(&incremental_db);
        let _ = std::fs::remove_dir_all(&full_db);
    }

    let snapshot = verified_snapshot.unwrap();
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    println!(
        "environment: {}/{} · {profile} · {cpus} logical CPUs",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "corpus: {} sessions, {} tool calls",
        paths.len(),
        snapshot.total
    );
    println!("arriving session: {arriving_count} calls");
    println!("trials: {trials}");
    println!(
        "all materialized views agree: totals, explicit errors, unknowns, per-tool rows, per-session rows, exact calls ✓\n"
    );

    let rows = [
        (
            "incremental parse",
            distribution(
                timings
                    .iter()
                    .map(|timing| timing.incremental_parse_ms)
                    .collect(),
            ),
        ),
        (
            "incremental fold",
            distribution(
                timings
                    .iter()
                    .map(|timing| timing.incremental_fold_ms)
                    .collect(),
            ),
        ),
        (
            "incremental total",
            distribution(
                timings
                    .iter()
                    .copied()
                    .map(TrialTiming::incremental_total_ms)
                    .collect(),
            ),
        ),
        (
            "rescan parse",
            distribution(
                timings
                    .iter()
                    .map(|timing| timing.rescan_parse_ms)
                    .collect(),
            ),
        ),
        (
            "rescan fold",
            distribution(timings.iter().map(|timing| timing.rescan_fold_ms).collect()),
        ),
        (
            "rescan total",
            distribution(
                timings
                    .iter()
                    .copied()
                    .map(TrialTiming::rescan_total_ms)
                    .collect(),
            ),
        ),
    ];
    println!(
        "  {:<20}{:>11}{:>11}{:>11}{:>11}",
        "METRIC", "median", "min", "max", "stddev"
    );
    for (name, stats) in rows {
        println!(
            "  {name:<20}{:>10.1}ms{:>10.1}ms{:>10.1}ms{:>10.1}ms",
            stats.median, stats.min, stats.max, stats.stddev
        );
    }

    let incremental = distribution(
        timings
            .iter()
            .copied()
            .map(TrialTiming::incremental_total_ms)
            .collect(),
    );
    let rescan = distribution(
        timings
            .iter()
            .copied()
            .map(TrialTiming::rescan_total_ms)
            .collect(),
    );
    if incremental.median > 0.0 {
        println!(
            "\nmedian rescan / incremental ratio: {:.1}×",
            rescan.median / incremental.median
        );
    }
}

/// The demo owns a process-unique scratch database and removes it afterward.
fn demo() {
    let db = scratch_db("demo", 0);
    let _ = std::fs::remove_dir_all(&db);
    let mut st = open!(&db);

    let first = fixture("session-a");
    st.wtx(|tx| {
        for call in &first {
            tx.insert(call);
        }
    });
    println!("== ingested session-a ==");
    show!(st);

    let second = fixture("session-b");
    st.wtx(|tx| {
        for call in &second {
            tx.insert(call);
        }
    });
    println!("\n== appended session-b — views moved, nothing rescanned ==");
    show!(st);

    st.wtx(|tx| {
        for call in &second {
            tx.remove(call);
        }
    });
    println!("\n== retracted session-b — every counter rolled back ==");
    show!(st);

    drop(st);
    let _ = std::fs::remove_dir_all(&db);
}

fn plural(n: i64, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

fn print_snapshot(
    total: i64,
    explicit_errors: i64,
    unknowns: i64,
    tools: &[(String, ToolStats)],
    sessions: &[(String, ToolStats)],
) {
    let known = total - unknowns;
    println!(
        "{} · {} among {} · {}",
        plural(total, "tool call", "tool calls"),
        plural(explicit_errors, "explicit error", "explicit errors"),
        plural(known, "known outcome", "known outcomes"),
        plural(unknowns, "unknown outcome", "unknown outcomes")
    );
    println!();
    println!(
        "  {:<26}{:>7}{:>7}{:>7}{:>8}{:>12}{:>9}",
        "TOOL", "CALLS", "ERROR", "UNK", "ERROR%", "CHARS", "~TOK"
    );
    for (tool, stats) in tools {
        println!(
            "  {:<26}{:>7}{:>7}{:>7}{:>7.0}%{:>12}{:>9.0}",
            truncate(tool, 25),
            stats.calls,
            stats.failures,
            stats.unknowns,
            stats.failure_rate() * 100.0,
            stats.result_chars,
            stats.est_tokens()
        );
    }

    let mut churn: Vec<&(String, ToolStats)> = tools
        .iter()
        .filter(|(_, stats)| stats.failures > 0 && stats.known_outcomes() >= 3)
        .collect();
    churn.sort_by(|a, b| b.1.failure_rate().total_cmp(&a.1.failure_rate()));
    if !churn.is_empty() {
        println!();
        println!("  churn — highest explicit-error rates:");
        for (tool, stats) in churn.iter().take(5) {
            println!(
                "    {:>5.0}%  {tool}  ({} explicit errors / {} known outcomes)",
                stats.failure_rate() * 100.0,
                stats.failures,
                stats.known_outcomes()
            );
        }
    }

    if !sessions.is_empty() {
        let mut heaviest: Vec<&(String, ToolStats)> = sessions.iter().collect();
        heaviest.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.result_chars));
        println!();
        println!(
            "  {} sessions in view — heaviest by normalized result characters:",
            sessions.len()
        );
        for (name, stats) in heaviest.iter().take(5) {
            println!(
                "    {}  {} calls, {} chars",
                session_label(name),
                stats.calls,
                stats.result_chars
            );
        }
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() > max_chars {
        let head: String = value.chars().take(max_chars - 1).collect();
        format!("{head}…")
    } else {
        value.to_string()
    }
}

fn session_label(session: &str) -> String {
    let path = Path::new(session);
    let mut parts = path
        .components()
        .rev()
        .take(2)
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return session.to_string();
    }
    parts.reverse();
    parts.join("/")
}

fn fixture(session: &str) -> Vec<ToolCall> {
    let make = |tool: &str, outcome: Outcome, chars: u64, ms: u64, at: u64| ToolCall {
        session: session.to_string(),
        tool: tool.to_string(),
        outcome,
        result_chars: chars,
        duration_ms: ms,
        at_ms: at,
    };
    match session {
        "session-a" => vec![
            make("Read", Outcome::Success, 8_200, 120, 1_000),
            make("Read", Outcome::Success, 15_400, 140, 2_000),
            make("Bash", Outcome::ExplicitError, 2_100, 3_400, 3_000),
            make("Bash", Outcome::Success, 640, 900, 4_000),
            make("Edit", Outcome::Success, 180, 80, 5_000),
        ],
        "session-b" => vec![
            make("Read", Outcome::Success, 44_000, 210, 6_000),
            make("Bash", Outcome::ExplicitError, 1_900, 5_200, 7_000),
            make("Bash", Outcome::ExplicitError, 2_000, 5_100, 8_000),
            make("Grep", Outcome::Success, 3_300, 190, 9_000),
        ],
        _ => Vec::new(),
    }
}
