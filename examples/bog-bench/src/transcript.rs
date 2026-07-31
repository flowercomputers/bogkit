//! Claude Code transcript parsing: JSONL on disk → `Vec<ToolCall>`.
//!
//! One transcript is a stream of JSON objects, one per line. A tool invocation
//! is split across two of them: an assistant record carrying a `tool_use`
//! content block, and a later user record carrying the matching `tool_result`.
//! They are joined on the `tool_use` id. Everything else in the file — usage
//! accounting, attachments, mode records, queue operations — is ignored.
//!
//! Every failure mode here degrades rather than aborts. Invalid UTF-8 is
//! lossy-converted, unparseable lines are skipped, and a file that yields
//! nothing returns an empty `Vec`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::toolcall::{Outcome, ToolCall};

/// A `tool_use` block waiting for its result.
struct Pending {
    tool: String,
    at_ms: u64,
}

/// Recoverable parser problems observed while reading one transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseDiagnostics {
    pub lines: usize,
    pub malformed_json_lines: usize,
    pub non_object_lines: usize,
    pub malformed_tool_uses: usize,
    pub unmatched_results: usize,
    pub unmatched_calls: usize,
    pub invalid_utf8: bool,
    pub read_error: Option<String>,
}

impl ParseDiagnostics {
    pub fn has_issues(&self) -> bool {
        self.malformed_json_lines > 0
            || self.non_object_lines > 0
            || self.malformed_tool_uses > 0
            || self.unmatched_results > 0
            || self.unmatched_calls > 0
            || self.invalid_utf8
            || self.read_error.is_some()
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.malformed_json_lines > 0 {
            parts.push(counted(self.malformed_json_lines, "malformed JSON line"));
        }
        if self.non_object_lines > 0 {
            parts.push(counted(self.non_object_lines, "non-object JSON line"));
        }
        if self.malformed_tool_uses > 0 {
            parts.push(counted(
                self.malformed_tool_uses,
                "malformed tool_use block",
            ));
        }
        if self.unmatched_results > 0 {
            parts.push(counted(self.unmatched_results, "unmatched tool result"));
        }
        if self.unmatched_calls > 0 {
            parts.push(counted(self.unmatched_calls, "unknown outcome"));
        }
        if self.invalid_utf8 {
            parts.push("invalid UTF-8 replaced lossily".to_string());
        }
        if let Some(error) = &self.read_error {
            parts.push(format!("read error: {error}"));
        }
        parts.join(" · ")
    }
}

fn counted(n: usize, singular: &str) -> String {
    format!("{n} {singular}{}", if n == 1 { "" } else { "s" })
}

/// Calls plus diagnostics and the canonical identity used for persistence.
#[derive(Debug, Clone)]
pub struct ParsedSession {
    pub session: String,
    pub calls: Vec<ToolCall>,
    pub diagnostics: ParseDiagnostics,
}

/// Stable identity for an on-disk transcript.
///
/// Canonical paths keep same-stem transcripts in different projects distinct.
/// The absolute fallback lets `retract` find an already-ingested snapshot even
/// if the source file was deleted after ingest.
pub fn session_key(path: &Path) -> String {
    fs::canonicalize(path)
        .or_else(|_| {
            if path.is_absolute() {
                Ok(path.to_path_buf())
            } else {
                std::env::current_dir().map(|cwd| cwd.join(path))
            }
        })
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Parse one Claude Code transcript into its tool calls.
///
/// Malformed lines are skipped, never fatal. A file that yields nothing
/// returns an empty `Vec` rather than an error. Calls still unmatched at
/// end-of-input are kept with an unknown outcome — the invocation happened,
/// but the transcript does not prove success or failure.
pub fn parse_session(path: &Path) -> Vec<ToolCall> {
    parse_session_report(path).calls
}

pub fn parse_session_report(path: &Path) -> ParsedSession {
    let session = session_key(path);
    let mut diagnostics = ParseDiagnostics::default();

    // Transcripts are known to contain invalid UTF-8 (truncated tool output,
    // mid-codepoint splits). Read bytes and lossy-convert; never panic.
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(error) => {
            diagnostics.read_error = Some(error.to_string());
            return ParsedSession {
                session,
                calls: Vec::new(),
                diagnostics,
            };
        }
    };
    diagnostics.invalid_utf8 = std::str::from_utf8(&bytes).is_err();
    let text = String::from_utf8_lossy(&bytes);

    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut calls: Vec<ToolCall> = Vec::new();

    for line in text.lines() {
        diagnostics.lines += 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                diagnostics.malformed_json_lines += 1;
                continue;
            }
        };
        let entry = match entry.as_object() {
            Some(o) => o,
            None => {
                diagnostics.non_object_lines += 1;
                continue;
            }
        };

        let at_ms = entry.get("timestamp").and_then(iso8601_to_epoch_ms);

        // `message.content` is sometimes a string (plain prose turn), sometimes
        // an array of content blocks, sometimes absent entirely.
        let content = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());

        if let Some(blocks) = content {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let (Some(id), Some(name)) = (
                    block.get("id").and_then(Value::as_str),
                    block.get("name").and_then(Value::as_str),
                ) else {
                    diagnostics.malformed_tool_uses += 1;
                    continue;
                };
                pending.insert(
                    id.to_string(),
                    Pending {
                        tool: name.to_string(),
                        at_ms: at_ms.unwrap_or(0),
                    },
                );
            }
        }

        // Results arrive as `tool_result` blocks. Older/alternate records carry
        // the join key at the top level as `toolUseID` with no content block,
        // so fall back to a single synthetic blockless result in that case.
        let mut result_blocks: Vec<Option<&Value>> = Vec::new();
        if let Some(blocks) = content {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    result_blocks.push(Some(block));
                }
            }
        }
        if result_blocks.is_empty() && entry.contains_key("toolUseID") {
            result_blocks.push(None);
        }

        for block in result_blocks {
            let Some(id) = result_id(entry, block) else {
                diagnostics.unmatched_results += 1;
                continue;
            };
            let Some(started) = pending.remove(id) else {
                diagnostics.unmatched_results += 1;
                continue;
            };

            // Block-local `content` wins over the top-level `toolUseResult`
            // mirror; they can disagree, and the block is what the model saw.
            let payload = block
                .and_then(|b| b.get("content"))
                .or_else(|| entry.get("toolUseResult"));

            let outcome = if is_error(block, payload) {
                Outcome::ExplicitError
            } else {
                Outcome::Success
            };
            let duration_ms = match (at_ms, started.at_ms) {
                (Some(end), start) if start > 0 => end.saturating_sub(start),
                _ => 0,
            };

            calls.push(ToolCall {
                session: session.clone(),
                tool: started.tool,
                outcome,
                result_chars: payload.map(result_len).unwrap_or(0),
                duration_ms,
                at_ms: started.at_ms,
            });
        }
    }

    // Drain: a call whose result never arrived (session ended mid-flight, or
    // the result line was malformed) is still a call that was made.
    diagnostics.unmatched_calls = pending.len();
    for (_, started) in pending {
        calls.push(ToolCall {
            session: session.clone(),
            tool: started.tool,
            outcome: Outcome::Unknown,
            result_chars: 0,
            duration_ms: 0,
            at_ms: started.at_ms,
        });
    }

    ParsedSession {
        session,
        calls,
        diagnostics,
    }
}

/// Find Claude transcripts under a root, newest first, capped at `limit`.
///
/// Walks recursively — subagent transcripts live one level deeper, under
/// `<session-uuid>/subagents/`, and count as sessions in their own right.
pub fn discover(root: &Path, limit: usize) -> Vec<PathBuf> {
    if limit == 0 {
        return Vec::new();
    }

    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                found.push((mtime, path));
            }
        }
    }

    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.truncate(limit);
    found.into_iter().map(|(_, p)| p).collect()
}

/// Join key: block-local `tool_use_id` first, else the top-level `toolUseID`.
fn result_id<'a>(
    entry: &'a serde_json::Map<String, Value>,
    block: Option<&'a Value>,
) -> Option<&'a str> {
    if let Some(id) = block
        .and_then(|b| b.get("tool_use_id"))
        .and_then(Value::as_str)
    {
        return Some(id);
    }
    entry.get("toolUseID").and_then(Value::as_str)
}

/// A result is a failure when the producer said so, or when the payload is
/// shaped like an error envelope. `is_error` is often absent, never inferred.
fn is_error(block: Option<&Value>, payload: Option<&Value>) -> bool {
    if block
        .and_then(|b| b.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    payload
        .and_then(|p| p.get("is_error"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Characters in a tool-result payload, normalised across its four shapes:
/// a plain string, a list of content blocks, a `{"content": [...]}` envelope,
/// or an arbitrary JSON object.
fn result_len(payload: &Value) -> u64 {
    match payload {
        Value::Null => 0,
        Value::String(s) => s.chars().count() as u64,
        Value::Array(blocks) => block_list_len(blocks),
        Value::Object(map) => match map.get("content") {
            Some(Value::Array(blocks)) => block_list_len(blocks),
            Some(Value::String(s)) => s.chars().count() as u64,
            _ => payload.to_string().chars().count() as u64,
        },
        other => other.to_string().chars().count() as u64,
    }
}

fn block_list_len(blocks: &[Value]) -> u64 {
    blocks
        .iter()
        .map(|block| match block {
            Value::String(s) => s.chars().count() as u64,
            Value::Object(map) => match map.get("text") {
                Some(Value::String(s)) => s.chars().count() as u64,
                _ => block.to_string().chars().count() as u64,
            },
            other => other.to_string().chars().count() as u64,
        })
        .sum()
}

/// `2026-07-25T20:13:47.933Z` → epoch milliseconds.
///
/// Hand-rolled rather than pulling in `chrono`: the producer emits one shape,
/// and anything that does not match it degrades to `None` (which becomes a
/// zero timestamp downstream) instead of failing the parse.
fn iso8601_to_epoch_ms(value: &Value) -> Option<u64> {
    let s = value.as_str()?;
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[10] != b'T' {
        return None;
    }

    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse::<i64>().ok() };

    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let min = num(14, 16)?;
    let sec = num(17, 19)?;

    // Optional `.mmm` fraction, then an optional `Z` or `±HH:MM` offset.
    let mut idx = 19;
    let mut millis = 0i64;
    if bytes.get(idx) == Some(&b'.') {
        idx += 1;
        let mut digits = 0;
        while digits < 3 {
            match bytes.get(idx + digits) {
                Some(b) if b.is_ascii_digit() => {
                    millis = millis * 10 + (b - b'0') as i64;
                    digits += 1;
                }
                _ => break,
            }
        }
        // Left-align a short fraction: ".5" is 500ms, not 5ms.
        for _ in digits..3 {
            millis *= 10;
        }
        idx += digits;
        while matches!(bytes.get(idx), Some(b) if b.is_ascii_digit()) {
            idx += 1;
        }
    }

    let mut offset_secs = 0i64;
    match bytes.get(idx) {
        Some(b'+') | Some(b'-') => {
            let sign = if bytes[idx] == b'-' { -1 } else { 1 };
            let oh = num(idx + 1, idx + 3)?;
            let om = num(idx + 4, idx + 6).unwrap_or(0);
            offset_secs = sign * (oh * 3600 + om * 60);
        }
        _ => {}
    }

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec - offset_secs;
    let ms = secs * 1000 + millis;
    u64::try_from(ms).ok()
}

/// Days since the Unix epoch for a proleptic-Gregorian civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn epoch_conversion_matches_known_instants() {
        let at = |s: &str| iso8601_to_epoch_ms(&Value::String(s.to_string()));
        assert_eq!(at("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(at("2026-07-25T20:13:47.933Z"), Some(1_785_010_427_933));
        assert_eq!(at("2026-07-25T20:13:47Z"), Some(1_785_010_427_000));
        // Offsets are normalised to UTC, not ignored.
        assert_eq!(
            at("2026-07-25T16:13:47.933-04:00"),
            at("2026-07-25T20:13:47.933Z")
        );
        assert_eq!(at("not-a-timestamp"), None);
        assert_eq!(at(""), None);
    }

    #[test]
    fn result_len_handles_every_payload_shape() {
        assert_eq!(result_len(&serde_json::json!("hello")), 5);
        assert_eq!(result_len(&serde_json::json!(null)), 0);
        assert_eq!(
            result_len(&serde_json::json!([{"type": "text", "text": "abcd"}, "ef"])),
            6
        );
        assert_eq!(
            result_len(&serde_json::json!({"content": [{"text": "abc"}]})),
            3
        );
        // Multi-byte text counts characters, not bytes.
        assert_eq!(result_len(&serde_json::json!("héllo")), 5);
    }

    #[test]
    fn joins_a_synthetic_transcript() {
        let dir = std::env::temp_dir().join("bog-bench-parser-test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("11111111-2222-3333-4444-555555555555.jsonl");
        let lines = [
            r#"{"type":"mode","sessionId":"s"}"#,
            r#"{ not json at all"#,
            r#"{"type":"assistant","timestamp":"2026-07-25T20:13:47.000Z","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Read"},{"type":"tool_use","id":"toolu_B","name":"Bash"}]}}"#,
            r#"{"type":"user","timestamp":"2026-07-25T20:13:48.500Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"0123456789"}]}}"#,
            r#"{"type":"user","timestamp":"2026-07-25T20:13:49.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_B","content":"boom","is_error":true}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-07-25T20:13:50.000Z","message":{"content":[{"type":"tool_use","id":"toolu_C","name":"Edit"}]}}"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();

        let parsed = parse_session_report(&path);
        let calls = parsed.calls;
        assert_eq!(calls.len(), 3, "two joined calls plus one drained orphan");
        assert_eq!(calls[0].session, session_key(&path));
        assert_eq!(parsed.diagnostics.malformed_json_lines, 1);
        assert_eq!(parsed.diagnostics.unmatched_calls, 1);

        assert_eq!(calls[0].tool, "Read");
        assert_eq!(calls[0].outcome, Outcome::Success);
        assert_eq!(calls[0].result_chars, 10);
        assert_eq!(calls[0].duration_ms, 1500);
        assert_eq!(calls[0].at_ms, 1_785_010_427_000);

        assert_eq!(calls[1].tool, "Bash");
        assert_eq!(calls[1].outcome, Outcome::ExplicitError);

        assert_eq!(calls[2].tool, "Edit");
        assert_eq!(calls[2].outcome, Outcome::Unknown);
        assert_eq!(calls[2].result_chars, 0, "orphan carries no result");

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_empty_not_a_panic() {
        assert!(parse_session(Path::new("/nonexistent/nope.jsonl")).is_empty());
    }

    /// The real thing: parse actual transcripts off disk and prove the join
    /// produces plausible data. Prints a tool histogram for cross-checking.
    /// Skips (rather than fails) on a machine with no transcripts.
    #[test]
    fn parses_real_transcripts() {
        let Some(home) = std::env::var_os("HOME") else {
            eprintln!("SKIP: no HOME");
            return;
        };
        let root = PathBuf::from(home).join(".claude/projects");
        if !root.is_dir() {
            eprintln!("SKIP: {} not present", root.display());
            return;
        }

        let paths = discover(&root, 40);
        assert!(!paths.is_empty(), "discover found no transcripts");

        let mut calls = Vec::new();
        for path in &paths {
            calls.extend(parse_session(path));
        }
        assert!(!calls.is_empty(), "no tool calls parsed from real data");

        let mut hist: BTreeMap<&str, (u64, u64, u64)> = BTreeMap::new();
        for call in &calls {
            let slot = hist.entry(call.tool.as_str()).or_default();
            slot.0 += 1;
            slot.1 += call.result_chars;
            if call.outcome == Outcome::ExplicitError {
                slot.2 += 1;
            }
        }

        let timed = calls.iter().filter(|c| c.duration_ms > 0).count();
        let stamped = calls.iter().filter(|c| c.at_ms > 0).count();
        let sized = calls.iter().filter(|c| c.result_chars > 0).count();

        eprintln!(
            "\n{} sessions → {} tool calls  |  {} timed, {} stamped, {} with output\n",
            paths.len(),
            calls.len(),
            timed,
            stamped,
            sized
        );
        let mut rows: Vec<_> = hist.into_iter().collect();
        rows.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.0));
        eprintln!(
            "{:<28} {:>7} {:>12} {:>9}",
            "tool", "calls", "avg chars", "errors"
        );
        for (tool, (n, chars, errs)) in rows.iter().take(25) {
            eprintln!(
                "{:<28} {:>7} {:>12} {:>9}",
                tool,
                n,
                chars / n.max(&1),
                errs
            );
        }
        eprintln!();

        // Sanity: real transcripts always contain reads, and timestamps parse.
        assert!(
            stamped > calls.len() / 2,
            "most calls should carry a timestamp"
        );
        assert!(sized > 0, "no call produced any output — join is broken");
    }
}
