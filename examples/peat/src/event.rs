//! The ledger schema — peat's API to its own past.
//!
//! Every fact peat ever records is an [`Envelope`] stored under an
//! [`EventId`]. Replay-from-genesis is the recovery and time-travel story,
//! so every envelope ever written must parse forever: evolution is
//! additive-only (new variants, new optional fields), never in-place.

use serde::{Deserialize, Serialize};

/// Bumped when the schema changes shape. Written into every envelope.
pub const EVENT_VERSION: u16 = 1;

pub type SessionId = String;

/// `(session, seq)`. Seq is the transcript entry index for captured events;
/// observations use `OBS_SEQ_BASE + n` so the two ranges never collide.
/// Upserting the same id twice is a no-op by construction — re-running
/// `peat capture` on the same transcript is the crash-recovery story.
pub type EventId = (SessionId, u32);

pub const OBS_SEQ_BASE: u32 = 100_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// [`EVENT_VERSION`] at write time.
    pub v: u16,
    /// Milliseconds since epoch, from the transcript or the caller —
    /// never read from the wall clock inside any fold path.
    pub ts_ms: u64,
    pub session: SessionId,
    pub kind: Event,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Event {
    // ---- mechanical exhaust (cannot lie) ----
    /// Once per captured session; pins the embedding provenance.
    SessionMeta {
        cwd: String,
        branch: Option<String>,
        /// ese crate version + dimension features at capture time. A brief
        /// or replay running a different ese would re-embed queries with a
        /// different model than indexed the text — warn loudly.
        ese_version: String,
    },
    /// What the user asked. Truncated to [`USER_MSG_CAP`].
    UserMsg { text: String },
    /// One tool invocation. `detail` is the command line / file path,
    /// truncated to [`DETAIL_CAP`].
    ToolCall { tool: String, detail: String, ok: bool },
    /// A file mutated via Edit/Write/NotebookEdit.
    FileTouch { path: String },
    Commit { hash: String, message: String },
    /// The agent's closing message — a free session summary.
    FinalMsg { text: String },
    Compaction {},

    // ---- the one judgment step ----
    /// A small claim the agent chose to record. `derived_from` cites the
    /// seqs of mechanical events it rests on; empty means a bare assertion,
    /// and readers are told so.
    Obs {
        subject: String,
        text: String,
        derived_from: Vec<u32>,
    },
}

pub const USER_MSG_CAP: usize = 2048;
pub const FINAL_MSG_CAP: usize = 8192;
pub const DETAIL_CAP: usize = 500;

/// Truncate on a char boundary at `cap` bytes.
pub fn cap(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

impl Envelope {
    pub fn new(session: &str, ts_ms: u64, kind: Event) -> Self {
        Envelope {
            v: EVENT_VERSION,
            ts_ms,
            session: session.to_string(),
            kind,
        }
    }
}
