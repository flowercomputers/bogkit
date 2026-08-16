mod keyed;
pub use keyed::*;

mod meter;
pub use meter::*;

mod retain;
pub use retain::*;

mod scored;
pub use scored::*;

use fxhash::FxHashMap;
use serde::Serialize;
use std::marker::PhantomData;

use crate::{
    pipeline::Push,
    stream::{PipelineInitCtx, Readable, WriteTx},
};

/// Forwards only data for which `pred` returns `true`.
///
/// Stateless; deltas pass through unchanged. Retractions are filtered by the
/// same predicate, so a record's insert and remove either both reach
/// downstream or neither does.
pub struct Filter<D, F, G> {
    pub pred: F,
    pub next: G,
    _p: PhantomData<D>,
}
impl<D: Clone, F: Fn(&D) -> bool, G: Push<D>> Filter<D, F, G> {
    pub fn new(pred: F, next: G) -> Filter<D, F, G> {
        Filter {
            pred,
            next,
            _p: PhantomData,
        }
    }
}
impl<F: Fn(&D) -> bool, G: Push<D>, D: Clone> Push<D> for Filter<D, F, G> {
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;
    #[inline]
    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.next.init(init)
    }
    #[inline]
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        if (self.pred)(data) {
            self.next.push(tx, data, delta)
        }
    }
    #[inline]
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        self.next.commit(tx)
    }
    #[inline]
    fn abort(&mut self) {
        self.next.abort()
    }
    #[inline]
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}

/// Applies `func` to each datum, forwarding the result.
///
/// Stateless; deltas pass through unchanged. `func` must be deterministic —
/// a retraction re-maps the original datum and must produce the same output
/// that was inserted, or downstream state will not cancel.
pub struct Map<F, G, D, O> {
    pub func: F,
    pub next: G,
    _p: PhantomData<(D, O)>,
}
impl<O: Clone, F: Fn(&D) -> O, G: Push<O>, D: Clone> Map<F, G, D, O> {
    pub fn new(func: F, next: G) -> Self {
        Map {
            func,
            next,
            _p: PhantomData,
        }
    }
}
impl<O: Clone, F: Fn(&D) -> O, G: Push<O>, D: Clone> Push<D> for Map<F, G, D, O> {
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;
    #[inline]
    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.next.init(init)
    }
    #[inline]
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        self.next.push(tx, &(self.func)(data), delta);
    }
    #[inline]
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        self.next.commit(tx)
    }
    #[inline]
    fn abort(&mut self) {
        self.next.abort()
    }
    #[inline]
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}

/// [`Map`] and [`Filter`] in one step: forwards `Some` results, drops `None`.
///
/// Stateless; deltas pass through unchanged. Like [`Map`], `func` must be
/// deterministic for retractions to cancel.
pub struct FilterMap<F, G, D, O> {
    pub func: F,
    pub next: G,
    _p: PhantomData<(D, O)>,
}
impl<D: Clone, O: Clone, F: Fn(&D) -> Option<O>, G: Push<O>> FilterMap<F, G, D, O> {
    pub fn new(func: F, next: G) -> Self {
        FilterMap {
            func,
            next,
            _p: PhantomData,
        }
    }
}
impl<D: Clone, O: Clone, F: Fn(&D) -> Option<O>, G: Push<O>> Push<D> for FilterMap<F, G, D, O> {
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;
    #[inline]
    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.next.init(init)
    }
    #[inline]
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        if let Some(o) = (self.func)(data) {
            self.next.push(tx, &o, delta);
        }
    }
    #[inline]
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        self.next.commit(tx)
    }
    #[inline]
    fn abort(&mut self) {
        self.next.abort()
    }
    #[inline]
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}

/// Expands each datum into zero or more outputs, forwarding each with the
/// input's delta.
///
/// Stateless. Like [`Map`], `func` must be deterministic: a retraction
/// re-expands the original datum and pushes each output with the negated
/// delta.
pub struct FlatMap<F, G, D, O> {
    pub func: F,
    pub next: G,
    _p: PhantomData<(D, O)>,
}
impl<I: IntoIterator<Item = O>, F: Fn(&D) -> I, G: Push<O>, D: Clone, O: Clone>
    FlatMap<F, G, D, O>
{
    pub fn new(func: F, next: G) -> Self {
        FlatMap {
            func,
            next,
            _p: PhantomData,
        }
    }
}
impl<D, O, I, F, G> Push<D> for FlatMap<F, G, D, O>
where
    D: Clone,
    O: Clone,
    I: IntoIterator<Item = O>,
    F: Fn(&D) -> I,
    G: Push<O>,
{
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;
    #[inline]
    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.next.init(init)
    }
    #[inline]
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        for item in (self.func)(data) {
            self.next.push(tx, &item, delta);
        }
    }
    #[inline]
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        self.next.commit(tx)
    }
    #[inline]
    fn abort(&mut self) {
        self.next.abort()
    }
    #[inline]
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}

/// Collapses multiplicity to {0, 1}. Stateful: persists per-element counts in
/// its own keyspace, emits +1 downstream when a count crosses 0→positive and
/// -1 when it crosses positive→0. Emission happens at commit time so hot
/// elements collapse to at most one downstream delta per commit.
pub struct Distinct<D, G> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    // encoded data -> (decoded data for re-emission, accumulated delta this tx)
    pending: FxHashMap<Vec<u8>, (D, i64)>,
    pub next: G,
}
impl<D, G> Distinct<D, G> {
    /// `name` identifies this node's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>, next: G) -> Self {
        Distinct {
            name: name.into(),
            ks: None,
            pending: FxHashMap::default(),
            next,
        }
    }
}
impl<D: Clone + Serialize, G: Push<D>> Push<D> for Distinct<D, G> {
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
        self.next.init(init);
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        tx.buf.clear();
        postcard::to_io(data, &mut tx.buf).unwrap();
        self.pending
            .entry(tx.buf.clone())
            .or_insert_with(|| (data.clone(), 0))
            .1 += delta as i64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        let ks = self.ks.clone().unwrap();
        for (key, (data, delta)) in self.pending.drain() {
            if delta == 0 {
                continue;
            }
            let cur = tx
                .get(&ks, &key)
                .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
                .unwrap_or(0);
            let new = cur + delta;
            debug_assert!(new >= 0, "distinct multiplicity went negative");

            if new > 0 {
                tx.insert(&ks, &key, new.to_be_bytes());
            } else {
                tx.remove(&ks, &key);
            }
            match (cur > 0, new > 0) {
                (false, true) => self.next.push(tx, &data, 1),
                (true, false) => self.next.push(tx, &data, -1),
                _ => {}
            }
        }
        self.next.commit(tx);
    }

    fn abort(&mut self) {
        self.pending.clear();
        self.next.abort();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}
