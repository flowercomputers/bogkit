//! The contract between the transcript parser and the fold pipeline.
//!
//! This struct is frozen: `transcript.rs` produces these, `main.rs` folds
//! them. Neither side needs to know anything else about the other.

use serde::{Deserialize, Serialize};

/// What the transcript proves about a tool call's result.
///
/// A call with no matching result is not evidence of success. Keeping that
/// state explicit prevents incomplete or malformed transcripts from diluting
/// the explicit-error rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Outcome {
    Success,
    ExplicitError,
    Unknown,
}

/// One tool invocation, joined from its request and its result.
///
/// A single agent turn may issue several of these. Every field is populated
/// by the parser; the pipeline never reaches back into the transcript.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolCall {
    /// Session this call belongs to — the transcript file's stem. Retraction
    /// is per-session, so this is what makes "un-ingest that session" work.
    pub session: String,
    /// Tool name as the agent invoked it (`Read`, `Bash`, `Edit`, …).
    pub tool: String,
    /// Success, an explicitly flagged error, or no observed result.
    pub outcome: Outcome,
    /// Characters in the joined tool result — the measured quantity, and the
    /// one we rank on. Counts characters rather than bytes so the estimate
    /// below stays honest on non-ASCII output.
    pub result_chars: u64,
    /// Wall-clock ms between request and result, 0 when untimed.
    pub duration_ms: u64,
    /// Epoch ms of the request, used for the rolling window.
    pub at_ms: u64,
}

impl ToolCall {
    /// Characters of tool-result payload — what this call cost the context
    /// window, in the one unit we can measure exactly for every call.
    pub fn cost(&self) -> u64 {
        self.result_chars
    }
}

/// Running per-tool accumulator. `delta` is +1 on insert, -1 on retraction,
/// so every field rolls back exactly when a session is removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStats {
    pub calls: i64,
    /// Results carrying an explicit producer error flag.
    pub failures: i64,
    /// Calls for which no matching result was observed.
    pub unknowns: i64,
    /// Measured: characters of tool-result payload.
    pub result_chars: i64,
    pub duration_ms: i64,
}

impl ToolStats {
    pub fn known_outcomes(&self) -> i64 {
        self.calls - self.unknowns
    }

    /// Explicit-error rate among calls with a known outcome.
    pub fn failure_rate(&self) -> f64 {
        let known = self.known_outcomes();
        if known == 0 {
            0.0
        } else {
            self.failures as f64 / known as f64
        }
    }

    /// Rough token estimate by the `chars / 4` convention.
    ///
    /// This is an **estimate, not a measurement**, and deliberately not what
    /// the leaderboard ranks on — `result_chars` is. Transcripts do carry real
    /// usage, but it cannot be attributed per call: prompt caching drives
    /// `input_tokens` to near zero (6, against 25 803 cache-read, in a typical
    /// turn), so a tool's true marginal cost is only recoverable by diffing
    /// total context between consecutive turns, and only for turns holding
    /// exactly one tool call. Characters are what we can honestly measure for
    /// every call, so characters are what we rank on.
    ///
    /// The estimate holds for ASCII-ish code and prose. It undercounts CJK
    /// (roughly one token per character) and punctuation-dense JSON.
    pub fn est_tokens(&self) -> i64 {
        self.result_chars / 4
    }
}

/// The fold step function: applied on insert with `delta = 1` and on
/// retraction with `delta = -1`.
pub fn tool_step(acc: &mut ToolStats, call: &ToolCall, delta: isize) {
    let d = delta as i64;
    acc.calls += d;
    acc.failures += if call.outcome == Outcome::ExplicitError {
        d
    } else {
        0
    };
    acc.unknowns += if call.outcome == Outcome::Unknown {
        d
    } else {
        0
    };
    acc.result_chars += call.cost() as i64 * d;
    acc.duration_ms += call.duration_ms as i64 * d;
}
