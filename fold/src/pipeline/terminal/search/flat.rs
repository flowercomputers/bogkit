//! Exact-scan search sink: the vector lane without an index.
//!
//! [`Hnsw`](super::Hnsw) keeps its graph in memory and rebuilds it from the
//! persisted rows on first use in every process — O(n log n) before any
//! O(1) work, which for a short-lived CLI is the whole budget: measured at
//! 8.9 s for 7,369 vectors, linear in n. Below a few hundred thousand
//! vectors an exact scan is the right structure: 0.1 ms of arithmetic at
//! 7k, 1 ms at 50k, and its only real cost is reading the rows.
//!
//! This sink stores rows **byte-identical to `Hnsw`'s** (`postcard(K)` →
//! `postcard(&[T])`) so a pipeline can swap one for the other over the same
//! keyspace with no rebuild. It has no in-memory state at all: a write is one
//! row, a retraction is one delete, and a search is a scan of a pinned
//! snapshot — exact, order-independent, and therefore deterministic under
//! replay in a way an approximate index is not.

use std::marker::PhantomData;

use anny::metric::{Metric, Scalar};
use fjall::Readable;
use fxhash::FxHashMap;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    pipeline::{Keyed, Push, Scored},
    stream::{PipelineInitCtx, WriteTx},
};

fn decode_vector<T: DeserializeOwned + Copy, const DIM: usize>(bytes: &[u8]) -> [T; DIM] {
    let v: Vec<T> = postcard::from_bytes(bytes).unwrap();
    std::array::from_fn(|i| v[i])
}

/// Exact nearest-neighbour search over [`Keyed`]`<K, [T; DIM]>` embeddings
/// by scanning every persisted row. Same reader surface as
/// [`HnswReader`](super::HnswReader): [`FlatReader::search`] returns the
/// `TOP_K` nearest keys ascending by distance under `M` (smaller is closer),
/// ties broken by the encoded key so results are stable across runs.
///
/// Documents are set-semantic per key with the same per-transaction
/// resolution `Hnsw` uses: the net delta decides, and a replacement
/// (retract old, insert new in one transaction) reaches the store.
///
/// ```ignore
/// let mut st = Stream::new("vecs.db", Flat::<u32, f32, L2, 4>::new("vecs", L2));
/// st.wtx(|tx| tx.insert(&Keyed::new(7, [0.1, 0.2, 0.3, 0.4])));
/// st.rtx(|idx| for hit in idx.search(&[0.1, 0.2, 0.3, 0.4]) { println!("{}: {}", hit.val, hit.score) });
/// ```
pub struct Flat<K, T, M: Metric<T>, const DIM: usize, const TOP_K: usize = 10> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    metric: M,
    // per-key resolution of this transaction's pushes, in push order:
    // (net delta, whether the LAST push was positive, latest positively-
    // pushed embedding). Nothing depends on cross-key drain order.
    pending: FxHashMap<Vec<u8>, (i64, bool, Option<[T; DIM]>)>,
    vec_buf: Vec<u8>,
    _key: PhantomData<K>,
}

impl<K, T, M: Metric<T>, const DIM: usize, const TOP_K: usize> Flat<K, T, M, DIM, TOP_K> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline. Naming it the same as a retired `Hnsw`
    /// sink adopts that sink's rows as they are.
    pub fn new(name: impl Into<String>, metric: M) -> Self {
        Flat {
            name: name.into(),
            ks: None,
            metric,
            pending: FxHashMap::default(),
            vec_buf: Vec::new(),
            _key: PhantomData,
        }
    }
}

impl<K, T, M, const DIM: usize, const TOP_K: usize> Push<Keyed<K, [T; DIM]>>
    for Flat<K, T, M, DIM, TOP_K>
where
    K: Clone + Serialize + DeserializeOwned,
    T: Scalar + Serialize + DeserializeOwned,
    M: Metric<T> + Copy,
    M::Out: PartialOrd + Copy,
{
    type Reader<'tx, R: Readable + 'tx> = FlatReader<'tx, R, K, T, M, DIM, TOP_K>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, [T; DIM]>, delta: isize) {
        tx.buf.clear();
        postcard::to_io(&data.key, &mut tx.buf).unwrap();
        let e = self
            .pending
            .entry(tx.buf.clone())
            .or_insert((0, false, None));
        e.0 += delta as i64;
        e.1 = delta > 0;
        if delta > 0 {
            e.2 = Some(data.val);
        }
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if self.pending.is_empty() {
            return;
        }
        let ks = self.ks.clone().unwrap();
        for (kenc, (net, last_was_positive, last_pos)) in self.pending.drain() {
            // net > 0, or net == 0 with a positive last push (replacement):
            // the latest embedding wins. net < 0: delete by key regardless
            // of the value the caller reproduced — embeddings may be
            // recomputed, and a byte mismatch must not make a row
            // undeletable. Insert + retract of the same key cancels.
            if net > 0 || (net == 0 && last_was_positive) {
                let vec = last_pos.expect("positive push recorded an embedding");
                self.vec_buf.clear();
                postcard::to_io(&vec[..], &mut self.vec_buf).unwrap();
                tx.insert(&ks, &kenc, &self.vec_buf);
            } else if net < 0 {
                tx.remove(&ks, &kenc);
            }
        }
    }

    fn abort(&mut self) {
        // nothing was written and nothing lives in memory: forget the tx
        self.pending.clear();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        FlatReader {
            tx,
            ks: self.ks.clone().unwrap(),
            metric: self.metric,
            _key: PhantomData,
        }
    }
}

/// Read handle for [`Flat`], pinned to one snapshot.
pub struct FlatReader<'tx, R: Readable, K, T, M: Metric<T>, const DIM: usize, const TOP_K: usize> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    #[allow(dead_code)]
    metric: M,
    _key: PhantomData<(K, T)>,
}

impl<'tx, R, K, T, M, const DIM: usize, const TOP_K: usize> FlatReader<'tx, R, K, T, M, DIM, TOP_K>
where
    R: Readable,
    K: DeserializeOwned,
    T: Scalar + DeserializeOwned,
    M: Metric<T>,
    M::Out: PartialOrd + Copy,
{
    /// The `TOP_K` nearest keys to `q`, ascending by distance. Exact: every
    /// row is scored. Ties are broken by the encoded key, so two readers of
    /// the same snapshot return the same list in the same order.
    pub fn search(&self, q: &[T; DIM]) -> Vec<Scored<M::Out, K>> {
        // bounded top-K kept sorted; TOP_K is small, so insertion beats a heap
        let mut best: Vec<(M::Out, Vec<u8>)> = Vec::with_capacity(TOP_K + 1);
        for kv in self.tx.iter(&self.ks) {
            let (kenc, venc) = kv.into_inner().unwrap();
            let v = decode_vector::<T, DIM>(&venc);
            let d = M::distance(&q[..], &v[..]);
            let full = best.len() == TOP_K;
            if full {
                let (wd, wk) = &best[TOP_K - 1];
                let worse_than_last = d > *wd || (d == *wd && kenc.as_ref() >= wk.as_slice());
                if worse_than_last {
                    continue;
                }
            }
            let pos = best
                .iter()
                .position(|(bd, bk)| d < *bd || (d == *bd && kenc.as_ref() < bk.as_slice()))
                .unwrap_or(best.len());
            best.insert(pos, (d, kenc.to_vec()));
            if full {
                best.pop();
            }
        }
        best.into_iter()
            .map(|(d, kenc)| Scored::new(d, postcard::from_bytes(&kenc).unwrap()))
            .collect()
    }

    /// Number of embedded keys. A scan, like everything here.
    pub fn len(&self) -> usize {
        self.tx.iter(&self.ks).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
