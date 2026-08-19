use std::marker::PhantomData;

use fjall::Readable;
use fxhash::FxHashMap;

use crate::{
    pipeline::{Push, Score, Scored},
    stream::{PipelineInitCtx, WriteTx},
};

// keyspace layout, discriminated by a leading tag byte:
//   [TOTAL]             -> record count i64
//   [BUCKET] T::encode  -> per-bucket count i64
const TOTAL: u8 = 0;
const BUCKET: u8 = 1;

/// Persistent bucketed distribution of [`Scored`]`<S, V>` scores: counts
/// per bucket, plus retraction-safe approximate quantiles.
///
/// `bucket` maps each score to a bucket of type `T:` [`Score`], whose
/// order-preserving encoding keeps buckets sorted in the store — so
/// [`quantile`](HistogramReader::quantile) is a single cumulative scan.
/// Unlike mergeable sketches (t-digest, DDSketch), plain counters undo
/// exactly, so retractions keep the distribution correct. Accuracy is
/// bounded by bucket width; pick the bucketing to match. Scoring by
/// timestamp and bucketing to the hour makes this a downsampled event-rate
/// series.
///
/// ```no_run
/// use fold::pipeline::{ScoreBy, terminal};
/// use fold::stream::Stream;
///
/// // latency distribution in 10ms buckets
/// let mut st = Stream::new(
///     "lat.db",
///     ScoreBy::new(
///         |ms: &u64| *ms,
///         terminal::Histogram::new("hist", |ms: &u64| ms / 10),
///     ),
/// );
/// st.wtx(|tx| tx.insert(&37));
/// st.rtx(|hist| assert_eq!(hist.quantile(0.5), Some(3)));
/// ```
pub struct Histogram<S, V, T, B> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    bucket: B,
    // encoded bucket -> accumulated delta this tx
    pending: FxHashMap<Vec<u8>, i64>,
    total: i64,
    _p: PhantomData<(S, V, T)>,
}

impl<S, V, T, B> Histogram<S, V, T, B> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline. `bucket` must be deterministic so
    /// retractions land in the bucket they incremented.
    pub fn new(name: impl Into<String>, bucket: B) -> Self {
        Histogram {
            name: name.into(),
            ks: None,
            bucket,
            pending: FxHashMap::default(),
            total: 0,
            _p: PhantomData,
        }
    }
}

impl<S, V, T, B> Push<Scored<S, V>> for Histogram<S, V, T, B>
where
    S: Clone,
    V: Clone,
    T: Score,
    B: Fn(&S) -> T,
{
    type Reader<'tx, R: Readable + 'tx> = HistogramReader<'tx, R, T>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Scored<S, V>, delta: isize) {
        tx.buf.clear();
        tx.buf.push(BUCKET);
        (self.bucket)(&data.score).encode(&mut tx.buf);
        *self.pending.entry(tx.buf.clone()).or_insert(0) += delta as i64;
        self.total += delta as i64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if self.pending.is_empty() {
            return;
        }
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
            debug_assert!(new >= 0, "histogram count went negative");
            if new > 0 {
                tx.insert(&ks, &key, new.to_be_bytes());
            } else {
                tx.remove(&ks, &key);
            }
        }
        let total = tx
            .get(&ks, [TOTAL])
            .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
            .unwrap_or(0)
            + self.total;
        if total > 0 {
            tx.insert(&ks, [TOTAL], total.to_be_bytes());
        } else {
            tx.remove(&ks, [TOTAL]);
        }
        self.total = 0;
    }

    fn abort(&mut self) {
        self.pending.clear();
        self.total = 0;
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        HistogramReader {
            tx,
            ks: self.ks.clone().unwrap(),
            name: self.name.clone(),
            _p: PhantomData,
        }
    }
}

/// Read handle for [`Histogram`], pinned to one snapshot.
pub struct HistogramReader<'tx, R: Readable, T> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    name: String,
    _p: PhantomData<T>,
}

impl<'tx, R: Readable, T> HistogramReader<'tx, R, T> {
    /// The sink name this reader serves, as given to [`Histogram::new`].
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<'tx, R: Readable, T: Score> HistogramReader<'tx, R, T> {
    /// The number of live records across all buckets.
    pub fn total(&self) -> i64 {
        self.tx
            .get(&self.ks, [TOTAL])
            .unwrap()
            .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
            .unwrap_or(0)
    }

    /// The number of records in `bucket` (0 if empty).
    pub fn count(&self, bucket: &T) -> i64 {
        let mut key = vec![BUCKET];
        bucket.encode(&mut key);
        self.tx
            .get(&self.ks, &key)
            .unwrap()
            .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
            .unwrap_or(0)
    }

    /// Iterate all nonempty `(bucket, count)` pairs in ascending bucket
    /// order; `.rev()` scans descending.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (T, i64)> + '_ {
        self.tx.prefix(&self.ks, [BUCKET]).map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            (
                T::decode(&key[1..]).0,
                i64::from_be_bytes(val.as_ref().try_into().unwrap()),
            )
        })
    }

    /// The bucket containing the `q`-quantile record (`q` in `[0, 1]`),
    /// or `None` if the histogram is empty. Accuracy is one bucket width.
    pub fn quantile(&self, q: f64) -> Option<T> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let target = ((q.clamp(0.0, 1.0) * total as f64).ceil() as i64).max(1);
        let mut cum = 0;
        for (bucket, count) in self.iter() {
            cum += count;
            if cum >= target {
                return Some(bucket);
            }
        }
        None
    }
}
