//! Sentence-length percentile vs the canon — one more fold sink exercised.
//!
//! A `Histogram` sink over word counts lives in its own tiny stream at
//! `data/canon_len.db`. Built once from **all** of `data/canon.jsonl`
//! (the full 129,097-sentence canon, regardless of `SOUNDINGS_CANON_STEP` —
//! the population a writer is compared against should be the whole canon,
//! not the sampled index), it reopens from disk in milliseconds. The
//! distribution is read out once into a `LenCdf` that the doc thread
//! consults per row.

use std::sync::Arc;

use fold::pipeline::{ScoreBy, terminal};
use fold::stream::Stream;
use tokio::sync::watch;

/// Cumulative distribution of canon sentence lengths (in words), read from
/// the Histogram sink once. `buckets` is ascending by word count.
#[derive(Debug, Clone, Default)]
pub struct LenCdf {
    /// (words, records strictly shorter, records exactly this long)
    pub buckets: Vec<(u32, i64, i64)>,
    pub total: i64,
}

impl LenCdf {
    /// Percentile (0–100) of a sentence of `words` words among the canon:
    /// records shorter plus half the records of equal length, over total.
    /// When no bucket has exactly `words`, the nearest lower bucket's
    /// cumulative count is used (everything shorter).
    pub fn percentile(&self, words: u32) -> f32 {
        if self.total <= 0 {
            return 0.0;
        }
        // last bucket with word count <= words
        let idx = self.buckets.partition_point(|b| b.0 <= words);
        let below = if idx == 0 {
            0.0
        } else {
            let (w, cum_below, count) = self.buckets[idx - 1];
            if w == words { cum_below as f64 + count as f64 / 2.0 } else { (cum_below + count) as f64 }
        };
        (below / self.total as f64 * 100.0) as f32
    }
}

/// Open the length histogram, building it from canon.jsonl if empty, and
/// publish the CDF on `out` (None if there is no canon to build from).
pub fn open_or_build(status: &watch::Sender<String>, out: &watch::Sender<Option<Arc<LenCdf>>>) {
    let mut st = Stream::new(
        std::path::Path::new("data/canon_len.db"),
        ScoreBy::new(|w: &u32| *w, terminal::Histogram::new("len", |w: &u32| *w)),
    );
    let total = st.rtx(|hist| hist.total());
    if total == 0 {
        let Ok(raw) = std::fs::read_to_string("data/canon.jsonl") else {
            let _ = out.send(None);
            return;
        };
        let words: Vec<u32> = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<crate::CanonRow>(l).ok())
            .map(|r| r.text.split_whitespace().count() as u32)
            .collect();
        let _ = status.send(format!("building length histogram · {} sentences…", words.len()));
        for chunk in words.chunks(8192) {
            st.wtx(|tx| {
                for w in chunk {
                    tx.insert(w);
                }
            });
        }
    }
    let cdf = st.rtx(|hist| {
        let total = hist.total();
        let mut cum = 0i64;
        let buckets = hist
            .iter()
            .map(|(w, count)| {
                let b = (w, cum, count);
                cum += count;
                b
            })
            .collect();
        LenCdf { buckets, total }
    });
    let _ = status.send(format!("length histogram · {} sentences (Histogram sink, {})", cdf.total, if total == 0 { "built" } else { "from disk" }));
    let _ = out.send(Some(Arc::new(cdf)));
}

#[cfg(test)]
mod tests {
    use super::LenCdf;

    fn cdf() -> LenCdf {
        // 4 sentences of 5 words, 4 of 10, 2 of 20 — total 10
        LenCdf { buckets: vec![(5, 0, 4), (10, 4, 4), (20, 8, 2)], total: 10 }
    }

    #[test]
    fn percentile_is_below_plus_half_of_equal() {
        let c = cdf();
        assert!((c.percentile(5) - 20.0).abs() < 1e-4); // 0 + 4/2 = 2 of 10
        assert!((c.percentile(10) - 60.0).abs() < 1e-4); // 4 + 4/2 = 6 of 10
        assert!((c.percentile(20) - 90.0).abs() < 1e-4); // 8 + 2/2 = 9 of 10
    }

    #[test]
    fn percentile_between_buckets_uses_everything_shorter() {
        let c = cdf();
        assert!((c.percentile(7) - 40.0).abs() < 1e-4); // all 4 five-word rows
        assert!((c.percentile(1) - 0.0).abs() < 1e-4);
        assert!((c.percentile(99) - 100.0).abs() < 1e-4);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(LenCdf::default().percentile(12), 0.0);
    }
}
