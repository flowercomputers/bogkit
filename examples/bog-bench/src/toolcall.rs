//! The contract between the transcript parser and the fold pipeline.
//!
//! This struct is frozen: `transcript.rs` produces these, `main.rs` folds
//! them. Neither side needs to know anything else about the other.

use serde::{Deserialize, Serialize};

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
    /// False when the result carried an error flag.
    pub ok: bool,
    /// Characters in the joined tool result. Context cost is `chars / 4`,
    /// matching the tool-benchmarks methodology.
    pub result_chars: u64,
    /// Wall-clock ms between request and result, 0 when untimed.
    pub duration_ms: u64,
    /// Epoch ms of the request, used for the rolling window.
    pub at_ms: u64,
}

impl ToolCall {
    /// Context cost in tokens, by the `chars / 4` convention.
    pub fn context_tokens(&self) -> u64 {
        self.result_chars / 4
    }
}

/// Running per-tool accumulator. `delta` is +1 on insert, -1 on retraction,
/// so every field rolls back exactly when a session is removed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolStats {
    pub calls: i64,
    pub failures: i64,
    pub context_tokens: i64,
    pub duration_ms: i64,
}

impl ToolStats {
    pub fn failure_rate(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.failures as f64 / self.calls as f64
        }
    }

    pub fn avg_tokens(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.context_tokens as f64 / self.calls as f64
        }
    }
}

/// The fold step function: applied on insert with `delta = 1` and on
/// retraction with `delta = -1`.
pub fn tool_step(acc: &mut ToolStats, call: &ToolCall, delta: isize) {
    let d = delta as i64;
    acc.calls += d;
    acc.failures += if call.ok { 0 } else { d };
    acc.context_tokens += call.context_tokens() as i64 * d;
    acc.duration_ms += call.duration_ms as i64 * d;
}
