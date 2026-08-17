//! peat — agent memory as a fold.
//!
//! Agents deposit events (mechanical session exhaust + small observations);
//! every readable surface is a materialized fold view. `capture` ingests a
//! Claude Code transcript at session end, `obs` records one observation,
//! `brief` assembles a session-start orientation from one snapshot.
//!
//!   peat capture <transcript.jsonl>     # Stop hook
//!   peat obs <subject> <text...>        # the one judgment step
//!   peat brief [task words...]          # SessionStart hook (stdout -> context)
//!
//! Wall-clock time is read only at the capture/render boundary (obs
//! timestamps, brief age labels) — never inside any fold path, so the
//! ledger stays deterministically replayable.

pub mod event;
pub mod pipeline;
pub mod transcript;
pub mod ui;

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Parser;
use fold::pipeline::terminal::{MultimapReader, TableReader};
use fold::stream::{KeyedStream, Readable};

use event::{Envelope, Event, EventId, OBS_SEQ_BASE};
use pipeline::{DayStats, SessStats, SubjStats, TextRow, DAY_MS};

/// Reciprocal-rank-fusion constant (value from the original RRF paper).
const RRF_K: f64 = 60.0;

pub fn ese_version() -> String {
    format!("ese-static-retrieval-mrl-en-v1 dim={}", ese::DIMENSIONS)
}

#[derive(Parser)]
#[command(name = "peat", about = "agent memory as a fold over bogkit")]
enum Cli {
    /// Ingest a Claude Code transcript (idempotent; run from the Stop hook)
    Capture {
        transcript: PathBuf,
        /// Session id if the transcript doesn't carry one
        #[arg(long)]
        session: Option<String>,
        /// Authoritative closing message (the Stop hook passes
        /// `.last_assistant_message`); overrides transcript tail parsing
        #[arg(long)]
        final_msg: Option<String>,
    },
    /// Record one observation about a subject
    Obs {
        subject: String,
        /// The claim, as one short sentence
        text: Vec<String>,
        /// Seqs of captured events this observation rests on
        #[arg(long, value_delimiter = ',')]
        from: Vec<u32>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Print a session-start orientation (stdout is injected as context)
    Brief {
        task: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Search memory: hybrid keyword + semantic recall, hits only
    Recall {
        query: Vec<String>,
        /// Max hits to print
        #[arg(long, default_value_t = 12)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Time travel: the brief as it would have read at the end of DATE.
    /// Replays the ledger prefix through the same deterministic pipeline.
    Asof {
        /// YYYY-MM-DD (cut at the end of that local calendar day)
        date: String,
        task: Vec<String>,
        #[arg(long)]
        json: bool,
    },
}

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("PEAT_DB") {
        return PathBuf::from(p);
    }
    peat_dir().join("db")
}

/// `.peat/` beside the nearest git/jj root above cwd, else cwd.
fn peat_dir() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap();
    loop {
        if dir.join(".git").exists() || dir.join(".jj").exists() {
            return dir.join(".peat");
        }
        if !dir.pop() {
            return std::env::current_dir().unwrap().join(".peat");
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn main() {
    let cli = Cli::parse();
    // Shared-db concurrency: fold is single-writer, and with several
    // worktree agents pointing PEAT_DB at one database, hook invocations
    // can collide. Peat processes are short-lived, so waiting is correct:
    // retry the open with backoff for up to ~45s before giving up.
    let mut st = {
        let phase = ui::Phase::new("opening ledger");
        let mut delay_ms = 200u64;
        let mut waited = 0u64;
        let st = loop {
            match std::panic::catch_unwind(|| {
                KeyedStream::<EventId, Envelope, _>::new(db_path(), peat_pipeline!())
            }) {
                Ok(st) => break st,
                Err(_) if waited < 45_000 => {
                    if waited == 0 && !ui::fancy_err() {
                        // non-tty gets one plain line instead of a spinner
                        eprintln!("peat: db busy (another agent writing); waiting…");
                    }
                    phase.tick(format!(
                        "ledger busy — another writer · waiting {}s (gives up at 45s)",
                        waited / 1000
                    ));
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    waited += delay_ms;
                    delay_ms = (delay_ms * 2).min(3_000);
                }
                Err(p) => std::panic::resume_unwind(p),
            }
        };
        phase.done();
        st
    };

    match cli {
        Cli::Capture {
            transcript,
            session,
            final_msg,
        } => {
            let Ok(jsonl) = std::fs::read_to_string(&transcript) else {
                eprintln!("peat: cannot read {}", transcript.display());
                std::process::exit(1);
            };
            let Some(mut parsed) = transcript::parse(&jsonl, session.as_deref()) else {
                eprintln!("peat: no session id found; pass --session");
                std::process::exit(1);
            };
            // hook-provided closing message is authoritative over tail parsing
            if let Some(text) = final_msg.filter(|t| !t.trim().is_empty()) {
                parsed.events.retain(|(_, e)| !matches!(e.kind, Event::FinalMsg { .. }));
                let ts = parsed.events.iter().map(|(_, e)| e.ts_ms).max().unwrap_or(0);
                parsed.events.push((
                    (parsed.session.clone(), event::HOOK_FINAL_SEQ),
                    Envelope::new(
                        &parsed.session,
                        ts,
                        Event::FinalMsg {
                            text: event::cap(&text, event::FINAL_MSG_CAP),
                        },
                    ),
                ));
            }
            let n = parsed.events.len();
            let phase = ui::Phase::new(&format!("capturing {n} events"));
            st.wtx(|tx| {
                for (id, env) in &parsed.events {
                    tx.upsert(id, env);
                }
            });
            // fsync + let fjall fold the journal into the LSM — without this
            // every subsequent open replays the whole journal, which after a
            // bulk backfill dominates brief latency
            st.checkpoint();
            phase.done();
            eprintln!("peat: captured {n} events from session {}", parsed.session);
        }

        Cli::Obs {
            subject,
            text,
            from,
            session,
        } => {
            let text = text.join(" ");
            let session = session
                .or_else(|| std::fs::read_to_string(peat_dir().join("current-session")).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    eprintln!(
                        "peat: no session id (.peat/current-session missing); pass --session"
                    );
                    std::process::exit(1);
                });

            // near-subject hint: cheap drift guard, never blocking
            let near: Vec<String> = st.rtx(|(_, _, _, (subjects, _), _, _)| {
                subjects
                    .iter()
                    .filter(|(s, _): &(String, SubjStats)| {
                        s != &subject && (s.contains(&subject) || subject.contains(s.as_str()))
                    })
                    .map(|(s, v)| format!("{s} ({} obs)", v.count))
                    .take(4)
                    .collect()
            });
            if !near.is_empty() {
                eprintln!("near subjects: {}", near.join(" · "));
            }

            // first free obs seq for this session
            let mut seq = OBS_SEQ_BASE;
            while st.contains(&(session.clone(), seq)) {
                seq += 1;
            }
            let env = Envelope::new(
                &session,
                now_ms(),
                Event::Obs {
                    subject: subject.clone(),
                    text,
                    derived_from: from,
                },
            );
            st.wtx(|tx| {
                tx.upsert(&(session.clone(), seq), &env);
            });
            let count = st.rtx(|(_, _, _, (subjects, _), _, _)| {
                subjects
                    .get(&subject)
                    .map(|s: SubjStats| s.count)
                    .unwrap_or(1)
            });
            st.checkpoint();
            eprintln!("recorded → {subject} (support {count})");
        }

        Cli::Brief { task, json } => {
            let query = task.join(" ");
            let brief = st.rtx(|(days, files, (kw, vec, texts), (subjects, _), sessions, _ledger)| {
                assemble(
                    &query,
                    now_ms(),
                    &days,
                    &files,
                    |q, n| kw.search(q, n),
                    |v| vec.search(v),
                    |id| texts.get(id),
                    &subjects,
                    &sessions,
                )
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&brief).unwrap());
            } else {
                print!("{}", render(&brief));
            }
        }

        Cli::Recall { query, limit, json } => {
            let query = query.join(" ");
            if query.trim().is_empty() {
                eprintln!("peat: recall needs a query");
                std::process::exit(1);
            }
            let now = now_ms();
            let hits = st.rtx(|(_, _, (kw, vec, texts), _, _, _)| {
                let mut fused: HashMap<EventId, f64> = HashMap::new();
                for (rank, hit) in kw.search(&query, limit * 2).iter().enumerate() {
                    *fused.entry(hit.val.clone()).or_default() +=
                        1.0 / (RRF_K + rank as f64 + 1.0);
                }
                for (rank, hit) in vec.search(&ese::encode_single(&query)).iter().enumerate() {
                    *fused.entry(hit.val.clone()).or_default() +=
                        1.0 / (RRF_K + rank as f64 + 1.0);
                }
                let mut fused: Vec<(EventId, f64)> = fused.into_iter().collect();
                fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                fused
                    .into_iter()
                    .filter_map(|(id, score)| {
                        let t = texts.get(&id)?;
                        Some(serde_json::json!({
                            "score": (score * 1000.0).round() / 1000.0,
                            "kind": t.kind,
                            "cited": t.cited,
                            "age": age_label(now, t.ts_ms),
                            "session": short_sess(&id.0),
                            "seq": id.1,
                            "text": clip(&t.text, 220),
                        }))
                    })
                    .take(limit)
                    .collect::<Vec<_>>()
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&hits).unwrap());
            } else if hits.is_empty() {
                println!("no hits for {query:?}");
            } else {
                for h in &hits {
                    println!(
                        "  [{}{} · {}] {}",
                        h["kind"].as_str().unwrap_or("?"),
                        if h["kind"] == "obs" && h["cited"] == true { "·cited" } else { "" },
                        h["age"].as_str().unwrap_or(""),
                        h["text"].as_str().unwrap_or(""),
                    );
                }
            }
        }

        Cli::Asof { date, task, json } => {
            // end of DATE in the caller's local day, not UTC: subtract the
            // local offset (render/capture boundary — the fold never sees
            // wall-clock or timezone)
            let Some(cutoff) = transcript::iso_to_ms(&format!("{date}T23:59:59.999Z"))
                .map(|utc| (utc as i64 - local_offset_ms()) as u64)
            else {
                eprintln!("peat: bad date {date:?}; expected YYYY-MM-DD");
                std::process::exit(1);
            };
            // the ledger mirror is what makes this possible: read every
            // event at-or-before the cutoff...
            let events: Vec<(EventId, Envelope)> =
                st.rtx(|(_, _, _, _, _, ledger)| {
                    ledger
                        .iter()
                        .filter(|(_, e): &(EventId, Envelope)| e.ts_ms <= cutoff)
                        .collect()
                });
            drop(st);
            // ...and fold that prefix through the SAME pipeline into a
            // scratch database. Determinism (oracle 2) is what makes the
            // result the truth of that day rather than a reconstruction.
            let scratch = std::env::temp_dir().join(format!("peat-asof-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&scratch);
            let phase = ui::Phase::new(&format!("replaying {} events to {date}", events.len()));
            let mut past =
                KeyedStream::<EventId, Envelope, _>::new(&scratch, peat_pipeline!());
            past.wtx(|tx| {
                for (id, e) in &events {
                    tx.upsert(id, e);
                }
            });
            phase.done();
            let query = task.join(" ");
            let mut brief = past.rtx(
                |(days, files, (kw, vec, texts), (subjects, _), sessions, _ledger)| {
                    assemble(
                        &query,
                        cutoff,
                        &days,
                        &files,
                        |q, n| kw.search(q, n),
                        |v| vec.search(v),
                        |id| texts.get(id),
                        &subjects,
                        &sessions,
                    )
                },
            );
            brief.today = format!("{date} · as of that day · {} events", events.len());
            if events.is_empty() {
                eprintln!(
                    "peat: no events at or before {date} — either this ledger's \
history starts later, or the db predates the ledger mirror \
(re-run `peat capture` on the transcripts to backfill it)"
                );
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&brief).unwrap());
            } else {
                print!("{}", render(&brief));
            }
            drop(past);
            let _ = std::fs::remove_dir_all(&scratch);
        }
    }
}

// ------------------------------------------------------------------ brief

#[derive(serde::Serialize)]
pub struct Brief {
    today: String,
    active: Vec<serde_json::Value>,
    days: Vec<serde_json::Value>,
    last_session: Option<serde_json::Value>,
    files: Vec<serde_json::Value>,
    relevant: Vec<serde_json::Value>,
    subjects: Vec<serde_json::Value>,
}

/// Assemble the brief from one snapshot's readers. The two search indexes
/// arrive as closures (their reader types carry tokenizer/const params);
/// tables and the multimap come as concrete readers.
#[allow(clippy::too_many_arguments)]
pub fn assemble<R: Readable>(
    query: &str,
    now: u64,
    days: &TableReader<'_, R, u64, DayStats>,
    files: &MultimapReader<'_, R, String, String>,
    kw_search: impl Fn(&str, usize) -> Vec<fold::pipeline::Scored<f64, EventId>>,
    vec_search: impl Fn(&[f32; ese::DIMENSIONS]) -> Vec<fold::pipeline::Scored<f32, EventId>>,
    text_of: impl Fn(&EventId) -> Option<TextRow>,
    subjects: &TableReader<'_, R, String, SubjStats>,
    sessions: &TableReader<'_, R, String, SessStats>,
) -> Brief {
    let today_bucket = now / DAY_MS;

    // ---- day digest: the 3 most recent non-empty days
    let mut day_rows: Vec<(u64, DayStats)> = days.iter().collect();
    day_rows.sort_by_key(|(d, _)| std::cmp::Reverse(*d));
    let days_out: Vec<serde_json::Value> = day_rows
        .iter()
        .take(3)
        .map(|(day, s)| {
            let mut fs: Vec<(&String, &i64)> = s.files.iter().collect();
            fs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
            serde_json::json!({
                "day": day_label(*day, today_bucket),
                "tools": s.tools, "fails": s.fails,
                "commits": s.commits, "sessions": s.sessions,
                "files": fs.iter().take(4).map(|(f, _)| short_path(f)).collect::<Vec<_>>(),
            })
        })
        .collect();

    // ---- active now: sessions with activity in the last hour, by
    // worktree — one agent's brief sees what the others are doing
    let mut sess: Vec<(String, SessStats)> = sessions.iter().collect();
    sess.sort_by_key(|(_, s)| std::cmp::Reverse(s.end_ms));
    let active: Vec<serde_json::Value> = sess
        .iter()
        .filter(|(_, s)| now.saturating_sub(s.end_ms) < 3_600_000)
        .take(6)
        .map(|(id, s)| {
            let place = s.cwd.rsplit('/').next().unwrap_or(&s.cwd);
            serde_json::json!({
                "where": place,
                "session": short_sess(id),
                "age": age_label(now, s.end_ms),
                "commits": s.commits,
            })
        })
        .collect();

    let last_session = sess
        .iter()
        .find(|(_, s)| !s.final_msg.is_empty())
        .map(|(_, s)| {
            serde_json::json!({
                "age_hours": (now.saturating_sub(s.end_ms)) / 3_600_000,
                "branch": s.branch,
                "final_msg": clip(&s.final_msg, 400),
            })
        });

    // ---- files: most-touched over the digest window, with their sessions
    let mut touch: HashMap<String, i64> = HashMap::new();
    for (_, s) in day_rows.iter().take(3) {
        for (f, n) in &s.files {
            *touch.entry(f.clone()).or_default() += n;
        }
    }
    let mut touch: Vec<(String, i64)> = touch.into_iter().collect();
    touch.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let files_out: Vec<serde_json::Value> = touch
        .iter()
        .take(5)
        .map(|(path, _)| {
            let mut ss = files.get(path);
            ss.sort();
            ss.dedup();
            serde_json::json!({
                "path": short_path(path),
                "sessions": ss.iter().map(|s| short_sess(s)).collect::<Vec<_>>(),
            })
        })
        .collect();

    // ---- relevant: hybrid RRF over the text indexes, disposition inline
    let mut relevant: Vec<serde_json::Value> = Vec::new();
    if !query.trim().is_empty() {
        let mut fused: HashMap<EventId, f64> = HashMap::new();
        for (rank, hit) in kw_search(query, 12).iter().enumerate() {
            *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
        for (rank, hit) in vec_search(&ese::encode_single(query)).iter().enumerate() {
            *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
        let mut fused: Vec<(EventId, f64)> = fused.into_iter().collect();
        fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut per_session: HashMap<String, usize> = HashMap::new();
        for (id, _) in fused {
            if relevant.len() >= 6 {
                break;
            }
            // one session must not flood the list with its user/final/obs
            let n = per_session.entry(id.0.clone()).or_default();
            if *n >= 2 {
                continue;
            }
            let Some(t) = text_of(&id) else { continue };
            *n += 1;
            let mut tag = t.kind.clone();
            if t.kind == "obs" && t.cited {
                tag.push_str("·cited");
            }
            relevant.push(serde_json::json!({
                "tag": format!("{tag} · {}", age_label(now, t.ts_ms)),
                "text": clip(&t.text, 160),
            }));
        }
    }

    // ---- subjects: current understanding, newest first
    let mut subj: Vec<(String, SubjStats)> = subjects.iter().collect();
    subj.sort_by_key(|(_, s)| std::cmp::Reverse(s.last_ms));
    let subjects_out: Vec<serde_json::Value> = subj
        .iter()
        .take(5)
        .map(|(name, s)| {
            serde_json::json!({
                "subject": name,
                "count": s.count,
                "cited": s.cited,
                "age": age_label(now, s.last_ms),
                "text": clip(&s.text, 120),
            })
        })
        .collect();

    Brief {
        today: local_date().unwrap_or_else(|| format!("{} (utc)", date_label(now))),
        active,
        days: days_out,
        last_session,
        files: files_out,
        relevant,
        subjects: subjects_out,
    }
}

const DEFAULT_TMPL: &str = include_str!("../brief.tmpl");

/// Render through `.peat/brief.tmpl` if present (the experimentation
/// surface), else the embedded default. The template only formats — every
/// value is precomputed.
fn render(brief: &Brief) -> String {
    let tmpl = std::fs::read_to_string(peat_dir().join("brief.tmpl"))
        .unwrap_or_else(|_| DEFAULT_TMPL.to_string());
    let mut env = minijinja::Environment::new();
    ui::add_style_filters(&mut env);
    env.add_template("brief", &tmpl).unwrap();
    env.get_template("brief")
        .unwrap()
        .render(minijinja::Value::from_serialize(brief))
        .unwrap_or_else(|e| format!("peat: template error: {e}\n"))
}

// ---------------------------------------------------------------- helpers

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn short_path(p: &str) -> String {
    let parts: Vec<&str> = p.rsplitn(3, '/').collect();
    match parts.len() {
        3 => format!("…/{}/{}", parts[1], parts[0]),
        _ => p.to_string(),
    }
}

fn short_sess(s: &str) -> String {
    s.chars().take(8).collect()
}

fn age_label(now: u64, ts: u64) -> String {
    let d = now.saturating_sub(ts);
    match d {
        _ if d < 3_600_000 => "<1h".into(),
        _ if d < DAY_MS => format!("{}h", d / 3_600_000),
        _ => format!("{}d", d / DAY_MS),
    }
}

fn day_label(bucket: u64, today: u64) -> String {
    match today.saturating_sub(bucket) {
        0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n}d ago"),
    }
}

/// Local calendar date via `date` — the brief header should read as the
/// user's "today". Render-side only; the fold path never sees this.
fn local_date() -> Option<String> {
    let out = std::process::Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (s.len() == 10).then_some(s)
}

/// Local UTC offset in ms via `date +%z` (e.g. "-0700"). Render/capture
/// boundary only. Zero on any failure — falls back to UTC semantics.
fn local_offset_ms() -> i64 {
    let Ok(out) = std::process::Command::new("date").arg("+%z").output() else {
        return 0;
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s.len() != 5 {
        return 0;
    }
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let (h, m) = (s[1..3].parse::<i64>(), s[3..5].parse::<i64>());
    match (h, m) {
        (Ok(h), Ok(m)) => sign * (h * 60 + m) * 60_000,
        _ => 0,
    }
}

fn date_label(ms: u64) -> String {
    // inverse of transcript::iso_to_ms's civil-days math, date part only
    let days = ms / DAY_MS;
    let era = (days + 719_468) / 146_097;
    let doe = days + 719_468 - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests;
