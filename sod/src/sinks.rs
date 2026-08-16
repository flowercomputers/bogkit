//! Replication-safe fold sinks (feature `fold-engine`).
//!
//! A sink in a sod pipeline must be a **pure function of the net multiset**:
//! under replication, a retraction can arrive before its matching insert,
//! and any sink that clamps or drops on that transient makes its state
//! depend on delta arrival order — replicas diverge (SOD-4).
//!
//! Fold's stock `Bag` clamps (a negative running sum is stored as absent,
//! so `-1` then `+2` converges differently than `+2` then `-1`). Rather
//! than patch fold — sod deliberately consumes fold's public API without
//! modifying it — this module provides sinks with order-independent
//! semantics. Fold sinks that are already pure functions of the multiset
//! (e.g. `Count`, a plain signed sum) can be used as-is.

use std::collections::BTreeMap;
use std::marker::PhantomData;

use fold::pipeline::Push;
use fold::stream::{PipelineInitCtx, Readable, WriteTx};
use serde::{Serialize, de::DeserializeOwned};

/// Persistent counted multiset: each distinct element maps to its net
/// multiplicity.
///
/// Elements are stored by their `postcard` encoding. Any **nonzero**
/// running sum is persisted — including negative ones — so the stored
/// state is a pure function of the net multiset regardless of delta
/// arrival order. Readers surface only positive multiplicities.
pub struct Bag<D> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    // key-encoded data -> accumulated delta this tx (BTreeMap: deterministic
    // flush order, per the no-hash-iteration rule at output boundaries)
    pending: BTreeMap<Vec<u8>, i64>,
    _p: PhantomData<D>,
}

impl<D> Bag<D> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Bag {
            name: name.into(),
            ks: None,
            pending: BTreeMap::new(),
            _p: PhantomData,
        }
    }
}

/// Read handle for [`Bag`], pinned to one snapshot.
pub struct BagReader<'tx, R: Readable, D> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    _p: PhantomData<D>,
}

impl<'tx, R: Readable, D: DeserializeOwned> BagReader<'tx, R, D> {
    /// Iterate all `(element, multiplicity)` pairs with multiplicity > 0,
    /// ordered by the element's `postcard` encoding. Elements currently at
    /// a negative running sum (retraction seen before its insert) are
    /// skipped.
    pub fn iter(&self) -> impl Iterator<Item = (D, i64)> + '_ {
        self.tx.iter(&self.ks).filter_map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            let n = i64::from_be_bytes(val.as_ref().try_into().unwrap());
            if n <= 0 {
                return None;
            }
            let d: D = postcard::from_bytes(&key).unwrap();
            Some((d, n))
        })
    }

    /// Whether `d` has multiplicity > 0.
    pub fn contains(&self, d: &D) -> bool
    where
        D: Serialize,
    {
        let key = postcard::to_stdvec(d).unwrap();
        self.tx
            .get(&self.ks, &key)
            .unwrap()
            .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()) > 0)
            .unwrap_or(false)
    }
}

impl<D: Clone + Serialize + DeserializeOwned> Push<D> for Bag<D> {
    type Reader<'tx, R: Readable + 'tx> = BagReader<'tx, R, D>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        tx.buf.clear();
        postcard::to_io(data, &mut tx.buf).unwrap();
        *self.pending.entry(tx.buf.clone()).or_insert(0) += delta as i64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        let ks = self.ks.clone().unwrap();
        while let Some((key, delta)) = self.pending.pop_first() {
            if delta == 0 {
                continue;
            }
            let cur = tx
                .get(&ks, &key)
                .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
                .unwrap_or(0);
            let new = cur + delta;
            if new != 0 {
                tx.insert(&ks, &key, new.to_be_bytes());
            } else {
                tx.remove(&ks, &key);
            }
        }
    }

    fn abort(&mut self) {
        self.pending.clear();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        BagReader {
            tx,
            ks: self.ks.clone().unwrap(),
            _p: PhantomData,
        }
    }
}
