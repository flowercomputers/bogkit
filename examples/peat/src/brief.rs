//! The brief: one snapshot folded into a session-start orientation.
//!
//! `--json` is the API and carries full text; the rendered form goes
//! through a template (`.peat/brief.tmpl` overrides the embedded default)
//! which owns all display-time clipping.

use std::collections::HashMap;

use fold::pipeline::terminal::{MultimapReader, TableReader};
use fold::pipeline::Scored;
use fold::stream::Readable;

use crate::event::EventId;
use crate::pipeline::{DayStats, SessStats, SubjStats, TextRow, DAY_MS};
use crate::transcript::{date_label, local_offset_ms};
use crate::ui::{self, age_label, short_path, short_sess};

/// Reciprocal-rank-fusion constant (value from the original RRF paper).
const RRF_K: f64 = 60.0;

/// Fuse keyword and vector hit lists by reciprocal rank — the one ranking
/// rule, shared by `recall` and the brief's `relevant` section. Sorted
/// best-first with a deterministic id tie-break.
pub fn rrf(kw: &[Scored<f64, EventId>], vec: &[Scored<f32, EventId>]) -> Vec<(EventId, f64)> {
    let mut fused: HashMap<EventId, f64> = HashMap::new();
    for (rank, hit) in kw.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    for (rank, hit) in vec.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    let mut fused: Vec<(EventId, f64)> = fused.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    fused
}

#[derive(serde::Serialize)]
pub struct Brief {
    pub today: String,
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
    kw_search: impl Fn(&str, usize) -> Vec<Scored<f64, EventId>>,
    vec_search: impl Fn(&[f32; ese::DIMENSIONS]) -> Vec<Scored<f32, EventId>>,
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
                "final_msg": s.final_msg,
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
        let fused = rrf(&kw_search(query, 12), &vec_search(&ese::encode_single(query)));
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
                "text": t.text,
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
                "text": s.text,
            })
        })
        .collect();

    Brief {
        // the caller's local calendar date, computed from the same civil-
        // days math as the rest of the tool (no subprocess)
        today: date_label((now as i64 + local_offset_ms()) as u64),
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
pub fn render(brief: &Brief) -> String {
    let tmpl = std::fs::read_to_string(crate::db::peat_dir().join("brief.tmpl"))
        .unwrap_or_else(|_| DEFAULT_TMPL.to_string());
    let mut env = minijinja::Environment::new();
    ui::add_style_filters(&mut env);
    env.add_template("brief", &tmpl).unwrap();
    env.get_template("brief")
        .unwrap()
        .render(minijinja::Value::from_serialize(brief))
        .unwrap_or_else(|e| format!("peat: template error: {e}\n"))
}

/// The one output branch: `--json` gets the full structure, a terminal
/// gets the template.
pub fn emit(brief: &Brief, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(brief).unwrap());
    } else {
        print!("{}", render(brief));
    }
}

fn day_label(bucket: u64, today: u64) -> String {
    match today.saturating_sub(bucket) {
        0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n}d ago"),
    }
}
