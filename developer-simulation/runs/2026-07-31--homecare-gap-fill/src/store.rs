use std::collections::BTreeMap;
use std::path::Path;

use fold::pipeline::{Aggregate, FilterMap, KeyBy, Keyed, terminal};
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize};

use crate::model::{EntityKey, Outcome, Record, State, VisitId};
use crate::scheduler::CancellationDelta;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Metric {
    Caregivers,
    ActiveVisits,
    CanceledVisits,
    Assignments,
    Unfilled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metrics {
    pub caregivers: i64,
    pub active_visits: i64,
    pub canceled_visits: i64,
    pub assignments: i64,
    pub unfilled: i64,
}

type MetricAggregate =
    Aggregate<Metric, Metric, i64, fn(&mut i64, &Metric, isize), terminal::Table<Metric, i64>>;
type MetricKeyBy = KeyBy<fn(&Metric) -> Metric, MetricAggregate, Metric, Metric>;
type MetricPipeline = FilterMap<
    fn(&Keyed<EntityKey, Record>) -> Option<Metric>,
    MetricKeyBy,
    Keyed<EntityKey, Record>,
    Metric,
>;
type Pipeline = (terminal::Table<EntityKey, Record>, MetricPipeline);
pub type Store = KeyedStream<EntityKey, Record, Pipeline>;

fn record_metric(row: &Keyed<EntityKey, Record>) -> Option<Metric> {
    match &row.val {
        Record::Caregiver(_) => Some(Metric::Caregivers),
        Record::Visit(visit) if visit.canceled => Some(Metric::CanceledVisits),
        Record::Visit(_) => Some(Metric::ActiveVisits),
        Record::Outcome {
            outcome: Outcome::Assigned(_),
            ..
        } => Some(Metric::Assignments),
        Record::Outcome {
            outcome: Outcome::Unfilled(_),
            ..
        } => Some(Metric::Unfilled),
    }
}

fn same_metric(metric: &Metric) -> Metric {
    *metric
}

fn count_metric(count: &mut i64, _metric: &Metric, delta: isize) {
    *count += delta as i64;
}

pub fn open_store(path: impl AsRef<Path>) -> Store {
    let records = terminal::Table::<EntityKey, Record>::new("records");
    let metrics = FilterMap::new(
        record_metric as fn(&Keyed<EntityKey, Record>) -> Option<Metric>,
        KeyBy::new(
            same_metric as fn(&Metric) -> Metric,
            Aggregate::new(
                "metric_counts",
                count_metric as fn(&mut i64, &Metric, isize),
                terminal::Table::<Metric, i64>::new("metrics"),
            ),
        ),
    );
    KeyedStream::new(path, (records, metrics))
}

pub fn persist_initial(store: &mut Store, state: &State, outcomes: &BTreeMap<VisitId, Outcome>) {
    store.wtx(|tx| {
        for caregiver in state.caregivers.values() {
            tx.upsert(
                &EntityKey::Caregiver(caregiver.id),
                &Record::Caregiver(caregiver.clone()),
            );
        }
        for visit in state.visits.values() {
            tx.upsert(&EntityKey::Visit(visit.id), &Record::Visit(visit.clone()));
        }
        for (visit_id, outcome) in outcomes {
            tx.upsert(
                &EntityKey::Outcome(*visit_id),
                &Record::Outcome {
                    visit_id: *visit_id,
                    outcome: *outcome,
                },
            );
        }
    });
}

pub fn persist_cancellation(
    store: &mut Store,
    state: &State,
    outcomes: &BTreeMap<VisitId, Outcome>,
    delta: CancellationDelta,
) {
    let canceled = state.visits[&delta.canceled_visit].clone();
    store.wtx(|tx| {
        tx.upsert(
            &EntityKey::Visit(delta.canceled_visit),
            &Record::Visit(canceled),
        );
        tx.remove(&EntityKey::Outcome(delta.canceled_visit));
        if let Some(visit_id) = delta.replacement_visit {
            tx.upsert(
                &EntityKey::Outcome(visit_id),
                &Record::Outcome {
                    visit_id,
                    outcome: outcomes[&visit_id],
                },
            );
        }
    });
}

pub fn load_state(store: &Store) -> (State, BTreeMap<VisitId, Outcome>, Metrics) {
    store.rtx(|(records, metrics)| {
        let mut state = State::default();
        let mut outcomes = BTreeMap::new();
        for (_, record) in records.iter() {
            match record {
                Record::Caregiver(caregiver) => {
                    state.caregivers.insert(caregiver.id, caregiver);
                }
                Record::Visit(visit) => {
                    state.visits.insert(visit.id, visit);
                }
                Record::Outcome { visit_id, outcome } => {
                    outcomes.insert(visit_id, outcome);
                }
            }
        }
        let metrics = Metrics {
            caregivers: metrics.get(&Metric::Caregivers).unwrap_or(0),
            active_visits: metrics.get(&Metric::ActiveVisits).unwrap_or(0),
            canceled_visits: metrics.get(&Metric::CanceledVisits).unwrap_or(0),
            assignments: metrics.get(&Metric::Assignments).unwrap_or(0),
            unfilled: metrics.get(&Metric::Unfilled).unwrap_or(0),
        };
        (state, outcomes, metrics)
    })
}
