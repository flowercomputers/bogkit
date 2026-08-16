//! The sans-io sync session: anti-entropy between two replicas.
//!
//! A [`Session`] owns no sockets — it consumes and produces [`Msg`] values
//! and mutates the local [`Replica`]; transports (`transport::ws`, tests,
//! future browser transports) decide how bytes move and in what order.
//!
//! Protocol: each side sends [`Msg::Hello`] (versions + version vector,
//! SOD-9); on receiving the peer's Hello, a side streams every frame the
//! peer lacks — **all origins it holds**, not just its own feed, which is
//! what makes relay and mesh topologies work — as batched [`Msg::Frames`],
//! per-origin contiguous, followed by [`Msg::Done`]. A session is finished
//! when both sides have sent and received `Done`.
//!
//! There is no session state to persist: the version vector *is* the
//! resume point, so a session killed at any byte is simply re-run (SOD-6).

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::frame::Frame;
use crate::replica::Replica;
use crate::store::LogStore;
use crate::vector::VersionVector;
use crate::SodError;

/// Bumped on any wire-format or protocol change (SOD-9).
pub const PROTOCOL_VERSION: u16 = 1;

/// Frames per [`Msg::Frames`] batch.
const BATCH: usize = 256;

/// One sync-protocol message; postcard-encoded by transports.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Msg {
    Hello {
        protocol: u16,
        schema: u32,
        vector: VersionVector,
    },
    Frames(Vec<Frame>),
    Done,
}

/// One replica's half of one sync session.
pub struct Session {
    schema: u32,
    sent_done: bool,
    peer_done: bool,
    skipped: Vec<SodError>,
}

impl Session {
    /// `schema` is the application's schema version: replicas whose apps
    /// disagree on it refuse to sync rather than corrupt (SOD-9).
    pub fn new(schema: u32) -> Self {
        Session {
            schema,
            sent_done: false,
            peer_done: false,
            skipped: Vec::new(),
        }
    }

    /// Our opening message.
    pub fn hello<E: Engine, L: LogStore>(&self, r: &Replica<E, L>) -> Msg {
        Msg::Hello {
            protocol: PROTOCOL_VERSION,
            schema: self.schema,
            vector: r.vector().clone(),
        }
    }

    /// Feed one inbound message; returns outbound messages (possibly none).
    ///
    /// Frames from an origin that equivocates (or is already poisoned) are
    /// skipped and recorded in [`skipped`](Session::skipped) — the session
    /// continues for other origins (SOD-2). A [`SodError::Gap`] or version
    /// mismatch is a hard protocol error.
    pub fn on_msg<E: Engine, L: LogStore>(
        &mut self,
        r: &mut Replica<E, L>,
        msg: Msg,
    ) -> Result<Vec<Msg>, SodError> {
        match msg {
            Msg::Hello {
                protocol,
                schema,
                vector,
            } => {
                if protocol != PROTOCOL_VERSION || schema != self.schema {
                    return Err(SodError::VersionMismatch {
                        ours: (PROTOCOL_VERSION, self.schema),
                        theirs: (protocol, schema),
                    });
                }
                let poisoned: Vec<_> = r.poisoned().copied().collect();
                let mut out: Vec<Msg> = Vec::new();
                for (origin, theirs, _have) in r.vector().ahead_of(&vector) {
                    if poisoned.contains(&origin) {
                        continue;
                    }
                    // chunk each origin's suffix directly: one clone per
                    // frame, and every batch stays per-origin contiguous
                    for chunk in r.frames_after(&origin, theirs).chunks(BATCH) {
                        out.push(Msg::Frames(chunk.to_vec()));
                    }
                }
                out.push(Msg::Done);
                self.sent_done = true;
                Ok(out)
            }
            Msg::Frames(frames) => {
                for frame in frames {
                    match r.ingest(frame) {
                        Ok(_) => {}
                        Err(e @ (SodError::Equivocation { .. } | SodError::Poisoned(_))) => {
                            self.skipped.push(e);
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(Vec::new())
            }
            Msg::Done => {
                self.peer_done = true;
                r.sync_log()?;
                Ok(Vec::new())
            }
        }
    }

    /// True once we have both sent and received `Done`.
    pub fn finished(&self) -> bool {
        self.sent_done && self.peer_done
    }

    /// Per-origin refusals recorded while the session continued (SOD-2).
    pub fn skipped(&self) -> &[SodError] {
        &self.skipped
    }

    /// Consume the session, yielding its recorded refusals.
    pub fn into_skipped(self) -> Vec<SodError> {
        self.skipped
    }
}

/// Drive a complete session between two in-process replicas. Returns the
/// per-origin refusals both sides recorded while continuing (SOD-2) —
/// empty on a fully clean sync.
pub fn sync_pair<E1: Engine, L1: LogStore, E2: Engine, L2: LogStore>(
    a: &mut Replica<E1, L1>,
    b: &mut Replica<E2, L2>,
    schema: u32,
) -> Result<Vec<SodError>, SodError> {
    use std::collections::VecDeque;
    let mut sa = Session::new(schema);
    let mut sb = Session::new(schema);
    // FIFO delivery: batches beyond the first must arrive in send order,
    // or contiguous-suffix ingestion fails with a gap
    let mut to_b: VecDeque<Msg> = VecDeque::from([sa.hello(a)]);
    let mut to_a: VecDeque<Msg> = VecDeque::from([sb.hello(b)]);
    while !(sa.finished() && sb.finished() && to_a.is_empty() && to_b.is_empty()) {
        if let Some(m) = to_b.pop_front() {
            to_a.extend(sb.on_msg(b, m)?);
        }
        if let Some(m) = to_a.pop_front() {
            to_b.extend(sa.on_msg(a, m)?);
        }
    }
    let mut skipped = sa.skipped;
    skipped.extend(sb.skipped);
    Ok(skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::MemEngine;
    use crate::frame::ReplicaId;
    use crate::store::MemLog;

    fn replica(b: u8) -> Replica<MemEngine, MemLog> {
        Replica::open(ReplicaId([b; 16]), MemLog::new(), MemEngine::new()).unwrap()
    }

    #[test]
    fn two_replica_session_converges() {
        let mut a = replica(1);
        let mut b = replica(2);
        a.commit(vec![(b"a1".to_vec(), 1)], 10).unwrap();
        a.commit(vec![(b"a2".to_vec(), 2)], 20).unwrap();
        b.commit(vec![(b"b1".to_vec(), -1)], 30).unwrap();

        sync_pair(&mut a, &mut b, 1).unwrap();

        assert_eq!(a.vector(), b.vector());
        assert_eq!(a.engine().view_bytes(), b.engine().view_bytes());
        assert_eq!(a.watermark(), b.watermark());
    }

    #[test]
    fn large_diff_crosses_batch_boundary() {
        // >BATCH frames force multiple Frames messages; delivery must be
        // FIFO or the second batch arrives before the first and gaps out.
        let mut a = replica(1);
        let mut b = replica(2);
        for i in 0..300u64 {
            a.commit(vec![(i.to_be_bytes().to_vec(), 1)], i).unwrap();
        }
        b.commit(vec![(b"from b".to_vec(), 1)], 7).unwrap();

        sync_pair(&mut a, &mut b, 1).unwrap();

        assert_eq!(a.vector(), b.vector());
        assert_eq!(a.engine().view_bytes(), b.engine().view_bytes());
        assert_eq!(b.vector().get(&ReplicaId([1; 16])), 300);
    }

    #[test]
    fn version_mismatch_refuses() {
        let mut a = replica(1);
        let mut b = replica(2);
        a.commit(vec![(b"x".to_vec(), 1)], 10).unwrap();

        let sa = Session::new(1);
        let mut sb = Session::new(2);
        let hello_a = sa.hello(&a);
        match sb.on_msg(&mut b, hello_a) {
            Err(SodError::VersionMismatch { ours, theirs }) => {
                assert_eq!(ours, (PROTOCOL_VERSION, 2));
                assert_eq!(theirs, (PROTOCOL_VERSION, 1));
            }
            other => panic!("expected mismatch, got {other:?}"),
        }
        // nothing was exchanged
        assert_eq!(b.vector().get(&ReplicaId([1; 16])), 0);
    }

    #[test]
    fn relay_carries_third_party_frames() {
        let mut a = replica(1);
        let mut b = replica(2);
        let mut c = replica(3);
        a.commit(vec![(b"from-a".to_vec(), 1)], 10).unwrap();

        sync_pair(&mut a, &mut b, 1).unwrap();
        // c never talks to a — only to b, which relays a's feed
        sync_pair(&mut b, &mut c, 1).unwrap();

        assert_eq!(c.vector().get(&ReplicaId([1; 16])), 1);
        assert_eq!(c.engine().count(b"from-a"), 1);
        assert_eq!(c.engine().view_bytes(), a.engine().view_bytes());
    }
}
