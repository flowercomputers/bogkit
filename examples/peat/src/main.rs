//! peat — agent memory as a fold.
//!
//! Agents deposit events (mechanical session exhaust + small observations);
//! every readable surface is a materialized fold view. Hooks run `capture`
//! at every session boundary (Stop, PreCompact, SessionEnd) and inject
//! `brief` stdout at SessionStart; the agent-facing surface is bare
//! `peat` (orient), `peat <thing>` (look closer, dispatched by shape),
//! and `peat obs` (the one judgment step).
//!
//! Wall-clock time is read only at the capture/render boundary (obs
//! timestamps, brief age labels) — never inside any fold path, so the
//! ledger stays deterministically replayable.
//!
//! Crate layout: `event` (the ledger schema — the API to our past),
//! `pipeline` (the fold graph), `transcript` (Claude Code JSONL -> events,
//! plus the civil-date math), `db` (path/session policy and the
//! open-with-retry), `brief` (assembly and rendering), `ui` (terminal
//! presentation). This file is CLI dispatch.

pub mod brief;
pub mod db;
pub mod event;
pub mod ladder;
pub mod pipeline;
pub mod transcript;
pub mod ui;

use std::path::PathBuf;

use clap::Parser;

use event::{Envelope, Event, EventId, OBS_SEQ_BASE};
use pipeline::{ObsRow, SessStats, SubjStats, DAY_MS};
use ui::{age_label, clip, short_sess};

/// Shared row filters for the verbs that walk indexed text or the ledger.
#[derive(clap::Args, Default)]
struct Filter {
    /// Only this session (prefix ok)
    #[arg(long)]
    session: Option<String>,
    /// Only this kind: obs | said | user | final | compact | tool | file |
    /// commit | meta | compacted
    #[arg(long)]
    kind: Option<String>,
    /// Only events from the last N days
    #[arg(long)]
    since: Option<u64>,
}

impl Filter {
    fn cutoff_ms(&self, now: u64) -> Option<u64> {
        self.since.map(|d| now.saturating_sub(d * DAY_MS))
    }

    fn matches(&self, now: u64, session: &str, kind: &str, ts_ms: u64) -> bool {
        self.session
            .as_ref()
            .is_none_or(|p| session.starts_with(p.as_str()))
            && self.kind.as_ref().is_none_or(|k| kind == k)
            && self.cutoff_ms(now).is_none_or(|c| ts_ms >= c)
    }
}

#[derive(Parser)]
#[command(
    name = "peat",
    about = "agent memory as a fold",
    long_about = "agent memory as a fold\n\n\
Sessions deposit events into one append-forever ledger; every readable\n\
surface is an incrementally maintained view over it. Bare `peat` orients;\n\
`peat <thing>` looks closer, inferring what you mean from its shape; and\n\
every line of every read ends in the exact command that goes one level\n\
deeper.",
    after_help = "READING BY SHAPE:\n  \
peat                       the brief (what hooks inject at session start)\n  \
peat 2026-w33              a window: w33, 2026-07, 2026-08-14, q3, 2026, a..b\n  \
peat 36f96b8d [seq]        a session by id prefix; one event with seq\n  \
peat fm18                  a subject's full evidence trail (exact name)\n  \
peat fold hnsw fix         anything else: hybrid keyword+semantic search\n\n\
DEPOSITING:\n  \
peat obs staging \"deploys go through the blue env first\" --from 1042\n\n\
Subcommands are the explicit spellings of the same reads; hooks use them.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// What to look at — see READING BY SHAPE below (empty: the brief)
    query: Vec<String>,
    /// Full structured output (the API form; never clipped, never styled)
    #[arg(long, global = true)]
    json: bool,
    /// How many summary lines the brief compresses the older past into
    /// (whole history always covered; default 8, or PEAT_BRIEF_BUDGET)
    #[arg(long)]
    budget: Option<usize>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// Ingest a session transcript — Claude Code JSONL or Codex rollout,
    /// auto-detected. Idempotent: hooks re-run it at Stop, PreCompact, and
    /// SessionEnd; re-capture ingests only the delta
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
    /// Deposit one judged claim about a subject (the only writing verb
    /// an agent runs by hand)
    Obs {
        subject: String,
        /// The claim, as one short sentence
        text: Vec<String>,
        /// Seqs of captured events this observation rests on
        #[arg(long, value_delimiter = ',')]
        from: Vec<u32>,
        #[arg(long)]
        session: Option<String>,
        /// Backdate the observation to noon of this day (YYYY-MM-DD) —
        /// retroactive annotation; asof briefs for that day will carry it
        #[arg(long)]
        at: Option<String>,
    },
    /// The orientation `peat` prints bare: digest, temporal ladder, last
    /// session, beliefs (SessionStart hooks inject its stdout as context)
    Brief {
        task: Vec<String>,
        #[arg(long)]
        json: bool,
        /// How many summary lines to compress the older past into
        /// (whole history always covered; default 8, or PEAT_BRIEF_BUDGET)
        #[arg(long)]
        budget: Option<usize>,
    },
    /// Search memory (what `peat <words>` runs): hybrid keyword+semantic,
    /// each hit tagged [kind · age] and addressed
    Recall {
        query: Vec<String>,
        /// Max hits to print
        #[arg(long, default_value_t = 12)]
        limit: usize,
        #[command(flatten)]
        filter: Filter,
        /// Read one subject's full evidence trail instead of searching
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The raw ledger floor, oldest first, auto-paged on a terminal
    Events {
        #[command(flatten)]
        filter: Filter,
        #[arg(long)]
        json: bool,
    },
    /// The claims register: every subject, newest-wins text, support
    Subjects {
        #[arg(long)]
        json: bool,
    },
    /// One session's overview (what `peat <prefix>` runs), or one event
    /// in full with its citers when seq is given
    Show {
        /// Session id (prefix ok)
        session: String,
        seq: Option<u32>,
    },
    /// One window of time (what `peat <window>` runs): digest, children
    /// one rung finer, and what was said inside it
    Zoom {
        /// w33, 2026-w33, 2026-07, 2026-08-14, q3, 2026, or a..b
        window: String,
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// One snapshot -> assembled brief. A macro because the reader tuple's
/// type contains closures and cannot be named.
macro_rules! make_brief {
    ($st:expr, $query:expr, $now:expr, $budget:expr) => {
        $st.rtx(|(days, files, (kw, vec, texts), (subjects, evidence), sessions, _ledger)| {
            brief::assemble(
                $query,
                $now,
                $budget,
                &days,
                &files,
                |q, n| kw.search(q, n),
                |v| vec.search(v),
                |id| texts.get(id),
                &subjects,
                &evidence,
                &sessions,
            )
        })
    };
}

/// The command that reads a subject's full trail: bare `peat <name>` when
/// the name is one shell-safe token, the explicit flag form otherwise.
pub fn subject_handle_pub(name: &str) -> String {
    subject_handle(name)
}

fn subject_handle(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        format!("▸ peat {name}")
    } else {
        format!("▸ peat recall --subject {name:?}")
    }
}

/// The brief's band budget: flag beats env beats default.
fn band_budget(flag: Option<usize>) -> usize {
    flag.or_else(|| std::env::var("PEAT_BRIEF_BUDGET").ok().and_then(|v| v.parse().ok()))
        .unwrap_or(8)
}

fn main() {
    let cli = Cli::parse();
    let mut st = db::open(db::db_path(), || peat_pipeline!());

    // ---- shape dispatch: bare `peat` orients; `peat <thing>` looks
    // closer, inferring window / session / search from the argument's
    // shape. The header of every non-obvious read names its
    // interpretation, and explicit subcommands remain the unambiguous
    // spellings of the same reads.
    let cmd = match cli.cmd {
        Some(c) => c,
        None if cli.query.is_empty() => Cmd::Brief {
            task: vec![],
            json: cli.json,
            budget: cli.budget,
        },
        None => {
            let q = &cli.query;
            let now_bucket = now_ms() / DAY_MS;
            let is_sess = |s: &str| {
                s.len() >= 6 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            };
            if q.len() == 1 && ladder::parse_window(&q[0], now_bucket).is_some() {
                Cmd::Zoom { window: q[0].clone(), json: cli.json }
            } else if is_sess(&q[0])
                && (q.len() == 1 || (q.len() == 2 && q[1].parse::<u32>().is_ok()))
            {
                Cmd::Show {
                    session: q[0].clone(),
                    seq: q.get(1).and_then(|s| s.parse().ok()),
                }
            } else if q.len() == 1
                && st.rtx(|(_, _, _, (subjects, _), _, _)| {
                    subjects.get(&q[0]).is_some() as bool
                })
            {
                // an exact subject name reads its full evidence trail —
                // the expansion path every clipped belief line points at
                Cmd::Recall {
                    query: vec![],
                    limit: 12,
                    filter: Default::default(),
                    subject: Some(q[0].clone()),
                    json: cli.json,
                }
            } else {
                let words = q.join(" ");
                if !cli.json {
                    println!("{}", ui::h1(&format!("== search: {words:?} ==")));
                }
                Cmd::Recall {
                    query: q.clone(),
                    limit: 12,
                    filter: Default::default(),
                    subject: None,
                    json: cli.json,
                }
            }
        }
    };

    match cmd {
        Cmd::Capture {
            transcript,
            session,
            final_msg,
        } => {
            let Ok(jsonl) = std::fs::read_to_string(&transcript) else {
                ui::error(&format!("cannot read {}", transcript.display()));
                std::process::exit(1);
            };
            let Some(mut parsed) = transcript::parse(&jsonl, session.as_deref()) else {
                ui::error("no session id found; pass --session");
                std::process::exit(1);
            };
            if let Some(text) = final_msg.filter(|t| !t.trim().is_empty()) {
                transcript::override_final_msg(&mut parsed, &text);
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
            ui::note(&format!(
                "captured {n} events from session {}",
                parsed.session
            ));
        }

        Cmd::Obs {
            subject,
            text,
            from,
            session,
            at,
        } => {
            let ts = match &at {
                None => now_ms(),
                Some(d) => match transcript::local_day_ms(d, "12:00:00.000") {
                    Some(ts) => ts,
                    None => {
                        ui::error(&format!("bad --at date {d:?}; expected YYYY-MM-DD"));
                        std::process::exit(1);
                    }
                },
            };
            let Some(session) = db::current_session(session) else {
                ui::error(
                    "no session id (.peat/current-session missing here and \
beside the shared db); pass --session",
                );
                std::process::exit(1);
            };
            let text = text.join(" ");
            let env = Envelope::new(
                &session,
                ts,
                Event::Obs {
                    subject: subject.clone(),
                    text: text.clone(),
                    derived_from: from,
                },
            );
            // Advisory lint, never blocking: observations are read months
            // later by agents with zero shared context, so phrasing that
            // leans on the present moment quietly rots. The envelope
            // already carries the timestamp; the ledger already holds the
            // story (cite it with --from).
            let text_l = text.to_lowercase();
            for (marker, why) in [
                ("tonight", "time-deictic"),
                ("today", "time-deictic"),
                ("yesterday", "time-deictic"),
                ("this session", "session-deictic"),
                ("this evening", "time-deictic"),
                ("this morning", "time-deictic"),
                ("just now", "time-deictic"),
                ("earlier", "time-deictic"),
                ("the reviewer", "person-deictic"),
                ("this change", "change-deictic"),
                ("this fix", "change-deictic"),
                ("in flight", "status-log"),
                ("now live", "status-log"),
                ("mirrors ", "status-log"),
                ("mirrored to", "status-log"),
                ("shipped", "status-log"),
                ("landed", "status-log"),
            ] {
                if text_l.contains(marker) {
                    ui::note(&format!(
                        "style: {why:?} phrase {marker:?} — future readers lack this context; state the rule standalone (the timestamp is recorded, the story is citable via --from)"
                    ));
                }
            }

            // one transaction: hint, seq scan, insert, and count all see
            // the same state (and the count sees our own write)
            let count = st.wtx(|tx| {
                let near: Vec<String> = tx.rtx(|(_, _, _, (subjects, _), _, _)| {
                    subjects
                        .iter()
                        .filter(|(s, _): &(String, SubjStats)| {
                            s != &subject
                                && (s.contains(&subject) || subject.contains(s.as_str()))
                        })
                        .map(|(s, v)| format!("{s} ({} obs)", v.count))
                        .take(4)
                        .collect()
                });
                if !near.is_empty() {
                    ui::note(&format!("near subjects: {}", near.join(" · ")));
                }
                let mut seq = OBS_SEQ_BASE;
                while tx.contains(&(session.clone(), seq)) {
                    seq += 1;
                }
                tx.upsert(&(session.clone(), seq), &env);
                tx.rtx(|(_, _, _, (subjects, _), _, _)| {
                    subjects
                        .get(&subject)
                        .map(|s: SubjStats| s.count)
                        .unwrap_or(1)
                })
            });
            // no checkpoint here: one observation is a few journal bytes,
            // and per-command rotation emits one-row SSTs across every
            // keyspace (L0 shredding — fjall stalls writes at 20 L0 runs).
            // Bulk capture checkpoints; the journal absorbs single events.
            if text.chars().count() > 240 {
                ui::note(
                    "long for a claim — briefs clip at ~120 chars (trails read whole); \
consider splitting into separate observations",
                );
            }
            ui::note(&format!("recorded → {subject} (support {count})"));
        }

        Cmd::Brief { task, json, budget } => {
            let brief = make_brief!(st, &task.join(" "), now_ms(), band_budget(budget));
            brief::emit(&brief, json);
        }

        Cmd::Recall {
            query,
            limit,
            filter,
            subject,
            json,
        } => {
            let now = now_ms();
            if let Some(subj) = subject {
                // the claims register read: current text plus the full
                // evidence trail, straight from the evidence multimap
                let (head, mut rows) = st.rtx(|(_, _, _, (subjects, evidence), _, _)| {
                    (
                        subjects.get(&subj) as Option<SubjStats>,
                        evidence.get(&subj) as Vec<ObsRow>,
                    )
                });
                rows.sort_by_key(|r| std::cmp::Reverse(r.ts_ms));
                print_subject(&subj, head, &rows, now, json);
                return;
            }
            let query = query.join(" ");
            if query.trim().is_empty() {
                ui::error("recall needs a query (or --subject)");
                std::process::exit(1);
            }

            /// One recall hit; `--json` serializes it verbatim.
            #[derive(serde::Serialize)]
            struct Hit {
                score: f64,
                kind: String,
                cited: bool,
                age: String,
                session: String,
                seq: u32,
                text: String,
            }
            let hits: Vec<Hit> = st.rtx(|(_, _, (kw, vec, texts), _, _, _)| {
                brief::rrf(
                    &kw.search(&query, limit * 2),
                    &vec.search(&ese::encode_single(&query)),
                )
                .into_iter()
                .filter_map(|(id, score)| {
                    let t = texts.get(&id)?;
                    filter
                        .matches(now, &id.0, &t.kind, t.ts_ms)
                        .then(|| Hit {
                            score: (score * 1000.0).round() / 1000.0,
                            kind: t.kind,
                            cited: t.cited,
                            age: age_label(now, t.ts_ms),
                            session: short_sess(&id.0),
                            seq: id.1,
                            text: t.text,
                        })
                })
                .take(limit)
                .collect()
            });
            if json {
                println!("{}", serde_json::to_string_pretty(&hits).unwrap());
            } else if hits.is_empty() {
                println!("{}", ui::dim(&format!("no hits for {query:?}")));
            } else {
                for h in &hits {
                    let cited = if h.kind == "obs" && h.cited { "·cited" } else { "" };
                    println!(
                        "  {} {} {}",
                        ui::dim(&format!("[{}{} · {}]", h.kind, cited, h.age)),
                        clip(&h.text, 200),
                        ui::dim(&format!("▸ peat {} {}", h.session, h.seq)),
                    );
                }
            }
        }

        Cmd::Events { filter, json } => {
            let now = now_ms();
            let mut rows: Vec<(EventId, Envelope)> = st.rtx(|(_, _, _, _, _, ledger)| {
                ledger
                    .iter()
                    .filter(|((sess, _), e): &(EventId, Envelope)| {
                        filter.matches(now, sess, e.kind.tag(), e.ts_ms)
                    })
                    .collect()
            });
            // table iteration is (session, seq) key order; the ledger view
            // is chronological
            rows.sort_unstable_by(|a, b| (a.1.ts_ms, &a.0).cmp(&(b.1.ts_ms, &b.0)));
            if json {
                for ((sess, seq), e) in &rows {
                    println!(
                        "{}",
                        serde_json::json!({
                            "session": sess, "seq": seq, "ts_ms": e.ts_ms,
                            "v": e.v, "event": &e.kind,
                        })
                    );
                }
                return;
            }
            use std::fmt::Write;
            let mut out = String::with_capacity(rows.len() * 96);
            for ((sess, seq), e) in &rows {
                let _ = writeln!(
                    out,
                    "{} {} {:>10}  {:<7} {}",
                    ui::dim(&transcript::date_label(e.ts_ms)),
                    ui::dim(&short_sess(sess)),
                    ui::dim(&seq.to_string()),
                    ui::accent(e.kind.tag()),
                    e.kind.summary(),
                );
            }
            out.push_str(&ui::dim(&format!("({} events)\n", rows.len())));
            ui::page(&out);
        }

        Cmd::Subjects { json } => {
            let now = now_ms();
            let mut subj: Vec<(String, SubjStats)> =
                st.rtx(|(_, _, _, (subjects, _), _, _)| subjects.iter().collect());
            subj.sort_by_key(|(_, s)| std::cmp::Reverse(s.last_ms));
            if json {
                let rows: Vec<serde_json::Value> = subj
                    .iter()
                    .map(|(name, s)| {
                        serde_json::json!({
                            "subject": name, "support": s.count, "cited": s.cited,
                            "age": age_label(now, s.last_ms), "text": s.text,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&rows).unwrap());
            } else if subj.is_empty() {
                println!("{}", ui::dim("no subjects yet"));
            } else {
                for (name, s) in subj {
                    println!(
                        "  {} {} {}",
                        ui::accent(&name),
                        ui::dim(&format!(
                            "({} obs{}, {}):",
                            s.count,
                            if s.cited { "" } else { ", uncited" },
                            age_label(now, s.last_ms)
                        )),
                        format!(
                            "{}  {}",
                            clip(&s.text, 120),
                            ui::dim(&subject_handle(&name))
                        )
                    );
                }
            }
        }

        Cmd::Show { session, seq } => {
            let now = now_ms();
            let Some(seq) = seq else {
                // no seq: one session's overview — summary row, its
                // observations, and the handle to its raw events
                let found = st.rtx(|(_, _, _, (subjects, evidence), sessions, _ledger)| {
                    let hit: Option<(String, SessStats)> = sessions
                        .iter()
                        .find(|(sess, _): &(String, SessStats)| sess.starts_with(session.as_str()));
                    let mut obs: Vec<(String, ObsRow)> = Vec::new();
                    if let Some((sess, _)) = &hit {
                        for (name, _) in subjects.iter().collect::<Vec<(String, SubjStats)>>() {
                            for r in evidence.get(&name) {
                                if r.session == *sess {
                                    obs.push((name.clone(), r));
                                }
                            }
                        }
                    }
                    obs.sort_by_key(|(_, r)| r.ts_ms);
                    (hit, obs)
                });
                let (Some((sess, s)), obs) = found else {
                    ui::error(&format!("no session matching {session}*"));
                    std::process::exit(1);
                };
                let place = s.cwd.rsplit('/').next().unwrap_or(&s.cwd);
                // branch is noise when it restates the worktree name
                let branch = (!s.branch.is_empty()
                    && !s.branch.ends_with(place)
                    && !place.ends_with(s.branch.as_str()))
                .then(|| format!(" · {}", s.branch))
                .unwrap_or_default();
                println!(
                    "{} {}",
                    ui::h1(&format!("== session {} ==", short_sess(&sess))),
                    ui::dim(&format!(
                        "{place}{branch} · active {} – {} ago · {} commits",
                        age_label(now, s.start_ms),
                        age_label(now, s.end_ms),
                        s.commits
                    ))
                );
                if !s.final_msg.is_empty() {
                    println!("\n{}\n  {}", ui::h1("closing message:"), clip(&s.final_msg, 400));
                }
                if !obs.is_empty() {
                    println!("\n{}", ui::h1("observations:"));
                    for (subj, r) in &obs {
                        println!(
                            "  {} {}",
                            ui::dim(&format!(
                                "[{subj} · {}{}]",
                                age_label(now, r.ts_ms),
                                if r.derived_from.is_empty() { "" } else { " · cited" }
                            )),
                            r.text
                        );
                    }
                }
                println!("\n{}", ui::dim(&format!("▸ peat events --session {}", short_sess(&sess))));
                return;
            };
            let (hit, citing) = st.rtx(|(_, _, _, (subjects, evidence), sessions, ledger)| {
                // resolve the session prefix against the small sessions
                // table, then point-read the ledger — never scan it
                let hit: Option<(EventId, Envelope)> = sessions
                    .iter()
                    .map(|(sess, _): (String, SessStats)| sess)
                    .find(|sess| sess.starts_with(session.as_str()))
                    .and_then(|sess| {
                        let id = (sess, seq);
                        ledger.get(&id).map(|e: Envelope| (id, e))
                    });
                // the citer walk is only worth paying on a hit, and only
                // matching rows earn a subject-name clone
                let mut citing: Vec<(String, ObsRow)> = Vec::new();
                if let Some(((sess, _), _)) = &hit {
                    for (name, _) in subjects.iter().collect::<Vec<(String, SubjStats)>>() {
                        for r in evidence.get(&name) {
                            if r.session == *sess && r.derived_from.contains(&seq) {
                                citing.push((name.clone(), r));
                            }
                        }
                    }
                }
                (hit, citing)
            });
            match hit {
                None => {
                    ui::error(&format!("no event ({session}*, {seq})"));
                    std::process::exit(1);
                }
                Some(((sess, q), env)) => {
                    println!(
                        "{} {}",
                        ui::h1(&format!("event ({sess}, {q})")),
                        ui::dim(&format!("· {} · v{}", age_label(now, env.ts_ms), env.v))
                    );
                    println!("{}", serde_json::to_string_pretty(&env.kind).unwrap());
                    for (subj, r) in citing {
                        println!(
                            "{} {}",
                            ui::dim(&format!(
                                "cited by obs [{subj} · {}]:",
                                age_label(now, r.ts_ms)
                            )),
                            r.text
                        );
                    }
                }
            }
        }

        Cmd::Zoom { window, json } => {
            let now = now_ms();
            let now_bucket = now / DAY_MS;
            let Some((start, end, label)) = ladder::parse_window(&window, now_bucket) else {
                ui::error(&format!(
                    "not a window: {window:?} (want w33, 2026-w33, 2026-07, 2026-08-14, q3, 2026, or a..b)"
                ));
                std::process::exit(1);
            };
            let lo_ms = start * DAY_MS;
            let hi_ms = (end + 1) * DAY_MS;
            let (head, kids, sess_rows, finals, obs) =
                st.rtx(|(days, _, _, (subjects, evidence), sessions, ledger)| {
                    let day_rows: std::collections::BTreeMap<u64, pipeline::DayStats> =
                        days.iter().collect();
                    let mut obs_per_day: std::collections::BTreeMap<u64, i64> = Default::default();
                    let mut obs_rows: Vec<(String, ObsRow)> = Vec::new();
                    for (name, _) in subjects.iter().collect::<Vec<(String, SubjStats)>>() {
                        for r in evidence.get(&name) {
                            *obs_per_day.entry(r.ts_ms / DAY_MS).or_default() += 1;
                            if (lo_ms..hi_ms).contains(&r.ts_ms) {
                                obs_rows.push((name.clone(), r));
                            }
                        }
                    }
                    obs_rows.sort_by_key(|(_, r)| r.ts_ms);
                    let head = ladder::digest(
                        &day_rows, &obs_per_day, start, end,
                        label.clone(), String::new(), String::new(),
                    );
                    let kids: Vec<ladder::Band> = ladder::children(start, end)
                        .into_iter()
                        .map(|(s, e, l, h)| ladder::digest(&day_rows, &obs_per_day, s, e, l, String::new(), h))
                        .filter(|b| b.tools + b.commits + b.obs > 0)
                        .collect();
                    // sessions overlapping the window, newest first
                    let mut sess_rows: Vec<(String, SessStats)> = sessions
                        .iter()
                        .filter(|(_, s): &(String, SessStats)| s.start_ms < hi_ms && s.end_ms >= lo_ms)
                        .collect();
                    sess_rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.end_ms));
                    // lane B from the ledger mirror: what was said, verbatim
                    let mut finals: Vec<(EventId, u64, String)> = Vec::new();
                    for (id, e) in ledger.iter().collect::<Vec<(EventId, Envelope)>>() {
                        if !(lo_ms..hi_ms).contains(&e.ts_ms) {
                            continue;
                        }
                        match &e.kind {
                            Event::FinalMsg { text } | Event::CompactSummary { text } => {
                                finals.push((id, e.ts_ms, text.clone()));
                            }
                            _ => {}
                        }
                    }
                    finals.sort_by_key(|(_, ts, _)| std::cmp::Reverse(*ts));
                    finals.truncate(4);
                    (head, kids, sess_rows, finals, obs_rows)
                });
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "window": label, "start_day": start, "end_day": end,
                        "digest": head, "children": kids,
                        "sessions": sess_rows.iter().map(|(id, s)| serde_json::json!({
                            "session": short_sess(id), "cwd": s.cwd, "branch": s.branch,
                            "commits": s.commits, "start_ms": s.start_ms, "end_ms": s.end_ms,
                        })).collect::<Vec<_>>(),
                        "said": finals.iter().map(|(id, ts, t)| serde_json::json!({
                            "session": short_sess(&id.0), "seq": id.1, "ts_ms": ts, "text": t,
                        })).collect::<Vec<_>>(),
                        "observations": obs.iter().map(|(subj, r)| serde_json::json!({
                            "subject": subj, "session": short_sess(&r.session), "seq": r.seq,
                            "cited": !r.derived_from.is_empty(), "text": r.text,
                        })).collect::<Vec<_>>(),
                    }))
                    .unwrap()
                );
                return;
            }
            let fails = if head.fails > 0 { format!(" ({} fail)", head.fails) } else { String::new() };
            let span = {
                let s = ladder::Civil::from_bucket(start);
                let e = ladder::Civil::from_bucket(end);
                ladder::span_label(s, e)
            };
            println!(
                "{} {}",
                ui::h1(&format!("== {label}{} ==", if label.contains(' ') || span.is_empty() { String::new() } else { format!(" · {span}") })),
                ui::dim(&format!(
                    "{} tools{fails} · {} commits · {} sessions{}",
                    ui::knum(head.tools), head.commits, sess_rows.len(),
                    if head.obs > 0 { format!(" · {} obs", head.obs) } else { String::new() }
                ))
            );
            if !kids.is_empty() {
                println!("\n{}", ui::h1("within:"));
                for b in &kids {
                    let f = if b.fails > 0 { format!(" ({} fail)", b.fails) } else { String::new() };
                    let files = if b.files.is_empty() { String::new() } else { format!(" · {}", b.files.join(", ")) };
                    println!(
                        "  {} {} tools{f} · {} commits{}{files}  {}",
                        ui::dim(&format!("[{}]", b.label)),
                        ui::knum(b.tools), b.commits,
                        if b.obs > 0 { format!(" · {} obs", b.obs) } else { String::new() },
                        ui::dim(&format!("▸ {}", b.handle)),
                    );
                }
            }
            if start == end && !sess_rows.is_empty() {
                println!("\n{}", ui::h1("sessions:"));
                for (id, s) in &sess_rows {
                    let place = s.cwd.rsplit('/').next().unwrap_or(&s.cwd);
                    println!(
                        "  {} {place} · {} commits  {}",
                        ui::dim(&format!("[{}]", short_sess(id))),
                        s.commits,
                        ui::dim(&format!("▸ peat {}", short_sess(id))),
                    );
                }
            }
            if !finals.is_empty() {
                println!("\n{}", ui::h1("closing messages:"));
                for (id, ts, t) in &finals {
                    println!(
                        "  {} {}",
                        ui::dim(&format!("[{} · {}]", short_sess(&id.0), age_label(now, *ts))),
                        clip(t, 180)
                    );
                }
            }
            if !obs.is_empty() {
                println!("\n{}", ui::h1("observations:"));
                for (subj, r) in obs.iter().rev().take(8) {
                    println!(
                        "  {} {}",
                        ui::dim(&format!(
                            "[{subj} · {}{}]",
                            age_label(now, r.ts_ms),
                            if r.derived_from.is_empty() { "" } else { " · cited" }
                        )),
                        clip(&r.text, 160)
                    );
                }
            }
        }

        Cmd::Asof { date, task, json } => {
            // end of DATE in the caller's local day, not UTC — the fold
            // never sees timezones; this is the render/capture boundary
            let Some(cutoff) = transcript::local_day_ms(&date, "23:59:59.999") else {
                ui::error(&format!("bad date {date:?}; expected YYYY-MM-DD"));
                std::process::exit(1);
            };
            // the ledger mirror is what makes this possible: read every
            // event at-or-before the cutoff...
            let events: Vec<(EventId, Envelope)> = st.rtx(|(_, _, _, _, _, ledger)| {
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
            let mut past = db::open(scratch.clone(), || peat_pipeline!());
            past.wtx(|tx| {
                for (id, e) in &events {
                    tx.upsert(id, e);
                }
            });
            phase.done();
            let mut brief = make_brief!(past, &task.join(" "), cutoff, band_budget(None));
            brief.today = format!("{date} · as of that day · {} events", events.len());
            if events.is_empty() {
                ui::note(&format!(
                    "no events at or before {date} — either this ledger's \
history starts later, or the db predates the ledger mirror \
(re-run `peat capture` on the transcripts to backfill it)"
                ));
            }
            brief::emit(&brief, json);
            drop(past);
            let _ = std::fs::remove_dir_all(&scratch);
        }
    }
}

/// Print one subject's evidence trail (`recall --subject`).
fn print_subject(subj: &str, head: Option<SubjStats>, rows: &[ObsRow], now: u64, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "subject": subj, "current": head.as_ref().map(|h| &h.text),
                "support": head.as_ref().map(|h| h.count),
                "evidence": rows.iter().map(|r| serde_json::json!({
                    "session": r.session, "seq": r.seq,
                    "age": age_label(now, r.ts_ms),
                    "cited": !r.derived_from.is_empty(),
                    "text": r.text,
                })).collect::<Vec<_>>(),
            }))
            .unwrap()
        );
        return;
    }
    if rows.is_empty() {
        println!("{}", ui::dim(&format!("no such subject: {subj}")));
        return;
    }
    if let Some(h) = head {
        println!(
            "{} — {} {}",
            ui::accent(subj),
            h.text,
            ui::dim(&format!("({} obs)", h.count))
        );
    }
    for r in rows {
        println!(
            "  {} {}",
            ui::dim(&format!(
                "[{} · {}{}]",
                short_sess(&r.session),
                age_label(now, r.ts_ms),
                if r.derived_from.is_empty() { "" } else { " · cited" }
            )),
            r.text
        );
    }
}

#[cfg(test)]
mod tests;
