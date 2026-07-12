//! Pure win-condition decision logic.
//!
//! Deliberately decoupled from fold — `battle.rs` reads counts out of the
//! roster/presence streams (whose pipeline types can't be named, see that
//! module's docs) and hands plain [`TeamSnapshot`]s in here; nothing below
//! touches a `Stream`, a store, or any I/O, so it's unit-testable on its
//! own without a live pipeline.

use std::collections::HashMap;

use crate::domain::{BattleOutcome, Team};

/// A team's rolled-up counts at one evaluation instant: `members` is the
/// roster size (permanent for the battle), `pinging`/`in_bounds` are the
/// live presence counts (bounded by `Retain`'s liveness horizon).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TeamSnapshot {
    pub members: i64,
    pub pinging: i64,
    pub in_bounds: i64,
}

impl TeamSnapshot {
    /// Fully removed from the battleground: confirmed live signal
    /// (`pinging > 0`), all of it outside the box. Total silence
    /// (`pinging == 0`) is deliberately excluded — it also produces
    /// `in_bounds == 0`, but a team that's gone quiet (dead phone, closed
    /// tab, shared dead zone) hasn't necessarily been pushed out of the
    /// area, so it can't itself trigger an instant win. A silent team can
    /// still lose, just via the timeout path, where its stale presence
    /// decays toward 0% as `Retain` ages its last ping out.
    fn swept(&self) -> bool {
        self.members > 0 && self.pinging > 0 && self.in_bounds == 0
    }

    /// Percentage of the roster currently confirmed inside the box.
    fn pct(&self) -> f64 {
        if self.members > 0 {
            self.in_bounds as f64 / self.members as f64
        } else {
            0.0
        }
    }
}

/// A team must read as continuously swept for this long before the instant
/// elimination win is confirmed — smooths over one flaky/late GPS fix.
pub const ELIMINATION_CONFIRM_MS: u64 = 60_000;

/// Checks the instant-elimination condition for both teams at once.
///
/// `zero_since` tracks, per team, the instant since which that team has
/// read as continuously swept; cleared the moment a team is no longer
/// swept. Both teams are evaluated before any decision is made, so a team
/// that becomes confirmed-swept in the exact same tick as its opponent is
/// handled explicitly (declared a tie — neither team controls the
/// battleground) rather than resolving arbitrarily by whichever team
/// happens to be checked first.
pub fn check_elimination(
    now: u64,
    zero_since: &mut HashMap<Team, u64>,
    snapshots: [(Team, TeamSnapshot); 2],
) -> Option<BattleOutcome> {
    let mut confirmed = Vec::with_capacity(2);
    for (team, snap) in snapshots {
        if snap.swept() {
            let since = *zero_since.entry(team).or_insert(now);
            if now - since >= ELIMINATION_CONFIRM_MS {
                confirmed.push(team);
            }
        } else {
            zero_since.remove(&team);
        }
    }

    match confirmed.as_slice() {
        [] => None,
        [team] => Some(BattleOutcome::Elimination { winner: team.opponent() }),
        // both teams confirmed fully swept in the same tick: neither
        // controls the battleground, so it's an explicit tie rather than
        // a pick that would otherwise depend on iteration order
        _ => Some(BattleOutcome::Tie),
    }
}

/// Decides the 3-hour timeout outcome: higher in-bounds percentage of
/// roster wins; a percentage tie breaks on higher absolute in-bounds
/// headcount; still tied after that is a draw.
pub fn check_timeout(court_square: TeamSnapshot, church_ave: TeamSnapshot) -> BattleOutcome {
    let (cs_pct, ca_pct) = (court_square.pct(), church_ave.pct());
    if cs_pct > ca_pct {
        BattleOutcome::Timeout { winner: Team::CourtSquare }
    } else if ca_pct > cs_pct {
        BattleOutcome::Timeout { winner: Team::ChurchAve }
    } else if court_square.in_bounds > church_ave.in_bounds {
        BattleOutcome::Timeout { winner: Team::CourtSquare }
    } else if church_ave.in_bounds > court_square.in_bounds {
        BattleOutcome::Timeout { winner: Team::ChurchAve }
    } else {
        BattleOutcome::Tie
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(members: i64, pinging: i64, in_bounds: i64) -> TeamSnapshot {
        TeamSnapshot { members, pinging, in_bounds }
    }

    #[test]
    fn not_confirmed_before_the_debounce_window_elapses() {
        let mut zero_since = HashMap::new();
        let snapshots = [
            (Team::CourtSquare, snap(1, 1, 1)),
            (Team::ChurchAve, snap(1, 1, 0)), // swept, but only just started
        ];
        assert_eq!(check_elimination(1_000, &mut zero_since, snapshots), None);
        assert_eq!(zero_since.get(&Team::ChurchAve), Some(&1_000));
    }

    #[test]
    fn confirms_a_single_team_elimination_after_the_window() {
        let mut zero_since = HashMap::from([(Team::ChurchAve, 0)]);
        let snapshots = [
            (Team::CourtSquare, snap(1, 1, 1)),
            (Team::ChurchAve, snap(1, 1, 0)),
        ];
        let outcome = check_elimination(ELIMINATION_CONFIRM_MS, &mut zero_since, snapshots);
        assert_eq!(outcome, Some(BattleOutcome::Elimination { winner: Team::CourtSquare }));
    }

    #[test]
    fn total_silence_never_counts_as_swept() {
        let mut zero_since = HashMap::new();
        let snapshots = [
            (Team::CourtSquare, snap(1, 1, 1)),
            (Team::ChurchAve, snap(1, 0, 0)), // silent, not swept
        ];
        let outcome = check_elimination(ELIMINATION_CONFIRM_MS * 10, &mut zero_since, snapshots);
        assert_eq!(outcome, None);
        assert!(!zero_since.contains_key(&Team::ChurchAve));
    }

    #[test]
    fn simultaneous_elimination_is_an_explicit_tie_not_iteration_order() {
        let mut zero_since = HashMap::from([(Team::CourtSquare, 0), (Team::ChurchAve, 0)]);
        let snapshots = [
            (Team::CourtSquare, snap(1, 1, 0)), // both fully swept at once
            (Team::ChurchAve, snap(1, 1, 0)),
        ];
        let outcome = check_elimination(ELIMINATION_CONFIRM_MS, &mut zero_since, snapshots);
        assert_eq!(outcome, Some(BattleOutcome::Tie));
    }

    #[test]
    fn becoming_unswept_clears_the_debounce_timer() {
        let mut zero_since = HashMap::from([(Team::ChurchAve, 0)]);
        let snapshots = [
            (Team::CourtSquare, snap(1, 1, 1)),
            (Team::ChurchAve, snap(1, 1, 1)), // back inside before the window elapsed
        ];
        check_elimination(ELIMINATION_CONFIRM_MS, &mut zero_since, snapshots);
        assert!(!zero_since.contains_key(&Team::ChurchAve));
    }

    #[test]
    fn timeout_picks_the_higher_percentage() {
        let outcome = check_timeout(snap(2, 0, 2), snap(2, 0, 1)); // 100% vs 50%
        assert_eq!(outcome, BattleOutcome::Timeout { winner: Team::CourtSquare });
    }

    #[test]
    fn timeout_tiebreaks_equal_percentage_by_headcount() {
        let outcome = check_timeout(snap(2, 0, 2), snap(1, 0, 1)); // both 100%, cs has more bodies
        assert_eq!(outcome, BattleOutcome::Timeout { winner: Team::CourtSquare });
    }

    #[test]
    fn timeout_declares_a_tie_when_fully_equal() {
        let outcome = check_timeout(snap(2, 0, 1), snap(2, 0, 1));
        assert_eq!(outcome, BattleOutcome::Tie);
    }
}
