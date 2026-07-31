use std::collections::BTreeMap;

use crate::model::{
    CERTIFICATIONS, Caregiver, CaregiverId, Outcome, REGIONS, State, UnfilledReason, Visit,
    VisitId, travel_minutes,
};

pub type PublishedAssignments = BTreeMap<VisitId, CaregiverId>;

pub fn validate(
    state: &State,
    outcomes: &BTreeMap<VisitId, Outcome>,
    protected: Option<(&PublishedAssignments, VisitId)>,
) -> Result<(), String> {
    let active = state
        .visits
        .values()
        .filter(|visit| !visit.canceled)
        .count();
    if outcomes.len() != active {
        return Err(format!(
            "expected one outcome per active visit: {active} active, {} outcomes",
            outcomes.len()
        ));
    }

    let mut assigned: BTreeMap<CaregiverId, Vec<&Visit>> = BTreeMap::new();
    for (visit_id, outcome) in outcomes {
        let visit = state
            .visits
            .get(visit_id)
            .ok_or_else(|| format!("outcome references missing visit {visit_id}"))?;
        if visit.canceled {
            return Err(format!("canceled visit {visit_id} still has an outcome"));
        }
        if let Outcome::Assigned(caregiver_id) = outcome {
            assigned.entry(*caregiver_id).or_default().push(visit);
        }
    }

    for (caregiver_id, visits) in &mut assigned {
        let caregiver = state
            .caregivers
            .get(caregiver_id)
            .ok_or_else(|| format!("assignment references missing caregiver {caregiver_id}"))?;
        visits.sort_by_key(|visit| (visit.start, visit.id));
        let minutes: i64 = visits.iter().map(|visit| visit.duration()).sum();
        if minutes > caregiver.max_minutes {
            return Err(format!("caregiver {caregiver_id} exceeds hour limit"));
        }
        for visit in visits.iter() {
            validate_static(caregiver, visit)?;
        }
        for pair in visits.windows(2) {
            let first = pair[0];
            let second = pair[1];
            let earliest = first.end + travel_minutes(first.region, second.region);
            if earliest > second.start {
                return Err(format!(
                    "caregiver {caregiver_id} lacks travel time between {} and {}",
                    first.id, second.id
                ));
            }
        }
    }

    for (visit_id, outcome) in outcomes {
        if let Outcome::Unfilled(reported) = outcome {
            let visit = &state.visits[visit_id];
            let expected = independent_reason(state, &assigned, visit)?;
            if *reported != expected {
                return Err(format!(
                    "visit {visit_id}: reason {} did not match validator reason {}",
                    reported.code(),
                    expected.code()
                ));
            }
        }
    }

    if let Some((before, changed_visit)) = protected {
        for (visit_id, caregiver_id) in before {
            if *visit_id == changed_visit || state.visits[visit_id].canceled {
                continue;
            }
            if outcomes.get(visit_id) != Some(&Outcome::Assigned(*caregiver_id)) {
                return Err(format!(
                    "unaffected published assignment for visit {visit_id} changed"
                ));
            }
        }
    }
    Ok(())
}

fn validate_static(caregiver: &Caregiver, visit: &Visit) -> Result<(), String> {
    if visit.required_certification >= CERTIFICATIONS
        || caregiver.certification_mask & (1 << visit.required_certification) == 0
    {
        return Err(format!("visit {} lacks certification", visit.id));
    }
    if visit.region >= REGIONS || caregiver.region_mask & (1 << visit.region) == 0 {
        return Err(format!("visit {} lacks region coverage", visit.id));
    }
    if !caregiver
        .availability
        .iter()
        .any(|window| window.start <= visit.start && visit.end <= window.end)
    {
        return Err(format!("visit {} is outside availability", visit.id));
    }
    if caregiver
        .required_rest
        .iter()
        .any(|rest| rest.start < visit.end && visit.start < rest.end)
    {
        return Err(format!("visit {} overlaps required rest", visit.id));
    }
    Ok(())
}

fn independent_reason(
    state: &State,
    assigned: &BTreeMap<CaregiverId, Vec<&Visit>>,
    visit: &Visit,
) -> Result<UnfilledReason, String> {
    let mut candidates: Vec<_> = state.caregivers.values().collect();
    candidates.retain(|caregiver| {
        visit.required_certification < CERTIFICATIONS
            && caregiver.certification_mask & (1 << visit.required_certification) != 0
    });
    if candidates.is_empty() {
        return Ok(UnfilledReason::NoCertification);
    }
    candidates.retain(|caregiver| {
        visit.region < REGIONS && caregiver.region_mask & (1 << visit.region) != 0
    });
    if candidates.is_empty() {
        return Ok(UnfilledReason::NoRegionCoverage);
    }
    candidates.retain(|caregiver| {
        caregiver
            .availability
            .iter()
            .any(|window| window.start <= visit.start && visit.end <= window.end)
    });
    if candidates.is_empty() {
        return Ok(UnfilledReason::OutsideAvailability);
    }
    candidates.retain(|caregiver| {
        !caregiver
            .required_rest
            .iter()
            .any(|rest| rest.start < visit.end && visit.start < rest.end)
    });
    if candidates.is_empty() {
        return Ok(UnfilledReason::RequiredRest);
    }
    candidates.retain(|caregiver| {
        let minutes: i64 = assigned
            .get(&caregiver.id)
            .into_iter()
            .flatten()
            .map(|assigned_visit| assigned_visit.duration())
            .sum();
        minutes + visit.duration() <= caregiver.max_minutes
    });
    if candidates.is_empty() {
        return Ok(UnfilledReason::HourLimit);
    }
    for caregiver in candidates {
        let travel_feasible = assigned
            .get(&caregiver.id)
            .into_iter()
            .flatten()
            .all(|existing| {
                if existing.end <= visit.start {
                    existing.end + travel_minutes(existing.region, visit.region) <= visit.start
                } else if visit.end <= existing.start {
                    visit.end + travel_minutes(visit.region, existing.region) <= existing.start
                } else {
                    false
                }
            });
        if travel_feasible {
            return Err(format!(
                "visit {} is unfilled but caregiver {} is independently eligible",
                visit.id, caregiver.id
            ));
        }
    }
    Ok(UnfilledReason::TravelConflict)
}
