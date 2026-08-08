pub mod model;
pub mod scheduler;
pub mod store;
pub mod validator;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::model::{
        Caregiver, EntityKey, Interval, Outcome, Record, Scale, State, UnfilledReason, Visit,
        generate, parse_import_minute,
    };
    use crate::scheduler::{CandidateIndex, IncrementalScheduler, Mode, build_schedule};
    use crate::store::{load_state, open_store, persist_initial};
    use crate::validator::validate;

    #[test]
    fn explicit_offsets_handle_dst_gap_and_fold() {
        let before_gap = parse_import_minute("2026-03-08T01:30:00-05:00").unwrap();
        let after_gap = parse_import_minute("2026-03-08T03:30:00-04:00").unwrap();
        assert_eq!(after_gap - before_gap, 60);

        let first_fold = parse_import_minute("2026-11-01T01:30:00-04:00").unwrap();
        let second_fold = parse_import_minute("2026-11-01T01:30:00-05:00").unwrap();
        assert_eq!(second_fold - first_fold, 60);
        assert!(parse_import_minute("2026-03-08T02:30:00").is_err());
    }

    #[test]
    fn scheduler_is_deterministic_and_validator_accepts_it() {
        let state = generate(7, Scale::Tiny);
        let index = CandidateIndex::new(&state);
        let first = build_schedule(&state, &index, Mode::ContinuityAware);
        let second = build_schedule(&state, &index, Mode::ContinuityAware);
        assert_eq!(first, second);
        validate(&state, &first, None).unwrap();
    }

    #[test]
    fn validator_rejects_spurious_travel_conflict() {
        let caregiver = Caregiver {
            id: 7,
            certification_mask: 1,
            region_mask: 1,
            availability: vec![Interval { start: 0, end: 600 }],
            required_rest: Vec::new(),
            max_minutes: 600,
        };
        let visit = Visit {
            id: 11,
            client_id: 1,
            start: 60,
            end: 120,
            region: 0,
            required_certification: 0,
            urgency: 5,
            preferred_caregiver: None,
            canceled: false,
        };
        let state = State {
            caregivers: [(caregiver.id, caregiver)].into(),
            visits: [(visit.id, visit)].into(),
        };
        let outcomes = [(11, Outcome::Unfilled(UnfilledReason::TravelConflict))].into();

        let error = validate(&state, &outcomes, None).unwrap_err();
        assert!(error.contains("caregiver 7 is independently eligible"));
    }

    #[test]
    fn cancellation_preserves_unaffected_assignments() {
        let mut state = generate(7, Scale::Tiny);
        let index = CandidateIndex::new(&state);
        let schedule = build_schedule(&state, &index, Mode::ContinuityAware);
        let before: BTreeMap<_, _> = schedule
            .iter()
            .filter_map(|(visit, outcome)| match outcome {
                Outcome::Assigned(caregiver) => Some((*visit, *caregiver)),
                Outcome::Unfilled(_) => None,
            })
            .collect();
        let cancel_id = *before.keys().next().unwrap();
        let mut incremental = IncrementalScheduler::new(&state, schedule);
        incremental.cancel(&mut state, cancel_id).unwrap();
        validate(&state, incremental.outcomes(), Some((&before, cancel_id))).unwrap();
    }

    #[test]
    fn fold_round_trip_recovers_records_and_metrics() {
        let state = generate(9, Scale::Tiny);
        let index = CandidateIndex::new(&state);
        let schedule = build_schedule(&state, &index, Mode::ContinuityAware);
        let path = std::env::current_dir()
            .unwrap()
            .join("target/fold-round-trip-test");
        let _ = std::fs::remove_dir_all(&path);
        {
            let mut store = open_store(&path);
            persist_initial(&mut store, &state, &schedule);
            store.checkpoint();
        }
        let store = open_store(&path);
        let (loaded, loaded_schedule, metrics) = load_state(&store);
        assert_eq!(state, loaded);
        assert_eq!(schedule, loaded_schedule);
        assert_eq!(metrics.caregivers, state.caregivers.len() as i64);
        assert_eq!(
            metrics.assignments,
            schedule
                .values()
                .filter(|o| matches!(o, Outcome::Assigned(_)))
                .count() as i64
        );
        drop(store);
        let _ = std::fs::remove_dir_all(path);

        let _type_check = (
            EntityKey::Visit(0),
            Record::Outcome {
                visit_id: 0,
                outcome: Outcome::Unfilled(crate::model::UnfilledReason::NoCertification),
            },
        );
    }
}
