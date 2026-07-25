//! Pipeline operators and the [`Push`] trait that composes them.
//!
//! A pipeline is a tree of [`Push`] nodes built inside-out: every operator
//! owns its downstream (`next`), so the whole graph is one concrete type and
//! all calls are statically dispatched. Interior nodes transform or route
//! data; the leaves are [`terminal`] sinks that persist state.
//!
//! # Deltas
//! Data flows through the graph as `(data, delta)` pairs, where `delta` is a
//! signed multiplicity: `+n` inserts `n` copies of `data`, `-n` retracts
//! them. Every operator and sink honors retraction, which is what makes the
//! materialized state incrementally maintainable — pushing a record and later
//! pushing it again with the opposite delta leaves every sink unchanged.
//!
//! # Operators
//! Stateless operators forward transformed deltas immediately:
//! - [`Map`] — apply a function to each datum
//! - [`Filter`] — drop data failing a predicate
//! - [`FilterMap`] — map and filter in one step
//! - [`FlatMap`] — expand each datum into zero or more outputs
//!
//! Stateful operators persist per-element state in their own keyspace,
//! buffering within a transaction and emitting downstream deltas at commit:
//! - [`Distinct`] — collapse multiplicities to set semantics
//! - [`Aggregate`] — per-key incremental aggregation
//! - [`TopK`] — retain the k highest-scored records, retracting the rest
//! - [`Retain`] — retract records once they age past a wall-clock horizon
//!
//! Keying operators convert between plain and [`Keyed`] streams:
//! - [`KeyBy`] — attach a key extracted from each datum
//! - [`Unkey`] — discard the key, forwarding the value
//!
//! Scoring operators do the same for [`Scored`] streams:
//! - [`ScoreBy`] — attach an ordering score extracted from each datum
//! - [`Unscore`] — discard the score, forwarding the value
//!
//! # Fan-out
//! Tuples of `Push` nodes (up to 16 elements) implement `Push` by
//! broadcasting each delta to every element, splitting a pipeline into
//! parallel branches. The tuple's reader is the tuple of its elements'
//! readers.

use crate::stream::Readable;

use crate::stream::{PipelineInitCtx, WriteTx};

pub mod terminal;

mod ops;
pub use ops::*;
use serde::{Deserialize, Serialize};

// contains (A,B,C..) tuples implementing Push for use as tee/tap
mod tuple;

/// A pipeline node that accepts a stream of `(data, delta)` pairs.
///
/// Implemented by operators (which transform and forward), sinks (which
/// persist), and tuples of nodes (which fan out). A node's lifecycle:
///
/// 1. [`init`](Push::init) — once, when the owning
///    [`Stream`](crate::stream::Stream) opens: resolve keyspaces, claim sink
///    names.
/// 2. [`push`](Push::push) — once per delta within a write transaction.
///    Stateful nodes typically buffer here rather than touching the store.
/// 3. [`commit`](Push::commit) — as the transaction completes, and before
///    each mid-transaction read ([`Tx::rtx`](crate::stream::Tx::rtx)):
///    flush buffered state and emit any resulting downstream deltas. This
///    can run several times per transaction, with pushes resuming in
///    between, so committing must leave the node ready for more deltas.
/// 4. [`abort`](Push::abort) — instead of the final `commit` if the
///    transaction panics: discard buffered state so the node is clean for
///    the next transaction. Store writes from earlier `commit` calls in the
///    aborted transaction roll back with it.
///
/// Operators must propagate `commit`/`abort`/`reader` to their downstream
/// node(s) even when they hold no state themselves.
pub trait Push<D: Clone> {
    /// Typed, lazy view over a pinned snapshot.
    /// Publically accessible in through read TXs; only relevant to sinks.
    /// Operators pass their downstream's reader through unchanged, so a
    /// pipeline's reader mirrors its sink structure.
    type Reader<'tx, R: Readable + 'tx>;

    /// Resolve keyspace handles and register sink names.
    fn init(&mut self, init: &mut PipelineInitCtx<'_>);

    /// Accept one delta: `+n` inserts `n` copies of `data`, `-n` retracts
    /// them.
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize);

    /// Flushes pending state to the store, emitting any resulting
    /// downstream deltas. Called as the transaction completes and before
    /// each mid-transaction read ([`Tx::rtx`](crate::stream::Tx::rtx)), so
    /// it may run several times per transaction and must leave the node
    /// ready to accept further pushes.
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        let _ = tx;
    }

    /// Drop pending state to reset node for the next transaction.
    /// Called once if the transaction fails.
    fn abort(&mut self) {}

    /// Get a read handle to the sink.
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R>;
}

impl<D: Clone, T: Push<D>> Push<D> for &mut T {
    type Reader<'tx, R: Readable + 'tx> = T::Reader<'tx, R>;
    #[inline]
    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        (**self).init(init)
    }
    #[inline]
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        (**self).push(tx, data, delta)
    }
    #[inline]
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        (**self).commit(tx)
    }
    #[inline]
    fn abort(&mut self) {
        (**self).abort()
    }
    #[inline]
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        (**self).reader(tx)
    }
}

/// A value paired with a grouping key.
///
/// The currency of keyed operators: [`KeyBy`] produces `Keyed` streams,
/// [`Aggregate`] consumes and emits them, and [`Unkey`] strips the key back
/// off.
#[derive(Clone, Serialize, Deserialize)]
pub struct Keyed<K, V> {
    pub key: K,
    pub val: V,
}
impl<K, V> Keyed<K, V> {
    pub fn new(key: K, val: V) -> Self {
        Keyed { key, val }
    }

    /// Key `val` by a function of itself.
    pub fn new_by<F: Fn(&V) -> K>(val: V, key_fn: F) -> Self {
        Keyed {
            key: (key_fn)(&val),
            val,
        }
    }

    /// Discard the key.
    pub fn unkey(self) -> V {
        self.val
    }
}

/// A value paired with an ordering score.
///
/// The currency of scored operators: [`ScoreBy`] produces `Scored` streams,
/// [`TopK`] consumes and forwards them, and [`Unscore`] strips the score
/// back off. Unlike [`Keyed`] keys, scores are ordered — see the
/// [`Score`] trait. Scoring by event timestamp turns [`TopK`] into a
/// sliding "most recent k records" window.
#[derive(Clone, Serialize, Deserialize)]
pub struct Scored<S, V> {
    pub score: S,
    pub val: V,
}
impl<S, V> Scored<S, V> {
    pub fn new(score: S, val: V) -> Self {
        Scored { score, val }
    }

    /// Score `val` by a function of itself.
    pub fn new_by<F: Fn(&V) -> S>(val: V, score_fn: F) -> Self {
        Scored {
            score: (score_fn)(&val),
            val,
        }
    }

    /// Discard the score.
    pub fn unscore(self) -> V {
        self.val
    }
}
