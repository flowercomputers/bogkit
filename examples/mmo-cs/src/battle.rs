//! Owns the fold streams and runs one battle after another.
//!
//! Shape (stolen from `examples/chat`):
//!
//!   ws clients -> mpsc -> ingest thread (owns both fold streams) -> watch -> ws clients
//!
//! One plain thread owns two independent `KeyedStream`s — a team roster
//! (permanent for the battle) and a location-ping presence stream
//! (`Retain`-bounded liveness) — and does all writes. Every write commits a
//! transaction, re-derives one consistent [`Scoreboard`], and publishes it
//! on a `watch` channel; every websocket task just forwards scoreboards to
//! its client. No locks, no async database code.
//!
//! Both streams' pipeline types contain a closure (the presence pipeline
//! captures the current battle's bounding box), so — like `chat`'s and
//! `search`'s pipelines — they can't be named as a function parameter or
//! return type. Everything that touches them (message handling, win-
//! condition evaluation, snapshotting) is inlined directly in [`run`]
//! instead of factored into helper functions, for the same reason.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fold::pipeline::{Aggregate, KeyBy, Keyed, Retain, terminal::Table};
use fold::stream::KeyedStream;
use tokio::sync::watch;

use crate::domain::{
    Battle, BattleOutcome, BattleStatus, LocationPing, PlayerId, PresenceCounts, RosterEntry,
    Scoreboard, Team, TeamStats,
};
use crate::parks;
use crate::protocol::ClientMsg;

/// A player's ping counts as live only if it arrived within this long.
const PRESENCE_HORIZON: Duration = Duration::from_secs(45);
/// A team must read as fully swept for this long, continuously, before the
/// instant elimination win is confirmed — smooths over one flaky/late fix.
const ELIMINATION_CONFIRM_MS: u64 = 60_000;
/// How long a battle runs before the timeout win condition decides it.
/// Overridable via `MMO_BATTLE_DURATION_SECS` for testing — a real battle
/// takes 3 hours to time out, far too long to exercise by hand.
const DEFAULT_BATTLE_DURATION_MS: u64 = 3 * 60 * 60 * 1000;
/// How often the ingest loop wakes up with no client message, purely to
/// advance `Retain`'s clock (an idle stream never self-expires stale pings).
const TICK: Duration = Duration::from_secs(5);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Owns the fold stores for the whole process lifetime: runs one battle to
/// completion, then immediately opens a fresh one so the server never needs
/// a restart between battles.
pub fn run(data_dir: &Path, rx: mpsc::Receiver<ClientMsg>, state_tx: watch::Sender<Scoreboard>) {
    let battle_duration_ms = std::env::var("MMO_BATTLE_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|secs| secs * 1000)
        .unwrap_or(DEFAULT_BATTLE_DURATION_MS);

    let mut next_id = 1u64;

    loop {
        let park = parks::pick_battleground();
        let mut battle = Battle {
            id: next_id,
            park: park.clone(),
            status: BattleStatus::Pending,
            started_at_ms: None,
            ends_at_ms: None,
            outcome: None,
        };
        let battle_dir = data_dir.join(format!("battle-{next_id}"));
        next_id += 1;
        let _ = std::fs::remove_dir_all(&battle_dir);

        println!("battle {} pending at {}", battle.id, battle.park.name);

        // Roster pipeline: KeyBy(team) + Aggregate(count) -> Table<Team, i64>.
        // Permanent membership fact, never wrapped in Retain.
        let mut roster = KeyedStream::new(
            battle_dir.join("roster.db"),
            KeyBy::new(
                |d: &Keyed<PlayerId, RosterEntry>| d.val.team,
                Aggregate::new(
                    "roster_by_team",
                    |acc: &mut i64, _entry: &Keyed<PlayerId, RosterEntry>, delta: isize| {
                        *acc += delta as i64
                    },
                    Table::new("roster_counts"),
                ),
            ),
        );

        // Presence pipeline: Retain(liveness) -> KeyBy(team) + Aggregate(in/out
        // counts) -> Table<Team, PresenceCounts>. `bbox` is captured by the
        // step closure at battle-creation time, since it differs per battle.
        let bbox = park.bbox;
        let mut presence = KeyedStream::new(
            battle_dir.join("presence.db"),
            Retain::new(
                "presence_ttl",
                PRESENCE_HORIZON,
                KeyBy::new(
                    |d: &Keyed<PlayerId, LocationPing>| d.val.team,
                    Aggregate::new(
                        "presence_by_team",
                        move |acc: &mut PresenceCounts, ping: &Keyed<PlayerId, LocationPing>, delta: isize| {
                            acc.pinging += delta as i64;
                            if bbox.contains(ping.val.lon, ping.val.lat) {
                                acc.in_bounds += delta as i64;
                            }
                        },
                        Table::new("presence_counts"),
                    ),
                ),
            ),
        );

        // team -> instant since which that team has read as fully swept,
        // continuously; cleared the moment it's no longer swept
        let mut zero_since: HashMap<Team, u64> = HashMap::new();

        // Reads roster/presence directly rather than through a helper fn:
        // their pipeline types contain closures and can't be named as a
        // function parameter type (see module docs).
        macro_rules! roster_count {
            ($team:expr) => {
                roster.rtx(|t| t.get(&$team)).unwrap_or(0)
            };
        }
        macro_rules! presence_counts {
            ($team:expr) => {{
                let pc: PresenceCounts = presence.rtx(|t| t.get(&$team)).unwrap_or_default();
                pc
            }};
        }
        macro_rules! snapshot {
            () => {{
                let stats = |team: Team| {
                    let members = roster_count!(team).max(0) as u32;
                    let pc = presence_counts!(team);
                    TeamStats {
                        members,
                        pinging: pc.pinging.max(0) as u32,
                        in_bounds: pc.in_bounds.max(0) as u32,
                    }
                };
                Scoreboard {
                    battle: battle.clone(),
                    court_square: stats(Team::CourtSquare),
                    church_ave: stats(Team::ChurchAve),
                }
            }};
        }

        let _ = state_tx.send(snapshot!());

        loop {
            match rx.recv_timeout(TICK) {
                Ok(ClientMsg::Join { player, team }) => {
                    // roster locks once Active: no late joins / team-switches
                    // mid-battle, so a losing team can't stack reinforcements
                    if battle.status == BattleStatus::Pending {
                        roster.wtx(|tx| {
                            tx.upsert(&player, &RosterEntry { team, joined_at_ms: now_ms() })
                        });
                    }
                }
                Ok(ClientMsg::Ping { player, lat, lon, client_ms }) => {
                    // team is looked up server-side from the roster, never
                    // trusted from the client, so a ping can't claim a team
                    // the player didn't actually join
                    if battle.status == BattleStatus::Active
                        && let Some(entry) = roster.get(&player)
                    {
                        presence.wtx(|tx| {
                            tx.upsert(&player, &LocationPing { team: entry.team, lat, lon, client_ms })
                        });
                    }
                }
                Ok(ClientMsg::StartBattle) => {
                    if battle.status == BattleStatus::Pending {
                        let cs = roster_count!(Team::CourtSquare);
                        let ca = roster_count!(Team::ChurchAve);
                        if cs > 0 && ca > 0 {
                            battle.status = BattleStatus::Active;
                            let start = now_ms();
                            battle.started_at_ms = Some(start);
                            battle.ends_at_ms = Some(start + battle_duration_ms);
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // no-op write: advances Retain's clock, expiring stale pings
                    presence.wtx(|_| {});
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }

            // --- win-condition evaluation ---
            //
            // Total silence (`pinging == 0`) never counts toward elimination
            // — only a team that's still confirmed pinging, but confirmed
            // entirely outside the box, does. This avoids a shared dead zone
            // (both teams' phones losing signal together) falsely ending the
            // battle; a team that quietly goes silent can only lose via the
            // timeout path below, where its stale presence decays toward 0%
            // as `Retain` ages its last ping out.
            if battle.status == BattleStatus::Active {
                let now = now_ms();

                for team in Team::ALL {
                    let members = roster_count!(team);
                    let pc = presence_counts!(team);
                    let swept = members > 0 && pc.pinging > 0 && pc.in_bounds == 0;

                    if swept {
                        let since = *zero_since.entry(team).or_insert(now);
                        if now - since >= ELIMINATION_CONFIRM_MS {
                            battle.status = BattleStatus::Ended;
                            battle.outcome = Some(BattleOutcome::Elimination { winner: team.opponent() });
                            break;
                        }
                    } else {
                        zero_since.remove(&team);
                    }
                }

                // 3-hour timeout: higher in-bounds % of roster wins; ties
                // break on higher absolute in-bounds headcount; still tied
                // after that is a draw.
                if battle.status == BattleStatus::Active
                    && let Some(ends_at) = battle.ends_at_ms
                    && now >= ends_at
                {
                    let pct_and_count = |team: Team| -> (f64, i64) {
                        let members = roster_count!(team);
                        let pc = presence_counts!(team);
                        let pct = if members > 0 { pc.in_bounds as f64 / members as f64 } else { 0.0 };
                        (pct, pc.in_bounds)
                    };
                    let (cs_pct, cs_in) = pct_and_count(Team::CourtSquare);
                    let (ca_pct, ca_in) = pct_and_count(Team::ChurchAve);

                    battle.status = BattleStatus::Ended;
                    battle.outcome = Some(if cs_pct > ca_pct {
                        BattleOutcome::Timeout { winner: Team::CourtSquare }
                    } else if ca_pct > cs_pct {
                        BattleOutcome::Timeout { winner: Team::ChurchAve }
                    } else if cs_in > ca_in {
                        BattleOutcome::Timeout { winner: Team::CourtSquare }
                    } else if ca_in > cs_in {
                        BattleOutcome::Timeout { winner: Team::ChurchAve }
                    } else {
                        BattleOutcome::Tie
                    });
                }
            }

            let _ = state_tx.send(snapshot!());

            if battle.status == BattleStatus::Ended {
                let outcome = match battle.outcome {
                    Some(BattleOutcome::Elimination { winner }) => format!("{} wins by elimination", winner.label()),
                    Some(BattleOutcome::Timeout { winner }) => format!("{} wins on time", winner.label()),
                    Some(BattleOutcome::Tie) | None => "tie".to_string(),
                };
                println!("battle {} ended: {outcome}", battle.id);
                break;
            }
        }
    }
}
