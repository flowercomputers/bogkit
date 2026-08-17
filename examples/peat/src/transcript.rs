//! Claude Code transcript JSONL -> ledger events.
//!
//! Transcripts are heterogeneous (`mode`, `file-history-snapshot`, hook
//! attachments, `user`/`assistant` messages, compaction summaries) and the
//! format is not ours. The one hard rule: **unknown or unparseable lines are
//! skipped, never fatal** — capture must succeed on a transcript we have
//! never seen, because a session only gets recorded once.
//!
//! Event ids are `(session, line_index * 16 + block_index)`, which is a pure
//! function of the transcript — re-capturing the same file produces the same
//! ids and `upsert` makes the whole operation idempotent.

use serde_json::Value;

use crate::event::{
    cap, Envelope, Event, EventId, DETAIL_CAP, FINAL_MSG_CAP, SAID_CAP, SAID_MIN, USER_MSG_CAP,
};

/// Blocks per transcript line the seq scheme can address.
const SEQ_STRIDE: u32 = 16;

pub struct Parsed {
    pub session: String,
    pub events: Vec<(EventId, Envelope)>,
}

pub fn parse(jsonl: &str, fallback_session: Option<&str>) -> Option<Parsed> {
    let lines: Vec<Value> = jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let session = lines
        .iter()
        .find_map(|v| v.get("sessionId").and_then(Value::as_str))
        .or(fallback_session)?
        .to_string();

    let mut events: Vec<(EventId, Envelope)> = Vec::new();
    let mut last_ts: u64 = 0;
    // tool_use id -> index into `events`, to mark `ok: false` when the
    // matching tool_result reports an error
    let mut call_sites: std::collections::HashMap<String, usize> = Default::default();
    let mut meta_done = false;
    // borrowed until the end of the loop; capped exactly once when kept
    let mut final_msg: Option<(u32, u64, &str)> = None;

    for (li, line) in lines.iter().enumerate() {
        let seq0 = (li as u32) * SEQ_STRIDE;
        let ts = line
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(iso_to_ms)
            .unwrap_or(last_ts);
        last_ts = ts;

        // one SessionMeta from the first line that carries cwd
        if !meta_done
            && let Some(cwd) = line.get("cwd").and_then(Value::as_str) {
                meta_done = true;
                events.push((
                    (session.clone(), seq0),
                    Envelope::new(
                        &session,
                        ts,
                        Event::SessionMeta {
                            cwd: cwd.to_string(),
                            branch: line
                                .get("gitBranch")
                                .and_then(Value::as_str)
                                .map(String::from),
                            ese_version: crate::event::ese_version(),
                        },
                    ),
                ));
            }

        if line.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
            events.push((
                (session.clone(), seq0 + 1),
                Envelope::new(&session, ts, Event::Compaction {}),
            ));
            // keep the compactor's distillation itself — it is the summary
            // of everything the context window lost
            if let Some(text) = line
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
            {
                events.push((
                    (session.clone(), seq0 + 2),
                    Envelope::new(
                        &session,
                        ts,
                        Event::CompactSummary {
                            text: cap(text, FINAL_MSG_CAP),
                        },
                    ),
                ));
            }
            continue;
        }

        let ty = line.get("type").and_then(Value::as_str).unwrap_or("");
        if ty != "user" && ty != "assistant" {
            continue;
        }
        let Some(msg) = line.get("message") else {
            continue;
        };

        // content is either a plain string (user prompts) or a block array
        match msg.get("content") {
            Some(Value::String(text)) if ty == "user" => {
                events.push((
                    (session.clone(), seq0 + 2),
                    Envelope::new(
                        &session,
                        ts,
                        Event::UserMsg {
                            text: cap(text, USER_MSG_CAP),
                        },
                    ),
                ));
            }
            Some(Value::Array(blocks)) => {
                for (bi, block) in blocks.iter().enumerate().take(SEQ_STRIDE as usize - 2) {
                    let seq = seq0 + 2 + bi as u32;
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") if ty == "user" => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                events.push((
                                    (session.clone(), seq),
                                    Envelope::new(
                                        &session,
                                        ts,
                                        Event::UserMsg {
                                            text: cap(text, USER_MSG_CAP),
                                        },
                                    ),
                                ));
                            }
                        }
                        Some("text") if ty == "assistant" => {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                // substantive assistant messages are recallable
                                if text.len() >= SAID_MIN {
                                    events.push((
                                        (session.clone(), seq),
                                        Envelope::new(
                                            &session,
                                            ts,
                                            Event::Said {
                                                text: cap(text, SAID_CAP),
                                            },
                                        ),
                                    ));
                                }
                                // remember the last assistant text: it becomes
                                // FinalMsg (replacing its Said at the same seq)
                                final_msg = Some((seq, ts, text));
                            }
                        }
                        Some("tool_use") => {
                            let tool = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            // borrow: tool inputs can be arbitrarily large
                            // (Edit old/new strings, MCP payloads)
                            let input = block.get("input").unwrap_or(&Value::Null);
                            let detail = tool_detail(tool, input);
                            if let Some(id) = block.get("id").and_then(Value::as_str) {
                                call_sites.insert(id.to_string(), events.len());
                            }
                            events.push((
                                (session.clone(), seq),
                                Envelope::new(
                                    &session,
                                    ts,
                                    Event::ToolCall {
                                        tool: tool.to_string(),
                                        detail: cap(&detail, DETAIL_CAP),
                                        ok: true,
                                    },
                                ),
                            ));
                            // Edit/Write-family calls also touch a file
                            if let Some(path) = file_touch(tool, input) {
                                events.push((
                                    (session.clone(), seq0 + SEQ_STRIDE - 1),
                                    Envelope::new(&session, ts, Event::FileTouch { path }),
                                ));
                            }
                            // best-effort commit detection from git commit commands
                            if let Some((hash, message)) = commit_of(tool, input, line) {
                                events.push((
                                    (session.clone(), seq0 + SEQ_STRIDE - 2),
                                    Envelope::new(&session, ts, Event::Commit { hash, message }),
                                ));
                            }
                        }
                        Some("tool_result") => {
                            let err = block.get("is_error").and_then(Value::as_bool)
                                == Some(true)
                                || line
                                    .get("toolUseResult")
                                    .and_then(|r| r.get("is_error"))
                                    .and_then(Value::as_bool)
                                    == Some(true);
                            if err
                                && let Some(id) =
                                    block.get("tool_use_id").and_then(Value::as_str)
                                    && let Some(&i) = call_sites.get(id)
                                        && let Event::ToolCall { ok, .. } =
                                            &mut events[i].1.kind
                                        {
                                            *ok = false;
                                        }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if let Some((seq, ts, text)) = final_msg {
        // the closing message is FinalMsg, not Said — drop the duplicate
        events.retain(|(id, e)| !(id.1 == seq && matches!(e.kind, Event::Said { .. })));
        events.push((
            (session.clone(), seq),
            Envelope::new(
                &session,
                ts,
                Event::FinalMsg {
                    text: cap(text, FINAL_MSG_CAP),
                },
            ),
        ));
    }

    Some(Parsed { session, events })
}

fn tool_detail(tool: &str, input: &Value) -> String {
    match tool {
        "Bash" => input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "Read" | "Edit" | "Write" | "NotebookEdit" => input
            .get("file_path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => {
            let s = input.to_string();
            if s == "null" { String::new() } else { s }
        }
    }
}

fn file_touch(tool: &str, input: &Value) -> Option<String> {
    matches!(tool, "Edit" | "Write" | "NotebookEdit")
        .then(|| input.get("file_path")?.as_str().map(String::from))
        .flatten()
}

/// `git commit` in a Bash command -> (hash, message). Hash is best-effort
/// (empty when unparseable); the message comes from `-m "..."`.
fn commit_of(tool: &str, input: &Value, line: &Value) -> Option<(String, String)> {
    if tool != "Bash" {
        return None;
    }
    let cmd = input.get("command")?.as_str()?;
    if !cmd.contains("git commit") && !cmd.contains("jj describe") {
        return None;
    }
    let message = cmd
        .split_once("-m")
        .map(|(_, rest)| {
            let rest = rest.trim_start();
            let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
            match quote {
                Some(q) => rest[1..].split(q).next().unwrap_or("").to_string(),
                None => rest.split_whitespace().next().unwrap_or("").to_string(),
            }
        })
        .unwrap_or_default();
    // stdout like "[main abc1234] msg" if the result rode along on this line
    let hash = line
        .get("toolUseResult")
        .and_then(|r| r.get("stdout"))
        .and_then(Value::as_str)
        .and_then(|out| {
            let i = out.find('[')?;
            out[i..].split_whitespace().nth(1).map(|h| {
                h.trim_end_matches(']')
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .collect::<String>()
            })
        })
        .unwrap_or_default();
    Some((hash, cap(&message, 200)))
}

/// Replace any parsed `FinalMsg` with the hook-provided closing message
/// (`Stop` passes `last_assistant_message`, which is authoritative — the
/// transcript file may lag the final turn).
pub fn override_final_msg(parsed: &mut Parsed, text: &str) {
    parsed
        .events
        .retain(|(_, e)| !matches!(e.kind, Event::FinalMsg { .. }));
    let ts = parsed.events.iter().map(|(_, e)| e.ts_ms).max().unwrap_or(0);
    parsed.events.push((
        (parsed.session.clone(), crate::event::HOOK_FINAL_SEQ),
        Envelope::new(
            &parsed.session,
            ts,
            Event::FinalMsg {
                text: cap(text, FINAL_MSG_CAP),
            },
        ),
    ));
}

/// "2026-08-10T19:18:11.311Z" -> unix ms. Hand-rolled to keep peat
/// dependency-light; returns None on anything malformed.
pub fn iso_to_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<u64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let ms = if b.get(19) == Some(&b'.') {
        num(20..23).unwrap_or(0)
    } else {
        0
    };
    // days-from-civil (Howard Hinnant), valid for all dates we will ever see
    let (y, mo) = if mo <= 2 { (y - 1, mo + 12) } else { (y, mo) };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * (mo - 3) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + sec * 1000 + ms)
}

/// Unix ms -> "YYYY-MM-DD" (UTC). Inverse of [`iso_to_ms`]'s date part;
/// the civil-days constants are stated only in this file.
pub fn date_label(ms: u64) -> String {
    let days = ms / 86_400_000;
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

/// Local UTC offset in ms via `date +%z`. Render/capture boundary only —
/// the fold path never sees timezones. Zero (UTC semantics) on failure.
pub fn local_offset_ms() -> i64 {
    let Ok(out) = std::process::Command::new("date").arg("+%z").output() else {
        return 0;
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let s = s.trim();
    if s.len() != 5 {
        return 0;
    }
    let sign = if s.starts_with('-') { -1 } else { 1 };
    match (s[1..3].parse::<i64>(), s[3..5].parse::<i64>()) {
        (Ok(h), Ok(m)) => sign * (h * 60 + m) * 60_000,
        _ => 0,
    }
}

/// A local-day instant for `date` (YYYY-MM-DD) at `time` ("12:00:00.000"
/// or "23:59:59.999"): shared by `obs --at` and `asof`.
pub fn local_day_ms(date: &str, time: &str) -> Option<u64> {
    iso_to_ms(&format!("{date}T{time}Z")).map(|utc| (utc as i64 - local_offset_ms()) as u64)
}
