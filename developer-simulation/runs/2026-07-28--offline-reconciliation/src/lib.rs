use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::fmt;
use std::path::{Path, PathBuf};

use fold::pipeline::terminal;
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize};

pub type OperationKey = (String, u64);

type OperationStore =
    KeyedStream<OperationKey, Operation, terminal::Table<OperationKey, Operation>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    pub device_id: String,
    pub sequence: u64,
    pub pallet_id: String,
    pub action: String,
    pub location: String,
    pub operator: String,
    pub device_timestamp_ms: i64,
}

impl Operation {
    #[must_use]
    pub fn key(&self) -> OperationKey {
        (self.device_id.clone(), self.sequence)
    }

    fn validate(&self) -> Result<(), IngestError> {
        for (field, value) in [
            ("device_id", &self.device_id),
            ("pallet_id", &self.pallet_id),
            ("action", &self.action),
            ("location", &self.location),
            ("operator", &self.operator),
        ] {
            if value.trim().is_empty() {
                return Err(IngestError::InvalidOperation {
                    key: self.key(),
                    reason: format!("{field} must not be empty"),
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestReport {
    pub received: usize,
    pub inserted: usize,
    pub duplicate_replays: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestError {
    InvalidOperation {
        key: OperationKey,
        reason: String,
    },
    DivergentDuplicate {
        key: OperationKey,
        stored: Box<Operation>,
        incoming: Box<Operation>,
    },
    SimulatedInterruption {
        after_inserts: usize,
    },
    NothingToInterrupt {
        requested_after: usize,
        new_operations: usize,
    },
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOperation { key, reason } => {
                write!(f, "invalid operation {}:{}: {reason}", key.0, key.1)
            }
            Self::DivergentDuplicate { key, .. } => write!(
                f,
                "operation identity {}:{} was reused with different content",
                key.0, key.1
            ),
            Self::SimulatedInterruption { after_inserts } => {
                write!(f, "simulated interruption after {after_inserts} inserts")
            }
            Self::NothingToInterrupt {
                requested_after,
                new_operations,
            } => write!(
                f,
                "cannot interrupt after {requested_after} inserts: only {new_operations} are new"
            ),
        }
    }
}

impl std::error::Error for IngestError {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Candidate {
    pub device_id: String,
    pub sequence: u64,
    pub action: String,
    pub location: String,
    pub operator: String,
    pub device_timestamp_ms: i64,
}

impl From<Operation> for Candidate {
    fn from(operation: Operation) -> Self {
        Self {
            device_id: operation.device_id,
            sequence: operation.sequence,
            action: operation.action,
            location: operation.location,
            operator: operation.operator,
            device_timestamp_ms: operation.device_timestamp_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PalletStatus {
    Settled {
        action: String,
        location: String,
        evidence: Vec<Candidate>,
    },
    Conflict {
        candidates: Vec<Candidate>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PalletView {
    pub pallet_id: String,
    #[serde(flatten)]
    pub status: PalletStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub operation_count: usize,
    pub pallets: Vec<PalletView>,
}

pub struct Model {
    path: PathBuf,
    store: Option<OperationStore>,
}

impl Model {
    #[must_use]
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref().to_path_buf();
        Self {
            store: Some(open_store(&path)),
            path,
        }
    }

    pub fn ingest_batch(&mut self, batch: &[Operation]) -> Result<IngestReport, IngestError> {
        let (normalized, mut duplicate_replays) = normalize_batch(batch)?;
        let mut new_operations = Vec::with_capacity(normalized.len());
        let mut conflict = None;

        self.store.as_mut().expect("model store is open").wtx(|tx| {
            for (key, incoming) in &normalized {
                match tx.get(key) {
                    Some(stored) if stored == *incoming => duplicate_replays += 1,
                    Some(stored) => {
                        conflict = Some(IngestError::DivergentDuplicate {
                            key: key.clone(),
                            stored: Box::new(stored),
                            incoming: Box::new(incoming.clone()),
                        });
                        return;
                    }
                    None => new_operations.push((key.clone(), incoming.clone())),
                }
            }

            for (key, operation) in &new_operations {
                let replaced = tx.upsert(key, operation);
                debug_assert!(replaced.is_none());
            }
        });

        if let Some(error) = conflict {
            return Err(error);
        }

        Ok(IngestReport {
            received: batch.len(),
            inserted: new_operations.len(),
            duplicate_replays,
        })
    }

    pub fn simulate_interrupted_batch(
        &mut self,
        batch: &[Operation],
        after_inserts: usize,
    ) -> Result<(), IngestError> {
        let (normalized, _) = normalize_batch(batch)?;
        let mut new_operations = Vec::with_capacity(normalized.len());
        let mut conflict = None;
        let mut nothing_to_interrupt = None;

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.store.as_mut().expect("model store is open").wtx(|tx| {
                for (key, incoming) in &normalized {
                    match tx.get(key) {
                        Some(stored) if stored == *incoming => {}
                        Some(stored) => {
                            conflict = Some(IngestError::DivergentDuplicate {
                                key: key.clone(),
                                stored: Box::new(stored),
                                incoming: Box::new(incoming.clone()),
                            });
                            return;
                        }
                        None => new_operations.push((key.clone(), incoming.clone())),
                    }
                }

                if after_inserts == 0 || after_inserts > new_operations.len() {
                    nothing_to_interrupt = Some(IngestError::NothingToInterrupt {
                        requested_after: after_inserts,
                        new_operations: new_operations.len(),
                    });
                    return;
                }

                for (index, (key, operation)) in new_operations.iter().enumerate() {
                    tx.upsert(key, operation);
                    if index + 1 == after_inserts {
                        std::panic::panic_any(InterruptionMarker);
                    }
                }
            });
        }));

        if let Some(error) = conflict {
            return Err(error);
        }
        if let Some(error) = nothing_to_interrupt {
            return Err(error);
        }

        match outcome {
            Err(payload) if payload.is::<InterruptionMarker>() => {
                // fjall's single-writer mutex is poisoned by the deliberately
                // injected panic. A real process interruption drops the
                // handle, so reopen here before retrying in this process.
                self.store.take();
                self.store = Some(open_store(&self.path));
                Err(IngestError::SimulatedInterruption { after_inserts })
            }
            Err(payload) => std::panic::resume_unwind(payload),
            Ok(()) => unreachable!("a valid simulated interruption must panic"),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let operations = self
            .store
            .as_ref()
            .expect("model store is open")
            .rtx(|table| table.iter().map(|(_, operation)| operation).collect());
        derive_snapshot(operations)
    }

    pub fn checkpoint(&mut self) {
        self.store
            .as_mut()
            .expect("model store is open")
            .checkpoint();
    }
}

#[derive(Debug)]
struct InterruptionMarker;

fn open_store(path: &Path) -> OperationStore {
    KeyedStream::new(path, terminal::Table::new("immutable_operations"))
}

fn normalize_batch(
    batch: &[Operation],
) -> Result<(BTreeMap<OperationKey, Operation>, usize), IngestError> {
    let mut operations = BTreeMap::new();
    let mut duplicate_replays = 0;

    for operation in batch {
        operation.validate()?;
        let key = operation.key();
        match operations.entry(key.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(operation.clone());
            }
            Entry::Occupied(entry) if entry.get() == operation => {
                duplicate_replays += 1;
            }
            Entry::Occupied(entry) => {
                return Err(IngestError::DivergentDuplicate {
                    key,
                    stored: Box::new(entry.get().clone()),
                    incoming: Box::new(operation.clone()),
                });
            }
        }
    }

    Ok((operations, duplicate_replays))
}

fn derive_snapshot(operations: Vec<Operation>) -> Snapshot {
    let operation_count = operations.len();
    let mut latest_per_device: BTreeMap<(String, String), Operation> = BTreeMap::new();

    for operation in operations {
        let key = (operation.pallet_id.clone(), operation.device_id.clone());
        match latest_per_device.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(operation);
            }
            Entry::Occupied(mut entry) if operation.sequence > entry.get().sequence => {
                entry.insert(operation);
            }
            Entry::Occupied(_) => {}
        }
    }

    let mut by_pallet: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for ((pallet_id, _), operation) in latest_per_device {
        by_pallet
            .entry(pallet_id)
            .or_default()
            .push(operation.into());
    }

    let pallets = by_pallet
        .into_iter()
        .map(|(pallet_id, mut candidates)| {
            candidates.sort();
            let outcomes: BTreeSet<_> = candidates
                .iter()
                .map(|candidate| (candidate.action.clone(), candidate.location.clone()))
                .collect();

            let status = if outcomes.len() == 1 {
                let (action, location) = outcomes
                    .into_iter()
                    .next()
                    .expect("one outcome was observed");
                PalletStatus::Settled {
                    action,
                    location,
                    evidence: candidates,
                }
            } else {
                PalletStatus::Conflict { candidates }
            };

            PalletView { pallet_id, status }
        })
        .collect();

    Snapshot {
        operation_count,
        pallets,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDb {
        path: std::path::PathBuf,
    }

    impl TempDb {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos();
            let counter = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "offline-reconciliation-{label}-{}-{nonce}-{counter}",
                std::process::id()
            ));
            Self { path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn op(
        device_id: &str,
        sequence: u64,
        pallet_id: &str,
        action: &str,
        location: &str,
    ) -> Operation {
        Operation {
            device_id: device_id.to_string(),
            sequence,
            pallet_id: pallet_id.to_string(),
            action: action.to_string(),
            location: location.to_string(),
            operator: format!("operator-{device_id}"),
            device_timestamp_ms: 1_700_000_000_000 + sequence as i64,
        }
    }

    #[test]
    fn duplicate_replay_changes_nothing() {
        let db = TempDb::new("duplicates");
        let mut model = Model::open(&db.path);
        let batch = vec![
            op("scanner-1", 1, "pallet-7", "arrive", "dock"),
            op("scanner-1", 2, "pallet-7", "move", "cold-1"),
        ];

        let first = model.ingest_batch(&batch).expect("first upload succeeds");
        let before = model.snapshot();
        let replay = model.ingest_batch(&batch).expect("replay succeeds");

        assert_eq!(first.inserted, 2);
        assert_eq!(replay.inserted, 0);
        assert_eq!(replay.duplicate_replays, 2);
        assert_eq!(model.snapshot(), before);
    }

    #[test]
    fn divergent_identity_is_rejected_without_partial_commit() {
        let db = TempDb::new("identity-reuse");
        let mut model = Model::open(&db.path);
        let original = op("scanner-1", 1, "pallet-7", "move", "cold-1");
        model
            .ingest_batch(std::slice::from_ref(&original))
            .expect("first upload succeeds");

        let mut altered = original;
        altered.location = "freezer-9".to_string();
        let fresh = op("scanner-2", 1, "pallet-8", "arrive", "dock");
        let before = model.snapshot();

        let error = model
            .ingest_batch(&[fresh, altered])
            .expect_err("identity reuse must be rejected");

        assert!(matches!(error, IngestError::DivergentDuplicate { .. }));
        assert_eq!(model.snapshot(), before);
    }

    #[test]
    fn at_least_one_hundred_random_orders_have_the_same_result() {
        let operations = vec![
            op("scanner-1", 1, "pallet-a", "arrive", "dock"),
            op("scanner-1", 2, "pallet-a", "move", "cold-1"),
            op("scanner-2", 1, "pallet-a", "move", "freezer-2"),
            op("scanner-3", 1, "pallet-a", "move", "cold-1"),
            op("scanner-1", 3, "pallet-b", "arrive", "dock-2"),
            op("scanner-2", 2, "pallet-b", "arrive", "dock-2"),
            op("scanner-4", 1, "pallet-c", "move", "aisle-9"),
            op("scanner-4", 2, "pallet-c", "move", "aisle-10"),
        ];

        let expected_db = TempDb::new("expected");
        let expected = {
            let mut model = Model::open(&expected_db.path);
            model
                .ingest_batch(&operations)
                .expect("canonical upload succeeds");
            model.snapshot()
        };

        for seed in 0..128_u64 {
            let db = TempDb::new("permutation");
            let mut shuffled = operations.clone();
            shuffle(&mut shuffled, seed + 1);
            let mut model = Model::open(&db.path);
            let mut cursor = 0;
            let mut rng = seed + 17;

            while cursor < shuffled.len() {
                rng = next_random(rng);
                let end = (cursor + (rng as usize % 4) + 1).min(shuffled.len());
                model
                    .ingest_batch(&shuffled[cursor..end])
                    .expect("permuted batch succeeds");
                cursor = end;
            }

            assert_eq!(model.snapshot(), expected, "seed {seed} differed");
        }
    }

    #[test]
    fn interrupted_batch_rolls_back_and_retry_loses_nothing() {
        let db = TempDb::new("interruption");
        let mut model = Model::open(&db.path);
        let batch: Vec<_> = (1..=20)
            .map(|sequence| {
                op(
                    "scanner-9",
                    sequence,
                    &format!("pallet-{sequence}"),
                    "arrive",
                    "dock",
                )
            })
            .collect();

        let interrupted = model
            .simulate_interrupted_batch(&batch, 7)
            .expect_err("upload is deliberately interrupted");
        assert_eq!(
            interrupted,
            IngestError::SimulatedInterruption { after_inserts: 7 }
        );
        assert_eq!(model.snapshot().operation_count, 0);

        let retry = model.ingest_batch(&batch).expect("whole retry succeeds");
        assert_eq!(retry.inserted, 20);
        assert_eq!(model.snapshot().operation_count, 20);
    }

    #[test]
    fn incompatible_moves_expose_both_candidates() {
        let db = TempDb::new("conflict");
        let mut model = Model::open(&db.path);
        model
            .ingest_batch(&[
                op("scanner-1", 4, "pallet-7", "move", "cold-1"),
                op("scanner-2", 8, "pallet-7", "move", "freezer-9"),
            ])
            .expect("upload succeeds");

        let snapshot = model.snapshot();
        let PalletStatus::Conflict { candidates } = &snapshot.pallets[0].status else {
            panic!("expected a conflict");
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].location, "cold-1");
        assert_eq!(candidates[1].location, "freezer-9");
    }

    #[test]
    fn state_survives_reopen() {
        let db = TempDb::new("reopen");
        let expected = {
            let mut model = Model::open(&db.path);
            model
                .ingest_batch(&[op("scanner-1", 1, "pallet-7", "arrive", "dock")])
                .expect("upload succeeds");
            model.checkpoint();
            model.snapshot()
        };

        let reopened = Model::open(&db.path);
        assert_eq!(reopened.snapshot(), expected);
    }

    #[test]
    fn twenty_thousand_operations_complete_under_five_seconds() {
        let db = TempDb::new("performance");
        let mut model = Model::open(&db.path);
        let operations = generated_operations(20_000);
        let started = Instant::now();

        let report = model
            .ingest_batch(&operations)
            .expect("large upload succeeds");
        let snapshot = model.snapshot();
        let elapsed = started.elapsed();

        assert_eq!(report.inserted, 20_000);
        assert_eq!(snapshot.operation_count, 20_000);
        assert!(
            elapsed < Duration::from_secs(5),
            "20,000 operations took {elapsed:?}"
        );
    }

    fn generated_operations(count: usize) -> Vec<Operation> {
        (0..count)
            .map(|index| {
                let device = index % 40;
                let sequence = (index / 40 + 1) as u64;
                op(
                    &format!("scanner-{device:02}"),
                    sequence,
                    &format!("pallet-{:04}", index % 1_000),
                    "move",
                    &format!("zone-{:02}", (index / 1_000) % 20),
                )
            })
            .collect()
    }

    fn shuffle<T>(values: &mut [T], mut state: u64) {
        for index in (1..values.len()).rev() {
            state = next_random(state);
            values.swap(index, state as usize % (index + 1));
        }
    }

    fn next_random(state: u64) -> u64 {
        state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
    }
}
