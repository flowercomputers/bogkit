//! The [`Engine`] port — "the bog machinery" that materializes deltas —
//! and [`MemEngine`], the always-compiled deterministic multiset engine.
//!
//! An engine is whatever turns applied frames into readable state. The
//! contract that makes crash healing (SOD-5) work: `apply` must commit the
//! frame's deltas **and** the cursor advance for `(origin, seq)` atomically,
//! and `applied` must report that cursor at open, so the replica can replay
//! exactly the log suffix the engine has not yet seen.
//!
//! [`MemEngine`] is three things at once: the property-test oracle, the
//! differential reference for richer engines, and the engine available on
//! targets fold cannot reach yet (browsers). `FoldEngine`
//! (feature `fold-engine`) is the full-powered native engine.

use std::collections::BTreeMap;

use crate::vector::VersionVector;
use crate::{Frame, SodError};

/// The bog machinery port.
pub trait Engine {
    /// Deterministically check that a frame is applicable — e.g. that its
    /// datums decode as the pipeline type — WITHOUT mutating anything.
    ///
    /// The replica calls this **before** appending a frame to the log:
    /// once a frame is logged it will be replayed on every open, so a
    /// frame that deterministically fails `apply` would brick the replica.
    /// `validate`-then-`apply` must agree: any frame that passes validate
    /// must not fail apply for a deterministic reason.
    fn validate(&self, _frame: &Frame) -> Result<(), SodError> {
        Ok(())
    }

    /// Apply one frame's deltas plus the new watermark. The deltas and the
    /// applied-cursor advance to `(frame.origin, frame.seq)` must commit
    /// atomically.
    fn apply(&mut self, frame: &Frame, watermark: u64) -> Result<(), SodError>;

    /// The cursor durably applied through, per origin. Read at open to
    /// replay exactly the un-applied log suffix.
    fn applied(&self) -> VersionVector;

    /// Seed the watermark at open, before any `apply` (default: no-op).
    fn seed_watermark(&mut self, _wm: u64) {}
}

/// Deterministic in-memory multiset engine.
///
/// State is exactly the Z-set: `datum bytes → net multiplicity`, with zero
/// entries removed (so a transient negative — a retraction arriving before
/// its insert — is representable and visible, per the spec).
#[derive(Default, Debug)]
pub struct MemEngine {
    multiset: BTreeMap<Vec<u8>, i64>,
    applied: VersionVector,
    watermark: u64,
}

impl MemEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Canonical bytes of the whole multiset — the convergence probe used
    /// by SOD-4 tests (`BTreeMap`, so iteration order is key order).
    pub fn view_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(&self.multiset).expect("multiset encoding is infallible")
    }

    /// Net multiplicity of one datum (0 if absent).
    pub fn count(&self, datum: &[u8]) -> i64 {
        self.multiset.get(datum).copied().unwrap_or(0)
    }

    /// Iterate `(datum, net multiplicity)` in datum order.
    pub fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &i64)> {
        self.multiset.iter()
    }

    pub fn watermark(&self) -> u64 {
        self.watermark
    }
}

impl Engine for MemEngine {
    fn apply(&mut self, frame: &Frame, watermark: u64) -> Result<(), SodError> {
        for (datum, mult) in &frame.payload {
            let entry = self.multiset.entry(datum.clone()).or_insert(0);
            *entry += mult;
            if *entry == 0 {
                self.multiset.remove(datum);
            }
        }
        self.applied.set(frame.origin, frame.seq);
        self.watermark = self.watermark.max(watermark);
        Ok(())
    }

    fn applied(&self) -> VersionVector {
        self.applied.clone()
    }

    fn seed_watermark(&mut self, wm: u64) {
        self.watermark = self.watermark.max(wm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ReplicaId, ZERO_HASH};

    fn frame(origin: u8, seq: u64, payload: Vec<(Vec<u8>, i64)>) -> Frame {
        Frame {
            prev_hash: ZERO_HASH, // engines don't verify chains; the replica does
            origin: ReplicaId([origin; 16]),
            seq,
            event_time: seq,
            payload,
        }
    }

    #[test]
    fn apply_advances_cursor() {
        let mut e = MemEngine::new();
        e.apply(&frame(1, 1, vec![(b"a".to_vec(), 1)]), 10).unwrap();
        e.apply(&frame(1, 2, vec![(b"a".to_vec(), 2)]), 20).unwrap();
        e.apply(&frame(2, 1, vec![(b"b".to_vec(), 1)]), 15).unwrap();
        assert_eq!(e.applied().get(&ReplicaId([1; 16])), 2);
        assert_eq!(e.applied().get(&ReplicaId([2; 16])), 1);
        assert_eq!(e.count(b"a"), 3);
        assert_eq!(e.watermark(), 20);
    }

    #[test]
    fn retraction_before_insert_goes_negative_then_zero_entry_removed() {
        let mut e = MemEngine::new();
        e.apply(&frame(1, 1, vec![(b"x".to_vec(), -1)]), 1).unwrap();
        assert_eq!(e.count(b"x"), -1);
        e.apply(&frame(2, 1, vec![(b"x".to_vec(), 1)]), 2).unwrap();
        assert_eq!(e.count(b"x"), 0);
        assert!(e.iter().next().is_none(), "zero entries are removed");
    }

    #[test]
    fn view_bytes_order_independent() {
        let fa = frame(1, 1, vec![(b"a".to_vec(), 2), (b"b".to_vec(), -1)]);
        let fb = frame(2, 1, vec![(b"c".to_vec(), 5)]);
        let fc = frame(3, 1, vec![(b"a".to_vec(), -2)]);

        let mut e1 = MemEngine::new();
        for f in [&fa, &fb, &fc] {
            e1.apply(f, f.event_time).unwrap();
        }
        let mut e2 = MemEngine::new();
        for f in [&fc, &fa, &fb] {
            e2.apply(f, f.event_time).unwrap();
        }
        assert_eq!(e1.view_bytes(), e2.view_bytes());
        assert_eq!(e1.watermark(), e2.watermark());
    }
}
