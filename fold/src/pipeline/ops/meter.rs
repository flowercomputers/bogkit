use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

use crate::{
    pipeline::Push,
    stream::{PipelineInitCtx, Readable, WriteTx},
};

/// Live counters behind one [`Meter`]. Read with [`snap`](MeterCounters::snap).
pub struct MeterCounters {
    pub name: String,
    pushes: AtomicU64,
    inserts: AtomicU64,
    retracts: AtomicU64,
    /// Σ|delta| — multiplicity-weighted push count.
    weight: AtomicU64,
    commits: AtomicU64,
    aborts: AtomicU64,
    ns_downstream: AtomicU64,
    ns_commit: AtomicU64,
}
impl MeterCounters {
    fn new(name: String) -> Self {
        MeterCounters {
            name,
            pushes: AtomicU64::new(0),
            inserts: AtomicU64::new(0),
            retracts: AtomicU64::new(0),
            weight: AtomicU64::new(0),
            commits: AtomicU64::new(0),
            aborts: AtomicU64::new(0),
            ns_downstream: AtomicU64::new(0),
            ns_commit: AtomicU64::new(0),
        }
    }

    /// Point-in-time copy of the counters.
    pub fn snap(&self) -> MeterSnap {
        MeterSnap {
            name: self.name.clone(),
            pushes: self.pushes.load(Ordering::Relaxed),
            inserts: self.inserts.load(Ordering::Relaxed),
            retracts: self.retracts.load(Ordering::Relaxed),
            weight: self.weight.load(Ordering::Relaxed),
            commits: self.commits.load(Ordering::Relaxed),
            aborts: self.aborts.load(Ordering::Relaxed),
            ns_downstream: self.ns_downstream.load(Ordering::Relaxed),
            ns_commit: self.ns_commit.load(Ordering::Relaxed),
        }
    }
}

/// A snapshot of one meter's counters. Cumulative since the process started;
/// [`diff`](MeterSnap::diff) two snapshots to get the numbers for one
/// transaction (or any window).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct MeterSnap {
    pub name: String,
    /// `push` calls seen.
    pub pushes: u64,
    /// Pushes with a positive delta.
    pub inserts: u64,
    /// Pushes with a negative delta.
    pub retracts: u64,
    /// Σ|delta| across all pushes.
    pub weight: u64,
    pub commits: u64,
    pub aborts: u64,
    /// Nanoseconds spent inside `next.push` — the whole subtree below the
    /// meter, so `outer.ns_downstream - inner.ns_downstream` isolates the
    /// stage(s) between two nested meters.
    pub ns_downstream: u64,
    /// Nanoseconds spent inside `next.commit`. Stateful nodes and sinks do
    /// their store writes here, not in `push`, so a stage's real cost is
    /// `ns_downstream + ns_commit` — see [`ns_total`](MeterSnap::ns_total).
    pub ns_commit: u64,
}
impl MeterSnap {
    /// `self - earlier`, saturating; the name is kept from `self`.
    pub fn diff(&self, earlier: &MeterSnap) -> MeterSnap {
        MeterSnap {
            name: self.name.clone(),
            pushes: self.pushes.saturating_sub(earlier.pushes),
            inserts: self.inserts.saturating_sub(earlier.inserts),
            retracts: self.retracts.saturating_sub(earlier.retracts),
            weight: self.weight.saturating_sub(earlier.weight),
            commits: self.commits.saturating_sub(earlier.commits),
            aborts: self.aborts.saturating_sub(earlier.aborts),
            ns_downstream: self.ns_downstream.saturating_sub(earlier.ns_downstream),
            ns_commit: self.ns_commit.saturating_sub(earlier.ns_commit),
        }
    }

    /// Push time plus commit time below this meter.
    pub fn ns_total(&self) -> u64 {
        self.ns_downstream + self.ns_commit
    }
}

/// Process-local registry of [`Meter`] counters. Clone it freely — clones
/// share the same table — and hand a reference to each `Meter::new`.
#[derive(Clone, Default)]
pub struct MeterRegistry(Arc<Mutex<Vec<Arc<MeterCounters>>>>);
impl MeterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim a meter name. Panics on a duplicate, like sink names do.
    pub fn register(&self, name: &str) -> Arc<MeterCounters> {
        let mut table = self.0.lock().unwrap();
        assert!(
            table.iter().all(|c| c.name != name),
            "duplicate meter name: {name}"
        );
        let c = Arc::new(MeterCounters::new(name.to_string()));
        table.push(c.clone());
        c
    }

    /// Snapshot every registered meter, in registration order.
    pub fn snapshot(&self) -> Vec<MeterSnap> {
        self.0.lock().unwrap().iter().map(|c| c.snap()).collect()
    }

    /// Snapshot one meter by name.
    pub fn get(&self, name: &str) -> Option<MeterSnap> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.snap())
    }
}

/// Stateless tap: forwards every delta unchanged and counts it, timing the
/// call into the downstream node.
///
/// Register the meter under a name in a [`MeterRegistry`] and read
/// [`MeterSnap`]s back out of the registry — the app diffs snapshots taken
/// around a transaction to say "this edit was 2 deltas, 213µs" with numbers
/// that came from inside the pipeline rather than a stopwatch around it.
/// Counters are process-local and never persisted — they describe *this
/// run*, not the store.
///
/// `ns_downstream` (push) and `ns_commit` (commit) each measure the whole
/// subtree below the meter, so nesting two meters and subtracting isolates
/// the stage(s) between them. Sinks write at commit, so read both.
pub struct Meter<D, G> {
    counters: Arc<MeterCounters>,
    pub next: G,
    _p: PhantomData<D>,
}
impl<D: Clone, G: Push<D>> Meter<D, G> {
    /// `name` must be unique within `registry`.
    pub fn new(registry: &MeterRegistry, name: impl Into<String>, next: G) -> Self {
        Meter {
            counters: registry.register(&name.into()),
            next,
            _p: PhantomData,
        }
    }

    /// The live counters this meter writes.
    pub fn counters(&self) -> Arc<MeterCounters> {
        self.counters.clone()
    }
}
impl<D: Clone, G: Push<D>> Push<D> for Meter<D, G> {
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;
    #[inline]
    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.next.init(init)
    }
    #[inline]
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        let c = &self.counters;
        c.pushes.fetch_add(1, Ordering::Relaxed);
        if delta >= 0 {
            c.inserts.fetch_add(1, Ordering::Relaxed);
        } else {
            c.retracts.fetch_add(1, Ordering::Relaxed);
        }
        c.weight
            .fetch_add(delta.unsigned_abs() as u64, Ordering::Relaxed);
        let t = Instant::now();
        self.next.push(tx, data, delta);
        c.ns_downstream
            .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    #[inline]
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        self.counters.commits.fetch_add(1, Ordering::Relaxed);
        let t = Instant::now();
        self.next.commit(tx);
        self.counters
            .ns_commit
            .fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    #[inline]
    fn abort(&mut self) {
        self.counters.aborts.fetch_add(1, Ordering::Relaxed);
        self.next.abort()
    }
    #[inline]
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}
