//! The watermark: sod's only notion of "now" (SOD-7).
//!
//! The watermark is the maximum event-time across every frame applied so
//! far. Max is commutative and associative over the replicated frame set,
//! so the watermark converges exactly as the data does — it is time *as
//! data*, never a wall clock.
//!
//! Note the deliberate limitation recorded in the spec: fold's `Retain`
//! (a processing-time window) stamps records at arrival, and arrival order
//! differs per replica, so no injected clock — this one included — makes
//! processing-time windows convergent. Time-windowed operators need an
//! event-time retain in fold before they are sod-compatible; until then
//! the watermark serves application reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A shared, monotonically-advancing watermark handle.
#[derive(Clone, Default)]
pub struct Watermark(Arc<AtomicU64>);

impl Watermark {
    pub fn new() -> Self {
        Self::default()
    }

    /// Max event-time applied so far.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Advance to at least `wm` (monotonic max).
    pub fn advance(&self, wm: u64) {
        self.0.fetch_max(wm, Ordering::AcqRel);
    }

    /// A `Fn() -> u64` closure over this watermark, in the shape fold's
    /// clock-taking operators accept.
    pub fn clock(&self) -> impl Fn() -> u64 + Clone + 'static {
        let inner = self.0.clone();
        move || inner.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watermark_is_monotonic_max() {
        let w = Watermark::new();
        let c = w.clock();
        w.advance(10);
        w.advance(5);
        assert_eq!(w.get(), 10);
        assert_eq!(c(), 10);
        w.advance(20);
        assert_eq!(c(), 20);
    }
}
