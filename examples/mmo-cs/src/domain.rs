//! Plain data types shared across the battle pipeline, the wire protocol,
//! and the web layer.

use serde::{Deserialize, Serialize};

pub type PlayerId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Team {
    CourtSquare,
    ChurchAve,
}

impl Team {
    pub fn label(self) -> &'static str {
        match self {
            Team::CourtSquare => "Team Court Square",
            Team::ChurchAve => "Team Church Ave",
        }
    }

    pub fn opponent(self) -> Team {
        match self {
            Team::CourtSquare => Team::ChurchAve,
            Team::ChurchAve => Team::CourtSquare,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bbox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

impl Bbox {
    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        lon >= self.min_lon && lon <= self.max_lon && lat >= self.min_lat && lat <= self.max_lat
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Park {
    pub id: String,
    pub name: String,
    pub bbox: Bbox,
}

/// Roster membership fact: written once at join time (or replaced while the
/// battle is still `Pending`), permanent for the battle's lifetime. Never
/// subject to `Retain`/expiry — the win condition's denominator is "% of a
/// team's members", not "% of currently-pinging members".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterEntry {
    pub team: Team,
    pub joined_at_ms: u64,
}

/// One GPS sample. `KeyedStream::upsert` per `PlayerId` means each new ping
/// supersedes the player's previous ping and restarts its liveness clock
/// (see `Retain` in battle.rs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationPing {
    pub team: Team,
    pub lat: f64,
    pub lon: f64,
    pub client_ms: u64,
}

/// Per-team accumulator for the live-presence `Aggregate`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PresenceCounts {
    pub pinging: i64,
    pub in_bounds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BattleStatus {
    Pending,
    Active,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BattleOutcome {
    Elimination { winner: Team },
    Timeout { winner: Team },
    Tie,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Battle {
    pub id: u64,
    pub park: Park,
    pub status: BattleStatus,
    pub started_at_ms: Option<u64>,
    pub ends_at_ms: Option<u64>,
    pub outcome: Option<BattleOutcome>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct TeamStats {
    pub members: u32,
    pub pinging: u32,
    pub in_bounds: u32,
}

/// What every client sees: the whole game is rendered from this one
/// snapshot, broadcast after every state-affecting commit.
#[derive(Debug, Clone, Serialize)]
pub struct Scoreboard {
    pub battle: Battle,
    pub court_square: TeamStats,
    pub church_ave: TeamStats,
}
