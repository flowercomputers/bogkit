//! Owns the fold streams and runs one battle after another.
//!
//! Shape (stolen from `examples/chat`):
//!
//!   ws clients -> mpsc -> ingest thread (owns both fold streams) -> watch -> ws clients
//!
//! Each battle runs in two phases:
//!  1. **Awaiting configuration** — `battle.park` is `None`. Only the
//!     roster stream exists yet (it doesn't need a park), so the client
//!     shows a park-search + match-length menu; anyone's `ConfigureBattle`
//!     picks the battleground and unlocks phase 2. First one in wins —
//!     later `ConfigureBattle`s are ignored once a park is set.
//!  2. **Lobby through to game over** — the presence stream (which needs
//!     the chosen park's bbox baked into its aggregate closure) is built,
//!     and the existing join/start/ping/win-condition machinery runs.
//!
//! One plain thread owns both `KeyedStream`s — a team roster (permanent
//! for the battle) and a location-ping presence stream (`Retain`-bounded
//! liveness) — and does all writes. Every write commits a transaction,
//! re-derives one consistent [`Scoreboard`], and publishes it on a `watch`
//! channel; every websocket task just forwards scoreboards to its client.
//! No locks, no async database code.
//!
//! Both streams' pipeline types contain a closure (the presence pipeline
//! captures the current battle's bounding box), so — like `chat`'s and
//! `search`'s pipelines — they can't be named as a function parameter or
//! return type. Everything that touches them (message handling, snapshotting)
//! is inlined directly in [`run`] instead of factored into helper functions,
//! for the same reason. Win-condition *decisions* don't have that
//! restriction — they're plain data in and out, so that logic lives in
//! [`crate::win`] instead, where it's unit-testable on its own.
//!
//! On startup, `run` resumes the most recent battle on disk if it hadn't
//! ended yet (see [`find_resumable_battle`]) rather than always starting
//! fresh — see that function's docs for what does and doesn't survive a
//! restart.

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
use crate::win;

/// A player's ping counts as live only if it arrived within this long.
const PRESENCE_HORIZON: Duration = Duration::from_secs(45);
/// Clamp range for the match length chosen through the client's menu.
const MIN_BATTLE_DURATION_SECS: u64 = 60;
const MAX_BATTLE_DURATION_SECS: u64 = 6 * 60 * 60;
/// How often the ingest loop wakes up with no client message, purely to
/// advance `Retain`'s clock (an idle stream never self-expires stale pings).
const TICK: Duration = Duration::from_secs(5);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn battle_json_path(dir: &Path) -> std::path::PathBuf {
    dir.join("battle.json")
}

/// Persists just enough of `Battle` (status, timers, chosen park/length) to
/// resume after a restart — the roster/presence facts themselves are
/// already durable via fold's own stores under `dir`. Best-effort: nothing
/// reads this back except a fresh process's startup scan, so a failed
/// write here only costs a clean resume after the *next* restart, not the
/// live game.
fn save_battle(dir: &Path, battle: &Battle) {
    if let Ok(json) = serde_json::to_vec(battle) {
        let _ = std::fs::write(battle_json_path(dir), json);
    }
}

fn load_battle(dir: &Path) -> Option<Battle> {
    let bytes = std::fs::read(battle_json_path(dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Highest `battle-{n}` id already on disk under `data_dir` (0 if none).
/// Used both to find the most recent battle to resume and to make sure a
/// fresh battle always picks an unused id — so `run` never has cause to
/// delete an existing directory, unlike the id counter simply restarting
/// at 1 on every process start.
fn max_existing_battle_id(data_dir: &Path) -> u64 {
    std::fs::read_dir(data_dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry.file_name().to_str()?.strip_prefix("battle-")?.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// If the most recent battle on disk was interrupted before it ended,
/// returns its id and last-known state so `run` can reopen the same
/// roster/presence stores in place instead of starting a new battle over
/// them.
///
/// What survives a restart: roster membership and location pings (both are
/// durable fold state), and the battle's status/timers/chosen park/length
/// (from `battle.json`) — including a battle interrupted before anyone had
/// configured it (`park: None`), which simply resumes in phase 1. What
/// doesn't survive: the elimination debounce (`zero_since` in `run`)
/// always restarts cold — a team that was, say, 55 seconds into being
/// confirmed swept when the process restarted needs a fresh
/// `ELIMINATION_CONFIRM_MS` window after resuming. That only delays an
/// instant-win confirmation; it can't produce a wrong one.
fn find_resumable_battle(data_dir: &Path) -> Option<(u64, Battle)> {
    let id = max_existing_battle_id(data_dir);
    if id == 0 {
        return None;
    }
    let battle = load_battle(&data_dir.join(format!("battle-{id}")))?;
    (battle.status != BattleStatus::Ended).then_some((id, battle))
}

/// Owns the fold stores for the whole process lifetime: runs one battle to
/// completion, then immediately opens a fresh one so the server never needs
/// a restart between battles.
pub fn run(data_dir: &Path, rx: mpsc::Receiver<ClientMsg>, state_tx: watch::Sender<Scoreboard>) {
    // Dev/test override: forces every battle in this process to run this
    // long regardless of what any client's menu picks, since a real match
    // can run for hours — far too long to exercise by hand.
    let duration_override_ms = std::env::var("MMO_BATTLE_DURATION_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|secs| secs * 1000);

    let mut next_id = max_existing_battle_id(data_dir) + 1;
    let mut resume = find_resumable_battle(data_dir);

    loop {
        let (battle_dir, mut battle) = match resume.take() {
            Some((id, battle)) => {
                let park_desc = battle.park.as_ref().map(|p| p.name.as_str()).unwrap_or("(unconfigured)");
                println!("resuming battle {id} ({:?}) at {park_desc}", battle.status);
                (data_dir.join(format!("battle-{id}")), battle)
            }
            None => {
                let battle = Battle {
                    id: next_id,
                    park: None,
                    duration_ms: None,
                    status: BattleStatus::Pending,
                    started_at_ms: None,
                    ends_at_ms: None,
                    outcome: None,
                };
                let dir = data_dir.join(format!("battle-{next_id}"));
                println!("battle {} awaiting park + match length", battle.id);
                next_id += 1;
                (dir, battle)
            }
        };
        save_battle(&battle_dir, &battle);

        // Roster pipeline: KeyBy(team) + Aggregate(count) -> Table<Team, i64>.
        // Permanent membership fact, never wrapped in Retain. Doesn't need
        // a chosen park, so it's built before phase 1 even resolves one.
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

        // Reads roster directly rather than through a helper fn: its
        // pipeline type contains a closure and can't be named as a
        // function parameter type (see module docs).
        macro_rules! roster_count {
            ($team:expr) => {
                roster.rtx(|t| t.get(&$team)).unwrap_or(0)
            };
        }

        // Phase 1: awaiting configuration. The presence pipeline needs the
        // chosen park's bbox baked into its aggregate closure at
        // construction time, so it can't be built yet — this loop only
        // handles picking a park/length (and, harmlessly, early team
        // joins, since `battle.park.is_none()` already implies Pending).
        if battle.park.is_none() {
            macro_rules! config_snapshot {
                () => {{
                    let stats = |team: Team| TeamStats {
                        members: roster_count!(team).max(0) as u32,
                        pinging: 0,
                        in_bounds: 0,
                    };
                    Scoreboard {
                        battle: battle.clone(),
                        court_square: stats(Team::CourtSquare),
                        church_ave: stats(Team::ChurchAve),
                    }
                }};
            }

            let _ = state_tx.send(config_snapshot!());

            loop {
                match rx.recv_timeout(TICK) {
                    Ok(ClientMsg::ConfigureBattle { park_id, duration_secs }) => {
                        if let Some(park) = parks::find_by_id(&park_id) {
                            let duration_ms = duration_override_ms.unwrap_or_else(|| {
                                duration_secs
                                    .clamp(MIN_BATTLE_DURATION_SECS, MAX_BATTLE_DURATION_SECS)
                                    * 1000
                            });
                            println!("battle {} set to {} ({duration_ms}ms)", battle.id, park.name);
                            battle.park = Some(park);
                            battle.duration_ms = Some(duration_ms);
                        }
                    }
                    Ok(ClientMsg::Join { player, team }) => {
                        roster.wtx(|tx| {
                            tx.upsert(&player, &RosterEntry { team, joined_at_ms: now_ms() })
                        });
                    }
                    Ok(ClientMsg::Ping { .. } | ClientMsg::StartBattle) => {} // no park chosen yet
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }

                save_battle(&battle_dir, &battle);
                let _ = state_tx.send(config_snapshot!());

                if battle.park.is_some() {
                    break;
                }
            }
        }

        let bbox = battle.park.as_ref().expect("configured in phase 1").bbox;
        let duration_ms = battle.duration_ms.expect("configured in phase 1");

        // Presence pipeline: Retain(liveness) -> KeyBy(team) + Aggregate(in/out
        // counts) -> Table<Team, PresenceCounts>. `bbox` is captured by the
        // step closure at construction time, since it differs per battle.
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
        // continuously; cleared the moment it's no longer swept. Always
        // starts empty, including on resume — see find_resumable_battle's
        // doc comment for why that's fine.
        let mut zero_since: HashMap<Team, u64> = HashMap::new();

        macro_rules! presence_counts {
            ($team:expr) => {{
                let pc: PresenceCounts = presence.rtx(|t| t.get(&$team)).unwrap_or_default();
                pc
            }};
        }
        macro_rules! team_snapshot {
            ($team:expr) => {{
                let pc = presence_counts!($team);
                win::TeamSnapshot { members: roster_count!($team), pinging: pc.pinging, in_bounds: pc.in_bounds }
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
                Ok(ClientMsg::ConfigureBattle { .. }) => {} // locked once a park is chosen
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
                            battle.ends_at_ms = Some(start + duration_ms);
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // no-op write: advances Retain's clock, expiring stale pings
                    presence.wtx(|_| {});
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }

            // win-condition evaluation: see `crate::win` for the decision
            // logic itself (pure, unit-tested there) — this just gathers
            // the current counts and applies whichever outcome it returns.
            if battle.status == BattleStatus::Active {
                let now = now_ms();
                let cs_snap = team_snapshot!(Team::CourtSquare);
                let ca_snap = team_snapshot!(Team::ChurchAve);

                let outcome = win::check_elimination(
                    now,
                    &mut zero_since,
                    [(Team::CourtSquare, cs_snap), (Team::ChurchAve, ca_snap)],
                )
                .or_else(|| {
                    let ends_at = battle.ends_at_ms?;
                    (now >= ends_at).then(|| win::check_timeout(cs_snap, ca_snap))
                });

                if let Some(outcome) = outcome {
                    battle.status = BattleStatus::Ended;
                    battle.outcome = Some(outcome);
                }
            }

            save_battle(&battle_dir, &battle);
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
