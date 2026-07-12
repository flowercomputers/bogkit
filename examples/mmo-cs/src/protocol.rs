//! The websocket wire protocol.
//!
//! Client -> server messages are one of a few actions. Server -> client is
//! just a serialized [`Scoreboard`](crate::domain::Scoreboard) — no
//! separate "battle started"/"battle ended" event type: a `watch` channel
//! only ever holds its latest value, so discrete one-shot events would be
//! lossy if two landed back-to-back. The client instead diffs
//! `battle.status`/`battle.outcome` across incoming scoreboards itself to
//! notice transitions, which is never lossy since it's driven off the
//! always-current value.
//!
//! `Ping` deliberately omits `team`: the server looks it up from the
//! player's own roster entry rather than trusting a client-supplied value,
//! so a ping can't claim a team the player didn't actually join.

use serde::Deserialize;

use crate::domain::{PlayerId, Team};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Picks the battleground and match length for a battle that hasn't
    /// been configured yet (`Battle::park.is_none()`). First one in wins —
    /// once a battle has a park, later `ConfigureBattle`s are ignored, the
    /// same "locks once set" discipline `Join` uses against `Active`.
    ConfigureBattle { park_id: String, duration_secs: u64 },
    Join { player: PlayerId, team: Team },
    Ping { player: PlayerId, lat: f64, lon: f64, client_ms: u64 },
    StartBattle,
}
