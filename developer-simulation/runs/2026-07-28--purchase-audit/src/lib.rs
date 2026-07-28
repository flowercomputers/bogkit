//! A deterministic comparison of a single-store audit transaction with a
//! PostgreSQL-plus-Fold split write.
//!
//! `BaselineModel` is deliberately small. It models transaction outcomes and
//! audit semantics, not PostgreSQL durability, locking, grants, or latency.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::Path;

use fold::pipeline::terminal;
use fold::stream::Stream;
use serde::{Deserialize, Serialize};

pub type RequestId = u64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RequestStatus {
    Draft,
    Approved,
    Rejected,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Action {
    Create,
    Approve,
    Reject,
    Cancel,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PurchaseRequest {
    pub id: RequestId,
    pub amount_cents: u64,
    pub status: RequestStatus,
    pub policy_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditEvent {
    pub sequence: u64,
    pub request_id: RequestId,
    pub action: Action,
    pub previous_status: Option<RequestStatus>,
    pub new_status: RequestStatus,
    pub amount_cents: u64,
    pub actor: String,
    pub policy_version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailPoint {
    Never,
    BeforeCommit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuditError {
    RequestAlreadyExists(RequestId),
    RequestNotFound(RequestId),
    InvalidTransition {
        request_id: RequestId,
        from: RequestStatus,
        action: Action,
    },
    InjectedFailure,
    AppRoleCannotModifyAudit,
}

impl Display for AuditError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequestAlreadyExists(id) => write!(f, "request {id} already exists"),
            Self::RequestNotFound(id) => write!(f, "request {id} does not exist"),
            Self::InvalidTransition {
                request_id,
                from,
                action,
            } => write!(
                f,
                "request {request_id} cannot apply {action:?} from {from:?}"
            ),
            Self::InjectedFailure => write!(f, "failure injected before commit"),
            Self::AppRoleCannotModifyAudit => {
                write!(f, "the ordinary application role cannot modify audit rows")
            }
        }
    }
}

impl std::error::Error for AuditError {}

/// Models current rows and append-only audit rows committed in one database
/// transaction. Changes are staged and become visible only at the commit
/// boundary.
#[derive(Default)]
pub struct BaselineModel {
    requests: BTreeMap<RequestId, PurchaseRequest>,
    audit_by_sequence: BTreeMap<u64, AuditEvent>,
    sequences_by_request: BTreeMap<RequestId, Vec<u64>>,
    next_sequence: u64,
}

impl BaselineModel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &mut self,
        id: RequestId,
        amount_cents: u64,
        actor: &str,
        policy_version: &str,
        fail: FailPoint,
    ) -> Result<(), AuditError> {
        if self.requests.contains_key(&id) {
            return Err(AuditError::RequestAlreadyExists(id));
        }

        let request = PurchaseRequest {
            id,
            amount_cents,
            status: RequestStatus::Draft,
            policy_version: policy_version.to_owned(),
        };
        let event = self.event_for(&request, Action::Create, None, actor, policy_version);

        if fail == FailPoint::BeforeCommit {
            return Err(AuditError::InjectedFailure);
        }

        self.commit(request, event);
        Ok(())
    }

    pub fn approve(
        &mut self,
        id: RequestId,
        actor: &str,
        policy_version: &str,
        fail: FailPoint,
    ) -> Result<(), AuditError> {
        self.transition(
            id,
            Action::Approve,
            RequestStatus::Approved,
            actor,
            policy_version,
            fail,
        )
    }

    pub fn reject(
        &mut self,
        id: RequestId,
        actor: &str,
        policy_version: &str,
        fail: FailPoint,
    ) -> Result<(), AuditError> {
        self.transition(
            id,
            Action::Reject,
            RequestStatus::Rejected,
            actor,
            policy_version,
            fail,
        )
    }

    pub fn cancel(
        &mut self,
        id: RequestId,
        actor: &str,
        policy_version: &str,
        fail: FailPoint,
    ) -> Result<(), AuditError> {
        self.transition(
            id,
            Action::Cancel,
            RequestStatus::Cancelled,
            actor,
            policy_version,
            fail,
        )
    }

    fn transition(
        &mut self,
        id: RequestId,
        action: Action,
        new_status: RequestStatus,
        actor: &str,
        policy_version: &str,
        fail: FailPoint,
    ) -> Result<(), AuditError> {
        let current = self
            .requests
            .get(&id)
            .ok_or(AuditError::RequestNotFound(id))?;

        if current.status != RequestStatus::Draft {
            return Err(AuditError::InvalidTransition {
                request_id: id,
                from: current.status,
                action,
            });
        }

        let previous_status = current.status;
        let mut changed = current.clone();
        changed.status = new_status;
        changed.policy_version = policy_version.to_owned();
        let event = self.event_for(
            &changed,
            action,
            Some(previous_status),
            actor,
            policy_version,
        );

        if fail == FailPoint::BeforeCommit {
            return Err(AuditError::InjectedFailure);
        }

        self.commit(changed, event);
        Ok(())
    }

    fn event_for(
        &self,
        request: &PurchaseRequest,
        action: Action,
        previous_status: Option<RequestStatus>,
        actor: &str,
        policy_version: &str,
    ) -> AuditEvent {
        AuditEvent {
            sequence: self.next_sequence + 1,
            request_id: request.id,
            action,
            previous_status,
            new_status: request.status,
            amount_cents: request.amount_cents,
            actor: actor.to_owned(),
            policy_version: policy_version.to_owned(),
        }
    }

    fn commit(&mut self, request: PurchaseRequest, event: AuditEvent) {
        self.next_sequence = event.sequence;
        self.sequences_by_request
            .entry(request.id)
            .or_default()
            .push(event.sequence);
        self.audit_by_sequence.insert(event.sequence, event);
        self.requests.insert(request.id, request);
    }

    #[must_use]
    pub fn request(&self, id: RequestId) -> Option<&PurchaseRequest> {
        self.requests.get(&id)
    }

    #[must_use]
    pub fn audit_len(&self) -> usize {
        self.audit_by_sequence.len()
    }

    #[must_use]
    pub fn timeline(&self, id: RequestId) -> Vec<&AuditEvent> {
        self.sequences_by_request
            .get(&id)
            .into_iter()
            .flatten()
            .filter_map(|sequence| self.audit_by_sequence.get(sequence))
            .collect()
    }

    #[must_use]
    pub fn all_timeline(&self, limit: usize) -> Vec<&AuditEvent> {
        self.audit_by_sequence.values().take(limit).collect()
    }

    pub fn app_role_update_audit(&mut self, _sequence: u64) -> Result<(), AuditError> {
        Err(AuditError::AppRoleCannotModifyAudit)
    }

    pub fn app_role_delete_audit(&mut self, _sequence: u64) -> Result<(), AuditError> {
        Err(AuditError::AppRoleCannotModifyAudit)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitBoundaryResult {
    pub current_state: RequestStatus,
    pub fold_events: Vec<AuditEvent>,
}

/// Uses the real Fold `Bag` as a durable sidecar and a deterministic stand-in
/// for a PostgreSQL current-state row. The second state mutation commits, then
/// the injected integration failure prevents the corresponding Fold write.
///
/// This proves only the transaction-boundary mismatch. It does not model a
/// PostgreSQL driver or a process crash.
#[must_use]
pub fn reproduce_split_commit(path: &Path) -> SplitBoundaryResult {
    let created = AuditEvent {
        sequence: 1,
        request_id: 7,
        action: Action::Create,
        previous_status: None,
        new_status: RequestStatus::Draft,
        amount_cents: 125_000,
        actor: "priya".to_owned(),
        policy_version: "policy-2026.1".to_owned(),
    };

    let mut fold_audit = Stream::new(path, terminal::Bag::<AuditEvent>::new("audit_events"));
    fold_audit.wtx(|tx| tx.insert(&created));

    // The PostgreSQL-like state commits first.
    let current_state = RequestStatus::Approved;
    // Injected failure: the matching Fold write is never called.

    let mut fold_events = fold_audit.rtx(|events| {
        events
            .iter()
            .flat_map(|(event, multiplicity)| {
                std::iter::repeat_n(event, usize::try_from(multiplicity).unwrap_or_default())
            })
            .collect::<Vec<_>>()
    });
    fold_events.sort_by_key(|event| event.sequence);

    SplitBoundaryResult {
        current_state,
        fold_events,
    }
}

/// Shows that a caller with a Fold write handle can retract an event. Fold
/// does not expose PostgreSQL-style table privileges or application roles.
#[must_use]
pub fn fold_retraction_count(path: &Path) -> usize {
    let event = AuditEvent {
        sequence: 1,
        request_id: 7,
        action: Action::Create,
        previous_status: None,
        new_status: RequestStatus::Draft,
        amount_cents: 125_000,
        actor: "priya".to_owned(),
        policy_version: "policy-2026.1".to_owned(),
    };

    let mut fold_audit = Stream::new(path, terminal::Bag::<AuditEvent>::new("audit_events"));
    fold_audit.wtx(|tx| tx.insert(&event));
    fold_audit.wtx(|tx| tx.remove(&event));
    fold_audit.rtx(|events| events.iter().count())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use super::*;

    fn clean_test_path(name: &str) -> std::path::PathBuf {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("test-data")
            .join(name);
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        path
    }

    #[test]
    fn every_successful_mutation_has_one_ordered_accurate_event() {
        let mut db = BaselineModel::new();
        db.create(10, 50_000, "priya", "policy-2026.1", FailPoint::Never)
            .unwrap();
        db.approve(10, "sam", "policy-2026.2", FailPoint::Never)
            .unwrap();

        let timeline = db.timeline(10);
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].sequence, 1);
        assert_eq!(timeline[0].action, Action::Create);
        assert_eq!(timeline[0].previous_status, None);
        assert_eq!(timeline[0].new_status, RequestStatus::Draft);
        assert_eq!(timeline[0].policy_version, "policy-2026.1");
        assert_eq!(timeline[1].sequence, 2);
        assert_eq!(timeline[1].action, Action::Approve);
        assert_eq!(timeline[1].previous_status, Some(RequestStatus::Draft));
        assert_eq!(timeline[1].new_status, RequestStatus::Approved);
        assert_eq!(timeline[1].policy_version, "policy-2026.2");
    }

    #[test]
    fn forced_failure_commits_neither_state_nor_event() {
        let mut db = BaselineModel::new();
        let result = db.create(
            11,
            75_000,
            "priya",
            "policy-2026.1",
            FailPoint::BeforeCommit,
        );

        assert_eq!(result, Err(AuditError::InjectedFailure));
        assert!(db.request(11).is_none());
        assert_eq!(db.audit_len(), 0);
    }

    #[test]
    fn failed_transition_preserves_prior_state_and_timeline() {
        let mut db = BaselineModel::new();
        db.create(12, 99_000, "priya", "policy-2026.1", FailPoint::Never)
            .unwrap();
        let result = db.reject(12, "sam", "policy-2026.2", FailPoint::BeforeCommit);

        assert_eq!(result, Err(AuditError::InjectedFailure));
        assert_eq!(
            db.request(12).map(|request| request.status),
            Some(RequestStatus::Draft)
        );
        assert_eq!(db.timeline(12).len(), 1);
    }

    #[test]
    fn approve_reject_and_cancel_are_terminal_transitions() {
        let mut db = BaselineModel::new();
        for id in 20..=22 {
            db.create(id, 10_000, "priya", "p1", FailPoint::Never)
                .unwrap();
        }
        db.approve(20, "sam", "p2", FailPoint::Never).unwrap();
        db.reject(21, "sam", "p2", FailPoint::Never).unwrap();
        db.cancel(22, "priya", "p1", FailPoint::Never).unwrap();

        assert_eq!(
            db.request(20).map(|request| request.status),
            Some(RequestStatus::Approved)
        );
        assert_eq!(
            db.request(21).map(|request| request.status),
            Some(RequestStatus::Rejected)
        );
        assert_eq!(
            db.request(22).map(|request| request.status),
            Some(RequestStatus::Cancelled)
        );
        assert!(matches!(
            db.cancel(20, "priya", "p2", FailPoint::Never),
            Err(AuditError::InvalidTransition { .. })
        ));
        assert_eq!(db.timeline(20).len(), 2);
    }

    #[test]
    fn app_role_cannot_update_or_delete_audit_events() {
        let mut db = BaselineModel::new();
        db.create(30, 15_000, "priya", "p1", FailPoint::Never)
            .unwrap();

        assert_eq!(
            db.app_role_update_audit(1),
            Err(AuditError::AppRoleCannotModifyAudit)
        );
        assert_eq!(
            db.app_role_delete_audit(1),
            Err(AuditError::AppRoleCannotModifyAudit)
        );
        assert_eq!(db.audit_len(), 1);
    }

    #[test]
    fn ten_thousand_event_query_is_under_250ms_in_the_local_model() {
        let mut db = BaselineModel::new();
        for id in 1..=5_000 {
            db.create(id, id * 100, "seed", "p1", FailPoint::Never)
                .unwrap();
            match id % 3 {
                0 => db.approve(id, "seed", "p2", FailPoint::Never).unwrap(),
                1 => db.reject(id, "seed", "p2", FailPoint::Never).unwrap(),
                _ => db.cancel(id, "seed", "p2", FailPoint::Never).unwrap(),
            }
        }

        let started = Instant::now();
        let timeline = db.all_timeline(10_000);
        let elapsed = started.elapsed();

        assert_eq!(timeline.len(), 10_000);
        assert!(
            timeline
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert!(
            elapsed < Duration::from_millis(250),
            "local model query took {elapsed:?}"
        );
    }

    #[test]
    fn fold_sidecar_cannot_share_the_state_commit() {
        let path = clean_test_path("split-commit");
        let result = reproduce_split_commit(&path);

        assert_eq!(result.current_state, RequestStatus::Approved);
        assert_eq!(result.fold_events.len(), 1);
        assert_eq!(result.fold_events[0].action, Action::Create);
        assert_ne!(
            result.fold_events.last().map(|event| event.new_status),
            Some(result.current_state)
        );
    }

    #[test]
    fn fold_sidecar_allows_retraction_by_its_writer() {
        let path = clean_test_path("retraction");
        assert_eq!(fold_retraction_count(&path), 0);
    }
}
