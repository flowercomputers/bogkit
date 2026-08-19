use std::marker::PhantomData;

use fjall::Readable;

use crate::{
    pipeline::Push,
    stream::{PipelineInitCtx, WriteTx},
};

/// Persistent running moments of a numeric projection of each record:
/// count, sum, mean, variance.
///
/// `value` extracts an `f64` from each datum; the sink maintains the count,
/// sum, and sum of squares, which subtract exactly under retraction (up to
/// float rounding — long-lived, high-churn streams accumulate error where a
/// recompute would not). Variance derives as `E[x²] − E[x]²`, which can
/// cancel catastrophically when the mean dwarfs the spread; center the
/// projection if that matters.
///
/// Deltas accumulate in memory and hit the store once per commit.
pub struct Stats<D, F> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    value: F,
    // accumulated this tx
    count: i64,
    sum: f64,
    sumsq: f64,
    _p: PhantomData<D>,
}

impl<D, F> Stats<D, F> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline. `value` must be deterministic so
    /// retractions subtract what was added.
    pub fn new(name: impl Into<String>, value: F) -> Self {
        Stats {
            name: name.into(),
            ks: None,
            value,
            count: 0,
            sum: 0.0,
            sumsq: 0.0,
            _p: PhantomData,
        }
    }
}

fn encode(count: i64, sum: f64, sumsq: f64) -> [u8; 24] {
    let mut v = [0u8; 24];
    v[..8].copy_from_slice(&count.to_be_bytes());
    v[8..16].copy_from_slice(&sum.to_be_bytes());
    v[16..].copy_from_slice(&sumsq.to_be_bytes());
    v
}

fn decode(v: &[u8]) -> (i64, f64, f64) {
    (
        i64::from_be_bytes(v[..8].try_into().unwrap()),
        f64::from_be_bytes(v[8..16].try_into().unwrap()),
        f64::from_be_bytes(v[16..].try_into().unwrap()),
    )
}

impl<D: Clone, F: Fn(&D) -> f64> Push<D> for Stats<D, F> {
    type Reader<'tx, R: Readable + 'tx> = StatsReader<'tx, R>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, _tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        let x = (self.value)(data);
        let d = delta as i64;
        self.count += d;
        self.sum += x * d as f64;
        self.sumsq += x * x * d as f64;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if self.count == 0 && self.sum == 0.0 && self.sumsq == 0.0 {
            return;
        }
        let ks = self.ks.clone().unwrap();
        let (count, sum, sumsq) = tx
            .get(&ks, [0])
            .map(|v| decode(&v))
            .unwrap_or((0, 0.0, 0.0));
        let (count, sum, sumsq) = (count + self.count, sum + self.sum, sumsq + self.sumsq);
        if count != 0 || sum != 0.0 || sumsq != 0.0 {
            tx.insert(&ks, [0], encode(count, sum, sumsq));
        } else {
            tx.remove(&ks, [0]);
        }
        (self.count, self.sum, self.sumsq) = (0, 0.0, 0.0);
    }

    fn abort(&mut self) {
        (self.count, self.sum, self.sumsq) = (0, 0.0, 0.0);
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        StatsReader {
            tx,
            ks: self.ks.clone().unwrap(),
            name: self.name.clone(),
        }
    }
}

/// Read handle for [`Stats`], pinned to one snapshot.
pub struct StatsReader<'tx, R: Readable> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    name: String,
}

impl<'tx, R: Readable> StatsReader<'tx, R> {
    /// The sink name this reader serves, as given to [`Stats::new`].
    pub fn name(&self) -> &str {
        &self.name
    }

    fn get(&self) -> (i64, f64, f64) {
        self.tx
            .get(&self.ks, [0])
            .unwrap()
            .map(|v| decode(&v))
            .unwrap_or((0, 0.0, 0.0))
    }

    /// The number of live records.
    pub fn count(&self) -> i64 {
        self.get().0
    }

    /// The sum of all live projections (0 when empty).
    pub fn sum(&self) -> f64 {
        self.get().1
    }

    /// The mean projection, or `None` when empty.
    pub fn mean(&self) -> Option<f64> {
        let (count, sum, _) = self.get();
        (count > 0).then(|| sum / count as f64)
    }

    /// The population variance, or `None` when empty.
    pub fn variance(&self) -> Option<f64> {
        let (count, sum, sumsq) = self.get();
        (count > 0).then(|| {
            let mean = sum / count as f64;
            (sumsq / count as f64 - mean * mean).max(0.0)
        })
    }

    /// The population standard deviation, or `None` when empty.
    pub fn stddev(&self) -> Option<f64> {
        self.variance().map(f64::sqrt)
    }
}
