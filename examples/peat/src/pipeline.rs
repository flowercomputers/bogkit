//! The fold pipeline: every readable surface peat has, as one static graph.
//!
//! The stream carries `Keyed<EventId, Envelope>` and each branch opens with a
//! `FilterMap` selecting the events it cares about (the salience pattern:
//! fold retracts whole records, so hot event kinds must not share a record
//! with expensive branches). Everything downstream is stock bogkit.
//!
//! Determinism rules (they make `--asof`-style replay possible later):
//! - no wall-clock or randomness anywhere in this module;
//! - aggregate steps are commutative over the deltas of one transaction or
//!   only depend on event-carried timestamps.

use fold::pipeline::Keyed;
use serde::{Deserialize, Serialize};

use crate::event::{Envelope, Event, EventId};

pub const DAY_MS: u64 = 86_400_000;

// ---------------------------------------------------------------- rows

/// One event's contribution to its day bucket.
#[derive(Clone, Serialize, Deserialize)]
pub struct DayDelta {
    pub session_start: bool,
    pub tool: bool,
    pub fail: bool,
    pub commit: bool,
    pub file: Option<String>,
}

/// Materialized per-day digest.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct DayStats {
    pub sessions: i64,
    pub tools: i64,
    pub fails: i64,
    pub commits: i64,
    /// path -> touch count; kept as a sorted map so folds are
    /// iteration-order independent
    pub files: std::collections::BTreeMap<String, i64>,
}

/// A recorded observation, kept verbatim as evidence.
#[derive(Clone, Serialize, Deserialize)]
pub struct ObsRow {
    pub session: String,
    pub seq: u32,
    pub ts_ms: u64,
    pub text: String,
    pub derived_from: Vec<u32>,
}

/// Current understanding of one subject. Deliberately dumb (newest obs
/// wins); anything cleverer must stay expressible as a fold over the
/// evidence Multimap, which keeps the raw trail visible.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SubjStats {
    pub text: String,
    pub count: i64,
    pub last_ms: u64,
    /// seq of the winning obs; tie-breaks equal timestamps so the fold is
    /// independent of within-transaction drain order
    pub last_seq: u32,
    /// whether the winning obs cited mechanical events
    pub cited: bool,
}

/// One session's summary row.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SessStats {
    pub start_ms: u64,
    pub end_ms: u64,
    pub final_msg: String,
    pub cwd: String,
    pub branch: String,
    pub commits: i64,
}

/// Text indexed for recall, with the disposition the brief must show.
#[derive(Clone, Serialize, Deserialize)]
pub struct TextRow {
    pub text: String,
    /// "obs" | "final" | "user"
    pub kind: String,
    pub ts_ms: u64,
    pub cited: bool,
}

// ------------------------------------------------------------- branch fns

pub fn day_delta(k: &Keyed<EventId, Envelope>) -> Option<Keyed<u64, DayDelta>> {
    let e = &k.val;
    let d = match &e.kind {
        Event::SessionMeta { .. } => DayDelta {
            session_start: true,
            tool: false,
            fail: false,
            commit: false,
            file: None,
        },
        Event::ToolCall { ok, .. } => DayDelta {
            session_start: false,
            tool: true,
            fail: !ok,
            commit: false,
            file: None,
        },
        Event::Commit { .. } => DayDelta {
            session_start: false,
            tool: false,
            fail: false,
            commit: true,
            file: None,
        },
        Event::FileTouch { path } => DayDelta {
            session_start: false,
            tool: false,
            fail: false,
            commit: false,
            file: Some(path.clone()),
        },
        _ => return None,
    };
    Some(Keyed::new(e.ts_ms / DAY_MS, d))
}

pub fn day_step(acc: &mut DayStats, v: &DayDelta, delta: isize) {
    let d = delta as i64;
    acc.sessions += d * v.session_start as i64;
    acc.tools += d * v.tool as i64;
    acc.fails += d * v.fail as i64;
    acc.commits += d * v.commit as i64;
    if let Some(f) = &v.file {
        let n = acc.files.entry(f.clone()).or_default();
        *n += d;
        if *n <= 0 {
            acc.files.remove(f);
        }
    }
}

pub fn file_session(k: &Keyed<EventId, Envelope>) -> Option<Keyed<String, String>> {
    match &k.val.kind {
        Event::FileTouch { path } => Some(Keyed::new(path.clone(), k.val.session.clone())),
        _ => None,
    }
}

/// Whether a text row is distilled enough to embed. Vectors are for
/// beliefs, session summaries, and short user messages (directives read
/// like "ground in the formal model" — short by nature). Long user
/// messages are pasted walls: keyword-searchable via Bm25, but not worth
/// the O(n) graph rebuild a query pays.
pub const EMBED_USER_MAX: usize = 400;

pub fn embeddable(t: &Keyed<EventId, TextRow>) -> Option<Keyed<EventId, [f32; ese::DIMENSIONS]>> {
    (t.val.kind != "user" || t.val.text.len() <= EMBED_USER_MAX)
        .then(|| Keyed::new(t.key.clone(), ese::encode_single(&t.val.text)))
}

pub fn searchable(k: &Keyed<EventId, Envelope>) -> Option<Keyed<EventId, TextRow>> {
    let e = &k.val;
    let (text, kind, cited) = match &e.kind {
        Event::Obs {
            text, derived_from, ..
        } => (text, "obs", !derived_from.is_empty()),
        Event::FinalMsg { text } => (text, "final", true),
        Event::UserMsg { text } => (text, "user", true),
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(Keyed::new(
        k.key.clone(),
        TextRow {
            text: text.clone(),
            kind: kind.to_string(),
            ts_ms: e.ts_ms,
            cited,
        },
    ))
}

pub fn obs_row(k: &Keyed<EventId, Envelope>) -> Option<Keyed<String, ObsRow>> {
    match &k.val.kind {
        Event::Obs {
            subject,
            text,
            derived_from,
        } => Some(Keyed::new(
            subject.clone(),
            ObsRow {
                session: k.val.session.clone(),
                seq: k.key.1,
                ts_ms: k.val.ts_ms,
                text: text.clone(),
                derived_from: derived_from.clone(),
            },
        )),
        _ => None,
    }
}

/// Newest obs wins, ties broken by seq. Deliberately asymmetric under
/// retraction: a negative delta decrements `count` but never re-derives the
/// winning text (the previous winner is not recoverable from one delta).
/// Correct for peat's write paths — append + same-id revision, where the
/// replacement insert immediately re-wins — but a bare remove of the
/// current winner would leave its text as a ghost. If a delete verb ever
/// exists, rebuild this view from the evidence Multimap instead.
pub fn subj_step(acc: &mut SubjStats, v: &ObsRow, delta: isize) {
    acc.count += delta as i64;
    if delta > 0
        && (v.ts_ms, v.seq) >= (acc.last_ms, acc.last_seq)
    {
        acc.text = v.text.clone();
        acc.last_ms = v.ts_ms;
        acc.last_seq = v.seq;
        acc.cited = !v.derived_from.is_empty();
    }
}

pub fn sess_row(k: &Keyed<EventId, Envelope>) -> Option<Keyed<String, Envelope>> {
    match &k.val.kind {
        Event::SessionMeta { .. }
        | Event::FinalMsg { .. }
        | Event::Commit { .. }
        | Event::ToolCall { .. } => Some(Keyed::new(k.val.session.clone(), k.val.clone())),
        _ => None,
    }
}

/// Same declared asymmetry as [`subj_step`]: retraction adjusts counts but
/// does not un-derive `final_msg`/`cwd`; peat's write paths never bare-remove.
pub fn sess_step(acc: &mut SessStats, e: &Envelope, delta: isize) {
    let d = delta as i64;
    if delta > 0 {
        if acc.start_ms == 0 || e.ts_ms < acc.start_ms {
            acc.start_ms = e.ts_ms;
        }
        if e.ts_ms > acc.end_ms {
            acc.end_ms = e.ts_ms;
        }
    }
    match &e.kind {
        Event::SessionMeta { cwd, branch, .. } if delta > 0 => {
            acc.cwd = cwd.clone();
            acc.branch = branch.clone().unwrap_or_default();
        }
        Event::FinalMsg { text } if delta > 0 => acc.final_msg = text.clone(),
        Event::Commit { .. } => acc.commits += d,
        _ => {}
    }
}

/// The pipeline expression. A macro because the resulting type contains
/// closures and cannot be named; expand it where the concrete type is
/// needed (`KeyedStream::new(path, peat_pipeline!())`).
///
/// Reader shape (mirrors the sink tree):
/// `(days, files, (kw, vec, texts), (subjects, evidence), sessions, ledger)`
#[macro_export]
macro_rules! peat_pipeline {
    () => {{
        use fold::pipeline::{terminal, Aggregate, FilterMap, Map};
        use $crate::pipeline as p;
        (
            FilterMap::new(
                p::day_delta,
                Aggregate::new("days", p::day_step, terminal::Table::new("days_tbl")),
            ),
            FilterMap::new(p::file_session, terminal::Multimap::new("file_sessions")),
            FilterMap::new(
                p::searchable,
                (
                    Map::new(
                        |t: &fold::pipeline::Keyed<$crate::event::EventId, p::TextRow>| {
                            fold::pipeline::Keyed::new(t.key.clone(), t.val.text.clone())
                        },
                        terminal::search::Bm25::new("kw"),
                    ),
                    FilterMap::new(
                        p::embeddable,
                        terminal::search::Hnsw::<
                            $crate::event::EventId,
                            f32,
                            ::anny::metric::Cosine,
                            { ese::DIMENSIONS },
                        >::new("vec", ::anny::metric::Cosine, 42),
                    ),
                    terminal::Table::new("texts"),
                ),
            ),
            FilterMap::new(
                p::obs_row,
                (
                    Aggregate::new("subj", p::subj_step, terminal::Table::new("subjects")),
                    terminal::Multimap::new("evidence"),
                ),
            ),
            FilterMap::new(
                p::sess_row,
                Aggregate::new("sess", p::sess_step, terminal::Table::new("sessions_tbl")),
            ),
            // ledger mirror: the full event stream as a point-readable,
            // iterable table — what makes `asof` replay possible without
            // re-parsing transcripts
            terminal::Table::new("ledger"),
        )
    }};
}
