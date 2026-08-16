//! Version vectors: per-origin high-water marks of contiguous frames held.
//!
//! `vector[origin] = n` means "this replica holds frames 1..=n of that
//! origin's feed". Because feeds are contiguous chains, a single integer per
//! origin fully describes possession, and the vector doubles as the sync
//! resume point (SOD-6): there is no session state to checkpoint.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::frame::ReplicaId;

/// Per-origin seq held through (contiguously). Absent origin = 0.
#[derive(Default, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct VersionVector(BTreeMap<ReplicaId, u64>);

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    /// The seq held through for `id`; 0 if the origin is unknown.
    pub fn get(&self, id: &ReplicaId) -> u64 {
        self.0.get(id).copied().unwrap_or(0)
    }

    /// Record possession of the next frame in `id`'s feed.
    ///
    /// # Panics
    /// Panics unless `seq == self.get(id) + 1` — feeds are contiguous, and a
    /// non-contiguous advance is a logic error upstream.
    pub fn advance(&mut self, id: ReplicaId, seq: u64) {
        let have = self.get(&id);
        assert_eq!(seq, have + 1, "non-contiguous advance for {id}: have {have}, got {seq}");
        self.0.insert(id, seq);
    }

    /// Set `id`'s entry directly (engine cursors loaded from storage).
    pub fn set(&mut self, id: ReplicaId, seq: u64) {
        if seq == 0 {
            self.0.remove(&id);
        } else {
            self.0.insert(id, seq);
        }
    }

    /// Iterate `(origin, seq-held-through)` in origin order.
    pub fn iter(&self) -> impl Iterator<Item = (&ReplicaId, &u64)> {
        self.0.iter()
    }

    /// The ranges `self` holds beyond `other`: for each origin where
    /// `self > other`, yields `(origin, other_have, self_have)` — the frames
    /// `(other_have, self_have]` are what a peer at `other` is missing.
    pub fn ahead_of(&self, other: &Self) -> Vec<(ReplicaId, u64, u64)> {
        self.0
            .iter()
            .filter_map(|(id, &have)| {
                let theirs = other.get(id);
                (have > theirs).then_some((*id, theirs, have))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(b: u8) -> ReplicaId {
        ReplicaId([b; 16])
    }

    #[test]
    fn advance_contiguous() {
        let mut v = VersionVector::new();
        assert_eq!(v.get(&id(1)), 0);
        v.advance(id(1), 1);
        v.advance(id(1), 2);
        v.advance(id(2), 1);
        assert_eq!(v.get(&id(1)), 2);
        assert_eq!(v.get(&id(2)), 1);
    }

    #[test]
    #[should_panic(expected = "non-contiguous")]
    fn advance_gap_panics() {
        let mut v = VersionVector::new();
        v.advance(id(1), 2);
    }

    #[test]
    fn ahead_of_disjoint_and_overlap() {
        let mut a = VersionVector::new();
        a.set(id(1), 5); // partially known to b
        a.set(id(2), 3); // unknown to b
        a.set(id(3), 2); // b is ahead here

        let mut b = VersionVector::new();
        b.set(id(1), 2);
        b.set(id(3), 4);

        assert_eq!(a.ahead_of(&b), vec![(id(1), 2, 5), (id(2), 0, 3)]);
        assert_eq!(b.ahead_of(&a), vec![(id(3), 2, 4)]);
        assert_eq!(a.ahead_of(&a), vec![]);
    }
}
