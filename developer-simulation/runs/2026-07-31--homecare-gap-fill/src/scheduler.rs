use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::{
    CERTIFICATIONS, Caregiver, CaregiverId, Minute, Outcome, REGIONS, State, UnfilledReason, Visit,
    VisitId, travel_minutes,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Baseline,
    ContinuityAware,
}

#[derive(Debug)]
pub struct CandidateIndex {
    by_region_certification: Vec<Vec<Vec<CaregiverId>>>,
}

impl CandidateIndex {
    pub fn new(state: &State) -> Self {
        let mut by_region_certification =
            vec![vec![Vec::new(); usize::from(CERTIFICATIONS)]; usize::from(REGIONS)];
        for caregiver in state.caregivers.values() {
            for region in 0..REGIONS {
                for certification in 0..CERTIFICATIONS {
                    if caregiver.region_mask & (1 << region) != 0
                        && caregiver.certification_mask & (1 << certification) != 0
                    {
                        by_region_certification[usize::from(region)][usize::from(certification)]
                            .push(caregiver.id);
                    }
                }
            }
        }
        Self {
            by_region_certification,
        }
    }

    pub fn candidates(&self, visit: &Visit) -> &[CaregiverId] {
        if visit.region >= REGIONS || visit.required_certification >= CERTIFICATIONS {
            return &[];
        }
        &self.by_region_certification[usize::from(visit.region)]
            [usize::from(visit.required_certification)]
    }
}

type PriorityKey = (Reverse<u8>, Minute, VisitId);

fn priority(visit: &Visit) -> PriorityKey {
    (Reverse(visit.urgency), visit.start, visit.id)
}

#[derive(Default)]
struct WorkingSchedule {
    outcomes: BTreeMap<VisitId, Outcome>,
    by_caregiver: BTreeMap<CaregiverId, BTreeSet<VisitId>>,
    minutes: BTreeMap<CaregiverId, Minute>,
}

impl WorkingSchedule {
    fn assigned_visits<'a>(
        &'a self,
        state: &'a State,
        caregiver_id: CaregiverId,
    ) -> impl Iterator<Item = &'a Visit> {
        self.by_caregiver
            .get(&caregiver_id)
            .into_iter()
            .flatten()
            .filter_map(|id| state.visits.get(id))
    }

    fn assign(&mut self, visit: &Visit, caregiver_id: CaregiverId) {
        self.outcomes
            .insert(visit.id, Outcome::Assigned(caregiver_id));
        self.by_caregiver
            .entry(caregiver_id)
            .or_default()
            .insert(visit.id);
        *self.minutes.entry(caregiver_id).or_default() += visit.duration();
    }
}

pub fn build_schedule(
    state: &State,
    index: &CandidateIndex,
    mode: Mode,
) -> BTreeMap<VisitId, Outcome> {
    let mut work = WorkingSchedule::default();
    let mut order: Vec<_> = state
        .visits
        .values()
        .filter(|visit| !visit.canceled)
        .collect();
    order.sort_by_key(|visit| priority(visit));

    for visit in order {
        let mut eligible: Vec<_> = index
            .candidates(visit)
            .iter()
            .copied()
            .filter(|caregiver_id| can_assign(state, &work, *caregiver_id, visit))
            .collect();
        match mode {
            Mode::Baseline => eligible.sort_unstable(),
            Mode::ContinuityAware => eligible.sort_by_key(|caregiver_id| {
                (
                    visit.preferred_caregiver != Some(*caregiver_id),
                    work.minutes.get(caregiver_id).copied().unwrap_or(0),
                    *caregiver_id,
                )
            }),
        }
        if let Some(caregiver_id) = eligible.first().copied() {
            work.assign(visit, caregiver_id);
        } else {
            work.outcomes
                .insert(visit.id, Outcome::Unfilled(UnfilledReason::TravelConflict));
        }
    }

    let unfilled: Vec<_> = work
        .outcomes
        .iter()
        .filter_map(|(id, outcome)| matches!(outcome, Outcome::Unfilled(_)).then_some(*id))
        .collect();
    for id in unfilled {
        let visit = &state.visits[&id];
        let reason = explain_unfilled(state, &work, visit);
        work.outcomes.insert(id, Outcome::Unfilled(reason));
    }
    work.outcomes
}

fn covers(caregiver: &Caregiver, visit: &Visit) -> bool {
    caregiver
        .availability
        .iter()
        .any(|window| window.contains(visit.start, visit.end))
}

fn rests(caregiver: &Caregiver, visit: &Visit) -> bool {
    caregiver
        .required_rest
        .iter()
        .any(|rest| rest.overlaps(visit.start, visit.end))
}

fn has_travel_room(existing: &Visit, candidate: &Visit) -> bool {
    if existing.end <= candidate.start {
        existing.end + travel_minutes(existing.region, candidate.region) <= candidate.start
    } else if candidate.end <= existing.start {
        candidate.end + travel_minutes(candidate.region, existing.region) <= existing.start
    } else {
        false
    }
}

fn can_assign(
    state: &State,
    work: &WorkingSchedule,
    caregiver_id: CaregiverId,
    visit: &Visit,
) -> bool {
    let Some(caregiver) = state.caregivers.get(&caregiver_id) else {
        return false;
    };
    caregiver.certification_mask & (1 << visit.required_certification) != 0
        && caregiver.region_mask & (1 << visit.region) != 0
        && covers(caregiver, visit)
        && !rests(caregiver, visit)
        && work.minutes.get(&caregiver_id).copied().unwrap_or(0) + visit.duration()
            <= caregiver.max_minutes
        && work
            .assigned_visits(state, caregiver_id)
            .all(|existing| has_travel_room(existing, visit))
}

fn explain_unfilled(state: &State, work: &WorkingSchedule, visit: &Visit) -> UnfilledReason {
    let mut candidates: Vec<_> = state.caregivers.values().collect();
    candidates.retain(|caregiver| {
        caregiver.certification_mask & (1 << visit.required_certification) != 0
    });
    if candidates.is_empty() {
        return UnfilledReason::NoCertification;
    }
    candidates.retain(|caregiver| caregiver.region_mask & (1 << visit.region) != 0);
    if candidates.is_empty() {
        return UnfilledReason::NoRegionCoverage;
    }
    candidates.retain(|caregiver| covers(caregiver, visit));
    if candidates.is_empty() {
        return UnfilledReason::OutsideAvailability;
    }
    candidates.retain(|caregiver| !rests(caregiver, visit));
    if candidates.is_empty() {
        return UnfilledReason::RequiredRest;
    }
    candidates.retain(|caregiver| {
        work.minutes.get(&caregiver.id).copied().unwrap_or(0) + visit.duration()
            <= caregiver.max_minutes
    });
    if candidates.is_empty() {
        return UnfilledReason::HourLimit;
    }
    UnfilledReason::TravelConflict
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancellationDelta {
    pub canceled_visit: VisitId,
    pub replacement_visit: Option<VisitId>,
}

pub struct IncrementalScheduler {
    work: WorkingSchedule,
    open: BTreeSet<PriorityKey>,
}

impl IncrementalScheduler {
    pub fn new(state: &State, outcomes: BTreeMap<VisitId, Outcome>) -> Self {
        let mut work = WorkingSchedule {
            outcomes,
            ..WorkingSchedule::default()
        };
        let mut open = BTreeSet::new();
        for (id, outcome) in &work.outcomes {
            let visit = &state.visits[id];
            match outcome {
                Outcome::Assigned(caregiver_id) => {
                    work.by_caregiver
                        .entry(*caregiver_id)
                        .or_default()
                        .insert(*id);
                    *work.minutes.entry(*caregiver_id).or_default() += visit.duration();
                }
                Outcome::Unfilled(_) => {
                    open.insert(priority(visit));
                }
            }
        }
        Self { work, open }
    }

    pub fn outcomes(&self) -> &BTreeMap<VisitId, Outcome> {
        &self.work.outcomes
    }

    pub fn cancel(
        &mut self,
        state: &mut State,
        visit_id: VisitId,
    ) -> Result<CancellationDelta, String> {
        let visit = state
            .visits
            .get_mut(&visit_id)
            .ok_or_else(|| format!("unknown visit {visit_id}"))?;
        if visit.canceled {
            return Err(format!("visit {visit_id} was already canceled"));
        }
        visit.canceled = true;
        let canceled_visit = visit.clone();
        let old = self
            .work
            .outcomes
            .remove(&visit_id)
            .ok_or_else(|| format!("visit {visit_id} had no published outcome"))?;
        self.open.remove(&priority(&canceled_visit));

        let freed = match old {
            Outcome::Assigned(caregiver_id) => {
                self.work
                    .by_caregiver
                    .entry(caregiver_id)
                    .or_default()
                    .remove(&visit_id);
                *self.work.minutes.entry(caregiver_id).or_default() -= canceled_visit.duration();
                Some(caregiver_id)
            }
            Outcome::Unfilled(_) => None,
        };

        let mut replacement = None;
        if let Some(caregiver_id) = freed {
            for key in &self.open {
                let open_visit = &state.visits[&key.2];
                if can_assign(state, &self.work, caregiver_id, open_visit) {
                    replacement = Some(open_visit.id);
                    break;
                }
            }
            if let Some(replacement_id) = replacement {
                let open_visit = &state.visits[&replacement_id];
                self.open.remove(&priority(open_visit));
                self.work.assign(open_visit, caregiver_id);
            }
        }

        Ok(CancellationDelta {
            canceled_visit: visit_id,
            replacement_visit: replacement,
        })
    }
}

pub fn counts(outcomes: &BTreeMap<VisitId, Outcome>, state: &State) -> (usize, usize, usize) {
    let mut filled = 0;
    let mut urgent_total = 0;
    let mut urgent_filled = 0;
    for (id, outcome) in outcomes {
        let visit = &state.visits[id];
        if visit.urgency >= 4 {
            urgent_total += 1;
        }
        if matches!(outcome, Outcome::Assigned(_)) {
            filled += 1;
            if visit.urgency >= 4 {
                urgent_filled += 1;
            }
        }
    }
    (filled, urgent_filled, urgent_total)
}

pub fn digest(state: &State, outcomes: &BTreeMap<VisitId, Outcome>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for visit in state.visits.values() {
        hash ^= visit.id;
        hash = hash.wrapping_mul(0x100_0000_01b3);
        hash ^= u64::from(visit.canceled);
        hash = hash.wrapping_mul(0x100_0000_01b3);
        if let Some(outcome) = outcomes.get(&visit.id) {
            let word = match outcome {
                Outcome::Assigned(id) => u64::from(*id) << 1,
                Outcome::Unfilled(reason) => 1 | ((*reason as u64) << 8),
            };
            hash ^= word;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    hash
}
