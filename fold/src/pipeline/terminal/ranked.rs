use std::{
    marker::PhantomData,
    ops::{Bound, RangeBounds},
};

use fjall::Readable;
use fxhash::FxHashMap;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    pipeline::{Keyed, Push, Score, Scored},
    stream::{PipelineInitCtx, WriteTx},
};

// read-modify-write a pending delta map into stored multiplicities,
// removing entries whose count reaches 0 (Bag-style)
fn fold_counts(
    tx: &mut WriteTx<'_>,
    ks: &fjall::SingleWriterTxKeyspace,
    pending: &mut FxHashMap<Vec<u8>, i64>,
) {
    for (key, delta) in pending.drain() {
        if delta == 0 {
            continue;
        }
        let cur = tx
            .get(ks, &key)
            .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
            .unwrap_or(0);
        let new = cur + delta;
        debug_assert!(new >= 0, "ranked multiplicity went negative");
        if new > 0 {
            tx.insert(ks, &key, new.to_be_bytes());
        } else {
            tx.remove(ks, &key);
        }
    }
}

// The smallest byte string strictly greater than every key starting with
// `p`: increment the last non-0xFF byte and truncate. `None` (all 0xFF)
// means no such string exists — but every key that sorts above `p` then
// necessarily starts with `p`, so an unbounded upper end is equivalent.
fn prefix_successor(mut p: Vec<u8>) -> Option<Vec<u8>> {
    while let Some(&last) = p.last() {
        if last == 0xFF {
            p.pop();
        } else {
            *p.last_mut().unwrap() += 1;
            return Some(p);
        }
    }
    None
}

type ByteBounds = (Bound<Vec<u8>>, Bound<Vec<u8>>);

// Convert score bounds into byte bounds over `prefix ++ Score ++ postcard(V)`
// keys. Returns `None` for a provably empty range (excluded lower bound with
// no successor).
fn byte_bounds<S: Score>(prefix: &[u8], range: impl RangeBounds<S>) -> Option<ByteBounds> {
    let enc = |s: &S| {
        let mut b = prefix.to_vec();
        s.encode(&mut b);
        b
    };
    let lo = match range.start_bound() {
        Bound::Unbounded => Bound::Included(prefix.to_vec()),
        Bound::Included(s) => Bound::Included(enc(s)),
        // every entry at score `s` extends `enc(s)`, so the first key past
        // all of them is the prefix successor
        Bound::Excluded(s) => Bound::Included(prefix_successor(enc(s))?),
    };
    let hi = match range.end_bound() {
        Bound::Unbounded => match prefix_successor(prefix.to_vec()) {
            Some(k) => Bound::Excluded(k),
            None => Bound::Unbounded,
        },
        Bound::Excluded(s) => Bound::Excluded(enc(s)),
        Bound::Included(s) => match prefix_successor(enc(s)) {
            Some(k) => Bound::Excluded(k),
            None => Bound::Unbounded,
        },
    };
    Some((lo, hi))
}

// a fjall iterator or provably-nothing, for ranges whose byte bounds don't
// exist; keeps `range` double-ended (TakeWhile-style guards are not)
struct MaybeIter(Option<fjall::Iter>);
impl Iterator for MaybeIter {
    type Item = <fjall::Iter as Iterator>::Item;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.as_mut()?.next()
    }
}
impl DoubleEndedIterator for MaybeIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.as_mut()?.next_back()
    }
}

fn decode_scored<S: Score, V: DeserializeOwned>(
    key: &[u8],
    val: &[u8],
    skip: usize,
) -> (Scored<S, V>, i64) {
    let (score, rest) = S::decode(&key[skip..]);
    (
        Scored::new(score, postcard::from_bytes(rest).unwrap()),
        i64::from_be_bytes(val.try_into().unwrap()),
    )
}

// expand `(record, count)` pairs into up to `n` record copies
fn take_copies<S: Score, V: Clone>(
    it: impl Iterator<Item = (Scored<S, V>, i64)>,
    n: usize,
) -> Vec<Scored<S, V>> {
    let mut out = Vec::with_capacity(n);
    for (scored, count) in it {
        if out.len() >= n {
            break;
        }
        for _ in 1..count.min((n - out.len()) as i64) {
            out.push(scored.clone());
        }
        out.push(scored);
    }
    out
}

/// Persistent multiset of [`Scored`]`<S, V>` records ordered by score: the
/// readable counterpart of [`TopK`](crate::pipeline::TopK), with the cutoff
/// chosen at read time instead of ingest time.
///
/// Records are stored by their [`Score`] encoding (ties broken by the
/// value's `postcard` encoding) with per-record multiplicities, so
/// [`min`](RankedReader::min)/[`max`](RankedReader::max) are single seeks
/// and [`top`](RankedReader::top)/[`range`](RankedReader::range) are
/// contiguous scans. Scoring by timestamp makes this a time index: "events
/// in `[t1, t2)`", "latest n".
///
/// Deltas accumulate multiset-style; retracting a record decrements its
/// multiplicity and removes it at 0, revealing the runner-up to `min`/`max`.
pub struct Ranked<S, V> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    // encoded (score, val) -> accumulated delta this tx
    pending: FxHashMap<Vec<u8>, i64>,
    _p: PhantomData<(S, V)>,
}

impl<S, V> Ranked<S, V> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Ranked {
            name: name.into(),
            ks: None,
            pending: FxHashMap::default(),
            _p: PhantomData,
        }
    }
}

impl<S: Score, V: Clone + Serialize + DeserializeOwned> Push<Scored<S, V>> for Ranked<S, V> {
    type Reader<'tx, R: Readable + 'tx> = RankedReader<'tx, R, S, V>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Scored<S, V>, delta: isize) {
        tx.buf.clear();
        data.score.encode(&mut tx.buf);
        postcard::to_io(&data.val, &mut tx.buf).unwrap();
        *self.pending.entry(tx.buf.clone()).or_insert(0) += delta as i64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        let ks = self.ks.clone().unwrap();
        fold_counts(tx, &ks, &mut self.pending);
    }

    fn abort(&mut self) {
        self.pending.clear();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        RankedReader {
            tx,
            ks: self.ks.clone().unwrap(),
            name: self.name.clone(),
            _p: PhantomData,
        }
    }
}

/// Read handle for [`Ranked`], pinned to one snapshot.
pub struct RankedReader<'tx, R: Readable, S, V> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    name: String,
    _p: PhantomData<(S, V)>,
}

impl<'tx, R: Readable, S, V> RankedReader<'tx, R, S, V> {
    /// The sink name this reader serves, as given to [`Ranked::new`].
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<'tx, R: Readable, S: Score, V: Clone + DeserializeOwned> RankedReader<'tx, R, S, V> {
    /// The lowest-scored record, if any.
    pub fn min(&self) -> Option<Scored<S, V>> {
        self.tx.first_key_value(&self.ks).map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            decode_scored(&key, &val, 0).0
        })
    }

    /// The highest-scored record, if any.
    pub fn max(&self) -> Option<Scored<S, V>> {
        self.tx.last_key_value(&self.ks).map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            decode_scored(&key, &val, 0).0
        })
    }

    /// Iterate all `(record, multiplicity)` pairs in ascending score order;
    /// `.rev()` scans descending.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (Scored<S, V>, i64)> + '_ {
        self.tx.iter(&self.ks).map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            decode_scored(&key, &val, 0)
        })
    }

    /// Iterate `(record, multiplicity)` pairs whose score lies in `range`,
    /// ascending; `.rev()` scans descending.
    pub fn range(
        &self,
        range: impl RangeBounds<S>,
    ) -> impl DoubleEndedIterator<Item = (Scored<S, V>, i64)> + '_ {
        let bounds = byte_bounds(&[], range);
        MaybeIter(bounds.map(|b| self.tx.range(&self.ks, b))).map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            decode_scored(&key, &val, 0)
        })
    }

    /// The `n` highest-scored record copies, descending. Multiplicities
    /// count: a record held twice occupies two slots.
    pub fn top(&self, n: usize) -> Vec<Scored<S, V>> {
        take_copies(self.iter().rev(), n)
    }

    /// The `n` lowest-scored record copies, ascending.
    pub fn bottom(&self, n: usize) -> Vec<Scored<S, V>> {
        take_copies(self.iter(), n)
    }
}

/// Persistent per-key multisets of [`Scored`]`<S, V>` records, each ordered
/// by score: [`Ranked`] grouped by `K`.
///
/// Accepts `Keyed { key, val: Scored { score, val } }` and stores records
/// under `postcard(K) ++ Score ++ postcard(V)`, so each key's records are
/// one contiguous, score-ordered run: per-key
/// [`min`](KeyedRankedReader::min)/[`max`](KeyedRankedReader::max)/
/// [`top`](KeyedRankedReader::top)/[`range`](KeyedRankedReader::range) are
/// prefix scans. Retraction decrements multiplicities and reveals the
/// runner-up, which is what makes min/max retractable at all — the full
/// per-key multiset is retained.
pub struct KeyedRanked<K, S, V> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    // encoded (key, score, val) -> accumulated delta this tx
    pending: FxHashMap<Vec<u8>, i64>,
    _p: PhantomData<(K, S, V)>,
}

impl<K, S, V> KeyedRanked<K, S, V> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        KeyedRanked {
            name: name.into(),
            ks: None,
            pending: FxHashMap::default(),
            _p: PhantomData,
        }
    }
}

impl<K, S, V> Push<Keyed<K, Scored<S, V>>> for KeyedRanked<K, S, V>
where
    K: Clone + Serialize,
    S: Score,
    V: Clone + Serialize + DeserializeOwned,
{
    type Reader<'tx, R: Readable + 'tx> = KeyedRankedReader<'tx, R, K, S, V>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, Scored<S, V>>, delta: isize) {
        tx.buf.clear();
        postcard::to_io(&data.key, &mut tx.buf).unwrap();
        data.val.score.encode(&mut tx.buf);
        postcard::to_io(&data.val.val, &mut tx.buf).unwrap();
        *self.pending.entry(tx.buf.clone()).or_insert(0) += delta as i64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        let ks = self.ks.clone().unwrap();
        fold_counts(tx, &ks, &mut self.pending);
    }

    fn abort(&mut self) {
        self.pending.clear();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        KeyedRankedReader {
            tx,
            ks: self.ks.clone().unwrap(),
            _p: PhantomData,
        }
    }
}

/// Read handle for [`KeyedRanked`], pinned to one snapshot.
pub struct KeyedRankedReader<'tx, R: Readable, K, S, V> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    _p: PhantomData<(K, S, V)>,
}

impl<'tx, R, K, S, V> KeyedRankedReader<'tx, R, K, S, V>
where
    R: Readable,
    K: Serialize,
    S: Score,
    V: Clone + DeserializeOwned,
{
    // postcard encodings of one type are prefix-free, so the prefix scan
    // only matches this key's entries
    fn prefix(&self, key: &K) -> Vec<u8> {
        postcard::to_stdvec(key).unwrap()
    }

    /// The lowest-scored record under `key`, if any.
    pub fn min(&self, key: &K) -> Option<Scored<S, V>> {
        self.iter(key).next().map(|(s, _)| s)
    }

    /// The highest-scored record under `key`, if any.
    pub fn max(&self, key: &K) -> Option<Scored<S, V>> {
        self.iter(key).next_back().map(|(s, _)| s)
    }

    /// Iterate `key`'s `(record, multiplicity)` pairs in ascending score
    /// order; `.rev()` scans descending.
    pub fn iter(&self, key: &K) -> impl DoubleEndedIterator<Item = (Scored<S, V>, i64)> + '_ {
        let prefix = self.prefix(key);
        let plen = prefix.len();
        self.tx.prefix(&self.ks, prefix).map(move |kv| {
            let (key, val) = kv.into_inner().unwrap();
            decode_scored(&key, &val, plen)
        })
    }

    /// Iterate `key`'s `(record, multiplicity)` pairs whose score lies in
    /// `range`, ascending; `.rev()` scans descending.
    pub fn range(
        &self,
        key: &K,
        range: impl RangeBounds<S>,
    ) -> impl DoubleEndedIterator<Item = (Scored<S, V>, i64)> + '_ {
        let prefix = self.prefix(key);
        let plen = prefix.len();
        let bounds = byte_bounds(&prefix, range);
        MaybeIter(bounds.map(|b| self.tx.range(&self.ks, b))).map(move |kv| {
            let (key, val) = kv.into_inner().unwrap();
            decode_scored(&key, &val, plen)
        })
    }

    /// The `n` highest-scored record copies under `key`, descending.
    pub fn top(&self, key: &K, n: usize) -> Vec<Scored<S, V>> {
        take_copies(self.iter(key).rev(), n)
    }

    /// The `n` lowest-scored record copies under `key`, ascending.
    pub fn bottom(&self, key: &K, n: usize) -> Vec<Scored<S, V>> {
        take_copies(self.iter(key), n)
    }
}
