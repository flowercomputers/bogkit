//! Terminal sinks: pipeline leaves that persist materialized state.
//!
//! Each sink owns a named keyspace in the store (registered during
//! [`init`](Push::init); names must be unique pipeline-wide) and exposes a
//! typed reader through [`Push::Reader`], obtained inside
//! [`Stream::rtx`](crate::stream::Stream::rtx) — or mid-transaction inside
//! [`Tx::rtx`](crate::stream::Tx::rtx), where readers additionally see the
//! transaction's own uncommitted deltas.
//!
//! # Sinks over any data
//! - [`Count`] — the number of live records
//! - [`Bag`] — counted multiset: membership and iteration with
//!   multiplicities
//! - [`Stats`] — running moments of a numeric projection: count, sum,
//!   mean, variance
//!
//! # Sinks over [`Keyed`]`<K, V>` data
//! Produced by [`KeyBy`](crate::pipeline::KeyBy), emitted by
//! [`Aggregate`](crate::pipeline::Aggregate), or pushed directly (e.g. by a
//! [`KeyedStream`](crate::stream::KeyedStream)):
//! - [`Table`] — last-writer-wins register per key: point-read the newest
//!   value; the natural sink for [`Aggregate`](crate::pipeline::Aggregate)
//!   changelogs
//! - [`Multimap`] — forward index: all values posted under a key
//! - [`InvertedIndex`] — reverse index: all keys posted under a value
//! - [`search::Bm25`] — full-text search over `Keyed { key: document, val:
//!   text }`, tokenized on ingest and query, ranked by BM25 relevance
//! - [`search::Hnsw`] — approximate nearest-neighbor search over `Keyed {
//!   key: document, val: embedding }`, backed by a retractable HNSW graph
//!
//! # Sinks over [`Scored`]`<S, V>` data
//! Produced by [`ScoreBy`](crate::pipeline::ScoreBy); scores are ordered
//! via the [`Score`](crate::pipeline::Score) encoding:
//! - [`Ranked`] — score-ordered multiset: min/max, top/bottom-n, and score
//!   range scans, with the cutoff chosen at read time
//! - [`Histogram`] — bucketed score distribution: per-bucket counts and
//!   retraction-safe approximate quantiles
//!
//! # Sinks over [`Keyed`]`<K, `[`Scored`]`<S, V>>` data
//! - [`KeyedRanked`] — [`Ranked`] grouped by key: per-key min/max, top-n,
//!   and range scans as prefix reads
//!
//! # Retraction
//! All sinks honor retraction — pushing a datum with delta `-n` undoes `n`
//! prior insertions — but their bookkeeping differs:
//! - *Counting* sinks ([`Count`], [`Bag`], [`Stats`], [`Histogram`],
//!   [`Ranked`], [`KeyedRanked`]) accumulate signed multiplicities, so
//!   deltas cancel exactly at any magnitude.
//! - *Posting* sinks ([`InvertedIndex`], [`Multimap`], [`search::Bm25`],
//!   [`search::Hnsw`]) are set-semantic per record: a transaction's
//!   net-positive delta inserts, net-negative deletes, regardless of
//!   magnitude — no prior state is read, keeping mass retraction cheap.
//! - [`Table`] is last-writer-wins: the final push to a key within a
//!   transaction decides its value (positive delta) or removal
//!   (non-positive).
//!
//! [`Scored`]: crate::pipeline::Scored

use std::{cell::RefCell, marker::PhantomData};

use fjall::Readable;
use fxhash::FxHashMap;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    pipeline::{Keyed, Push},
    stream::{PipelineInitCtx, WriteTx},
};

pub mod search;

mod table;
pub use table::*;

mod histogram;
pub use histogram::*;

mod ranked;
pub use ranked::*;

mod stats;
pub use stats::*;

/// Persistent running sum of all deltas: the number of live records.
///
/// Accepts any data type and ignores the data itself. Deltas accumulate in
/// memory and hit the store once per commit.
pub struct Count {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    pending: i64,
}

impl Count {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Count {
            name: name.into(),
            ks: None,
            pending: 0,
        }
    }
}

/// Read handle for [`Count`], pinned to one snapshot.
pub struct CountReader<'tx, R: Readable> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
}

impl<R: Readable> CountReader<'_, R> {
    /// The current count (0 if nothing was ever inserted).
    pub fn get(&self) -> i64 {
        self.tx
            .get(&self.ks, b"\0")
            .unwrap()
            .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
            .unwrap_or(0)
    }
}

impl<D: Clone> Push<D> for Count {
    type Reader<'tx, R: Readable + 'tx> = CountReader<'tx, R>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    #[inline]
    fn push(&mut self, _tx: &mut WriteTx<'_>, _data: &D, delta: isize) {
        // no I/O per push: accumulate, fold once in commit()
        self.pending += delta as i64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if self.pending == 0 {
            return;
        }
        let ks = self.ks.as_ref().unwrap();
        let cur = tx
            .get(ks, b"\0")
            .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
            .unwrap_or(0);
        tx.insert(ks, b"\0", (cur + self.pending).to_be_bytes());
        self.pending = 0;
    }

    fn abort(&mut self) {
        self.pending = 0;
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        CountReader {
            tx,
            ks: self.ks.clone().unwrap(),
        }
    }
}

/// Persistent counted multiset: each distinct element maps to its
/// multiplicity.
///
/// Elements are stored by their `postcard` encoding; the multiplicity is the
/// running sum of that element's deltas, and elements whose multiplicity
/// reaches 0 are removed. Deltas accumulate in memory so hot elements hit
/// the store once per commit.
pub struct Bag<D> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    // key-encoded data -> accumulated delta this tx
    pending: FxHashMap<Vec<u8>, i64>,
    _p: PhantomData<D>,
}

impl<D> Bag<D> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Bag {
            name: name.into(),
            ks: None,
            pending: FxHashMap::default(),
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
    /// Iterate all `(element, multiplicity)` pairs, ordered by the element's
    /// `postcard` encoding. Multiplicities are always positive.
    pub fn iter(&self) -> impl Iterator<Item = (D, i64)> + '_ {
        self.tx.iter(&self.ks).map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            let d: D = postcard::from_bytes(&key).unwrap();
            let n = i64::from_be_bytes(*val.as_array::<8>().unwrap());
            (d, n)
        })
    }

    /// Whether `d` has multiplicity > 0.
    pub fn contains(&self, d: &D) -> bool
    where
        D: Serialize,
    {
        thread_local! {
            static KEY_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        }
        KEY_BUF.with_borrow_mut(|buf| {
            buf.clear();
            postcard::to_io(d, &mut *buf).unwrap();
            self.tx.contains_key(&self.ks, &buf[..]).unwrap()
        })
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
        // hot keys within one tx collapse here instead of hitting fjall N times
        *self.pending.entry(tx.buf.clone()).or_insert(0) += delta as i64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        let ks = self.ks.clone().unwrap();
        for (key, delta) in self.pending.drain() {
            if delta == 0 {
                continue;
            }
            let cur = tx
                .get(&ks, &key)
                .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
                .unwrap_or(0);
            let new = cur + delta;
            if new > 0 {
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

/// Persistent inverted index over [`Keyed`]`<K, V>` postings: look up all
/// keys `K` posted under a value `V`.
///
/// Accepts `Keyed { key: document_key, val: term }` postings — typically
/// produced by a [`FlatMap`](crate::pipeline::FlatMap) that tokenizes
/// documents — and supports exact-match lookup of every document key posted
/// under a term via [`InvertedIndexReader::search`].
///
/// Postings are set-semantic per `(K, V)` pair: a positive delta inserts the
/// posting, a non-positive delta deletes it, regardless of magnitude or
/// prior multiplicity.
pub struct InvertedIndex<K, V> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    _p: PhantomData<(K, V)>,
}

impl<K, V> InvertedIndex<K, V> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        InvertedIndex {
            name: name.into(),
            ks: None,
            _p: PhantomData,
        }
    }

    // TODO: use len-prefixed keys instead of sep
    fn sep() -> [u8; 4] {
        [255, 255, 255, 255]
    }
}

/// Read handle for [`InvertedIndex`], pinned to one snapshot.
pub struct InvertedIndexReader<'tx, R: Readable, K, V> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    _p: PhantomData<(K, V)>,
}

impl<'tx, R: Readable, K: DeserializeOwned, V: Serialize> InvertedIndexReader<'tx, R, K, V> {
    /// All keys posted under exactly `q` (empty if none).
    pub fn search(&self, q: &V) -> Vec<K> {
        thread_local! {
            static KEY_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        }
        KEY_BUF.with_borrow_mut(|buf| {
            buf.clear();
            postcard::to_io(q, &mut *buf).unwrap();
            buf.extend_from_slice(&InvertedIndex::<K, V>::sep());
            let plen = buf.len();
            self.tx
                .prefix(&self.ks, &buf[..])
                .map(|kv| postcard::from_bytes(&kv.key().unwrap()[plen..]).unwrap())
                .collect()
        })
    }
}

impl<K, V> Push<Keyed<K, V>> for InvertedIndex<K, V>
where
    K: Clone + Serialize + DeserializeOwned,
    V: Clone + Serialize + DeserializeOwned,
{
    type Reader<'tx, R: Readable + 'tx> = InvertedIndexReader<'tx, R, K, V>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    // layout: `postcard(V) sep postcard(K)`
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, V>, delta: isize) {
        let Keyed { key, val } = data;
        let ks = self.ks.clone().unwrap();

        tx.buf.clear();
        postcard::to_io(val, &mut tx.buf).unwrap();
        tx.buf.extend_from_slice(&Self::sep());
        postcard::to_io(key, &mut tx.buf).unwrap();

        let k = std::mem::take(&mut tx.buf);
        if delta.is_positive() {
            tx.insert(&ks, &k, []);
        } else {
            tx.remove(&ks, &k);
        }
        tx.buf = k;
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        InvertedIndexReader {
            tx,
            ks: self.ks.clone().unwrap(),
            _p: PhantomData,
        }
    }
}

/// Persistent forward index over [`Keyed`]`<K, V>` postings: look up all
/// values `V` posted under a key `K` — [`InvertedIndex`] with the roles
/// swapped.
///
/// Postings are set-semantic per `(K, V)` pair: a positive delta inserts the
/// posting, a non-positive delta deletes it, regardless of magnitude or
/// prior multiplicity.
pub struct Multimap<K, V> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    _p: PhantomData<(K, V)>,
}

impl<K, V> Multimap<K, V> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Multimap {
            name: name.into(),
            ks: None,
            _p: PhantomData,
        }
    }
}

/// Read handle for [`Multimap`], pinned to one snapshot.
pub struct MultimapReader<'tx, R: Readable, K, V> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    _p: PhantomData<(K, V)>,
}

impl<'tx, R: Readable, K: Serialize, V: DeserializeOwned> MultimapReader<'tx, R, K, V> {
    /// All values posted under `key` (empty if none), ordered by the
    /// value's `postcard` encoding.
    pub fn get(&self, key: &K) -> Vec<V> {
        thread_local! {
            static KEY_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        }
        KEY_BUF.with_borrow_mut(|buf| {
            buf.clear();
            postcard::to_io(key, &mut *buf).unwrap();
            let plen = buf.len();
            self.tx
                .prefix(&self.ks, &buf[..])
                .map(|kv| postcard::from_bytes(&kv.key().unwrap()[plen..]).unwrap())
                .collect()
        })
    }
}

impl<K, V> Push<Keyed<K, V>> for Multimap<K, V>
where
    K: Clone + Serialize,
    V: Clone + Serialize + DeserializeOwned,
{
    type Reader<'tx, R: Readable + 'tx> = MultimapReader<'tx, R, K, V>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    // layout: `postcard(K) postcard(V)` — no separator: postcard encodings
    // of one type are prefix-free, so a key's postings are exactly the keys
    // extending its encoding
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, V>, delta: isize) {
        let Keyed { key, val } = data;
        let ks = self.ks.clone().unwrap();

        tx.buf.clear();
        postcard::to_io(key, &mut tx.buf).unwrap();
        postcard::to_io(val, &mut tx.buf).unwrap();

        let k = std::mem::take(&mut tx.buf);
        if delta.is_positive() {
            tx.insert(&ks, &k, []);
        } else {
            tx.remove(&ks, &k);
        }
        tx.buf = k;
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        MultimapReader {
            tx,
            ks: self.ks.clone().unwrap(),
            _p: PhantomData,
        }
    }
}
