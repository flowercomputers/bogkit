//! [`Replica`]: the write path, crash recovery, and feed bookkeeping.
//!
//! A replica owns a [`LogStore`] (the truth, SOD-1) and an [`Engine`] (a
//! rebuildable cache). Local commits and remote ingests both follow the
//! log-first discipline: append (+ sync per policy), then apply — with the
//! engine's own applied-cursor closing the crash window in either direction
//! (log ahead of engine: replay at open; engine ahead of log: the apply
//! guard skips re-application).

use std::collections::{BTreeMap, BTreeSet};

use crate::engine::Engine;
use crate::frame::{Frame, FrameHash, ReplicaId, ZERO_HASH};
use crate::store::LogStore;
use crate::vector::VersionVector;
use crate::SodError;

#[derive(Default)]
struct Feed {
    frames: Vec<Frame>,      // frames[seq-1]
    hashes: Vec<FrameHash>,  // hashes[seq-1]
}

impl Feed {
    fn head_hash(&self) -> FrameHash {
        self.hashes.last().copied().unwrap_or(ZERO_HASH)
    }
}

/// A sod: one replica of the shared multiset.
pub struct Replica<E: Engine, L: LogStore> {
    id: ReplicaId,
    log: L,
    engine: E,
    feeds: BTreeMap<ReplicaId, Feed>,
    vector: VersionVector,
    engine_cursor: VersionVector,
    poisoned: BTreeSet<ReplicaId>,
    watermark: u64,
}

impl<E: Engine, L: LogStore> Replica<E, L> {
    /// Rebuild feeds, vector, and watermark from the log, verifying every
    /// chain, then replay into the engine each frame beyond its applied
    /// cursor (SOD-1, SOD-5).
    pub fn open(id: ReplicaId, log: L, mut engine: E) -> Result<Self, SodError> {
        let mut feeds: BTreeMap<ReplicaId, Feed> = BTreeMap::new();
        let mut vector = VersionVector::new();
        let engine_cursor = engine.applied();
        let mut watermark = 0u64;

        for frame in log.frames() {
            let feed = feeds.entry(frame.origin).or_default();
            let have = feed.frames.len() as u64;
            if frame.seq != have + 1 {
                return Err(SodError::Corrupt("log feed not contiguous"));
            }
            if frame.prev_hash != feed.head_hash() {
                return Err(SodError::Corrupt("log feed chain broken"));
            }
            watermark = watermark.max(frame.event_time);
            if frame.seq > engine_cursor.get(&frame.origin) {
                engine.apply(frame, watermark)?;
            } else {
                engine.seed_watermark(watermark);
            }
            feed.hashes.push(frame.hash());
            feed.frames.push(frame.clone());
            vector.advance(frame.origin, frame.seq);
        }

        let engine_cursor = engine.applied();
        Ok(Replica {
            id,
            log,
            engine,
            feeds,
            vector,
            engine_cursor,
            poisoned: BTreeSet::new(),
            watermark,
        })
    }

    /// Commit a local write: chain a frame onto our own feed, append it to
    /// the log, sync, and apply. `event_time` is stamped by the caller —
    /// sod never reads a clock (SOD-7). Returns the new frame's hash.
    pub fn commit(
        &mut self,
        payload: Vec<(Vec<u8>, i64)>,
        event_time: u64,
    ) -> Result<FrameHash, SodError> {
        if self.poisoned.contains(&self.id) {
            return Err(SodError::Poisoned(self.id));
        }
        let feed = self.feeds.entry(self.id).or_default();
        let frame = Frame {
            prev_hash: feed.head_hash(),
            origin: self.id,
            seq: feed.frames.len() as u64 + 1,
            event_time,
            payload,
        };
        self.log.append(&frame)?;
        self.log.sync()?;
        let hash = frame.hash();
        self.accept(frame, hash)?;
        Ok(hash)
    }

    /// Ingest a frame received from a peer.
    ///
    /// - Already held with the same hash → `Ok(false)` (dedup, SOD-6).
    /// - Already held with a different hash, or a broken chain link →
    ///   the origin's feed is poisoned and further frames refused (SOD-2).
    /// - Seq beyond our head + 1 → [`SodError::Gap`] (peers must stream
    ///   per-origin contiguous suffixes).
    /// - Otherwise: append to the log and apply → `Ok(true)`. Durability
    ///   batching is the caller's business: call
    ///   [`sync_log`](Replica::sync_log) at session boundaries.
    pub fn ingest(&mut self, frame: Frame) -> Result<bool, SodError> {
        let origin = frame.origin;
        if self.poisoned.contains(&origin) {
            return Err(SodError::Poisoned(origin));
        }
        let have = self.vector.get(&origin);
        let hash = frame.hash();
        if frame.seq <= have {
            let known = self.feeds[&origin].hashes[frame.seq as usize - 1];
            if known == hash {
                return Ok(false);
            }
            self.poisoned.insert(origin);
            return Err(SodError::Equivocation { origin, seq: frame.seq });
        }
        if frame.seq > have + 1 {
            return Err(SodError::Gap { origin, have, got: frame.seq });
        }
        let expected_prev = self
            .feeds
            .get(&origin)
            .map(Feed::head_hash)
            .unwrap_or(ZERO_HASH);
        if frame.prev_hash != expected_prev {
            // same (origin, seq) position as a chain we don't hold: a fork
            self.poisoned.insert(origin);
            return Err(SodError::Equivocation { origin, seq: frame.seq });
        }
        self.log.append(&frame)?;
        self.accept(frame, hash)?;
        Ok(true)
    }

    /// Log-accepted frame: update feeds/vector/watermark and guard-apply.
    fn accept(&mut self, frame: Frame, hash: FrameHash) -> Result<(), SodError> {
        self.watermark = self.watermark.max(frame.event_time);
        if frame.seq > self.engine_cursor.get(&frame.origin) {
            self.engine.apply(&frame, self.watermark)?;
            self.engine_cursor.set(frame.origin, frame.seq);
        }
        let feed = self.feeds.entry(frame.origin).or_default();
        feed.hashes.push(hash);
        self.vector.advance(frame.origin, frame.seq);
        feed.frames.push(frame);
        Ok(())
    }

    /// Harden received frames (fsync); called at sync-session boundaries.
    pub fn sync_log(&mut self) -> Result<(), SodError> {
        self.log.sync()
    }

    pub fn id(&self) -> ReplicaId {
        self.id
    }

    pub fn vector(&self) -> &VersionVector {
        &self.vector
    }

    /// Max event-time across every frame held (SOD-7's watermark).
    pub fn watermark(&self) -> u64 {
        self.watermark
    }

    /// The frames of `origin`'s feed with `seq > after`, in seq order.
    pub fn frames_after(&self, origin: &ReplicaId, after: u64) -> &[Frame] {
        self.feeds
            .get(origin)
            .map(|f| &f.frames[after as usize..])
            .unwrap_or(&[])
    }

    /// Origins whose feeds this replica has poisoned (SOD-2).
    pub fn poisoned(&self) -> impl Iterator<Item = &ReplicaId> {
        self.poisoned.iter()
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut E {
        &mut self.engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemEngine;
    use crate::store::MemLog;

    fn id(b: u8) -> ReplicaId {
        ReplicaId([b; 16])
    }

    fn replica(b: u8) -> Replica<MemEngine, MemLog> {
        Replica::open(id(b), MemLog::new(), MemEngine::new()).unwrap()
    }

    fn datum(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn commit_chains_and_applies() {
        let mut r = replica(1);
        let h1 = r.commit(vec![(datum("a"), 1)], 100).unwrap();
        let _h2 = r.commit(vec![(datum("a"), 2), (datum("b"), -1)], 200).unwrap();
        assert_eq!(r.vector().get(&id(1)), 2);
        assert_eq!(r.engine().count(b"a"), 3);
        assert_eq!(r.engine().count(b"b"), -1);
        assert_eq!(r.watermark(), 200);
        let frames = r.frames_after(&id(1), 0);
        assert_eq!(frames[0].prev_hash, ZERO_HASH);
        assert_eq!(frames[1].prev_hash, h1);
    }

    #[test]
    fn reopen_replays_only_unapplied() {
        // Build a log of three frames; give the reopening engine a cursor
        // that has already applied the first — only the suffix replays.
        let mut r = replica(1);
        r.commit(vec![(datum("a"), 1)], 10).unwrap();
        r.commit(vec![(datum("b"), 1)], 20).unwrap();
        r.commit(vec![(datum("c"), 1)], 30).unwrap();
        let mut log = MemLog::new();
        for f in r.frames_after(&id(1), 0) {
            log.append(f).unwrap();
        }

        let mut pre = MemEngine::new();
        pre.apply(&log.frames()[0], 10).unwrap();
        let r2 = Replica::open(id(1), log, pre).unwrap();
        assert_eq!(r2.engine().count(b"a"), 1, "not double-applied");
        assert_eq!(r2.engine().count(b"b"), 1);
        assert_eq!(r2.engine().count(b"c"), 1);
        assert_eq!(r2.watermark(), 30);
        assert_eq!(r2.vector().get(&id(1)), 3);
    }

    #[test]
    fn ingest_dedups() {
        let mut a = replica(1);
        a.commit(vec![(datum("a"), 1)], 10).unwrap();
        let frame = a.frames_after(&id(1), 0)[0].clone();

        let mut b = replica(2);
        assert!(b.ingest(frame.clone()).unwrap());
        assert!(!b.ingest(frame).unwrap(), "duplicate is a no-op");
        assert_eq!(b.engine().count(b"a"), 1);
    }

    #[test]
    fn ingest_gap_rejected() {
        let mut a = replica(1);
        a.commit(vec![(datum("a"), 1)], 10).unwrap();
        a.commit(vec![(datum("b"), 1)], 20).unwrap();
        let second = a.frames_after(&id(1), 1)[0].clone();

        let mut b = replica(2);
        match b.ingest(second) {
            Err(SodError::Gap { have: 0, got: 2, .. }) => {}
            other => panic!("expected gap, got {other:?}"),
        }
    }

    #[test]
    fn equivocation_poisons() {
        let mut a = replica(1);
        a.commit(vec![(datum("a"), 1)], 10).unwrap();
        let honest = a.frames_after(&id(1), 0)[0].clone();

        // a forged alternative frame 1 from the same origin
        let mut forged = honest.clone();
        forged.payload = vec![(datum("evil"), 1)];

        let mut b = replica(2);
        b.ingest(honest.clone()).unwrap();
        match b.ingest(forged) {
            Err(SodError::Equivocation { seq: 1, .. }) => {}
            other => panic!("expected equivocation, got {other:?}"),
        }
        // origin is now poisoned, even for the honest frame
        match b.ingest(honest) {
            Err(SodError::Poisoned(o)) if o == id(1) => {}
            other => panic!("expected poisoned, got {other:?}"),
        }
        // other origins unaffected
        let mut c = replica(3);
        c.commit(vec![(datum("fine"), 1)], 5).unwrap();
        let fine = c.frames_after(&id(3), 0)[0].clone();
        assert!(b.ingest(fine).unwrap());
    }

    #[test]
    fn watermark_is_max_event_time() {
        let mut a = replica(1);
        a.commit(vec![(datum("late"), 1)], 500).unwrap();
        a.commit(vec![(datum("early"), 1)], 100).unwrap();
        assert_eq!(a.watermark(), 500);
        assert_eq!(a.engine().watermark(), 500);
    }
}
