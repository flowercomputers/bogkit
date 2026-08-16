//! [`FoldEngine`]: the fold-backed [`Engine`] (feature `fold-engine`).
//!
//! Wraps a fold [`Stream`] with the app's pipeline. Frame deltas and the
//! applied-cursor advance commit in **one fold write transaction** — the
//! cursor lives in the metadata keyspace `meta_sod_cursor`
//! (key = 16-byte origin id, value = 8-byte big-endian seq) — which is what
//! makes crash healing exact (SOD-5): on open, the cursor tells the replica
//! precisely which log suffix the fold db has not yet seen.

use std::path::Path;

use fold::pipeline::Push;
use fold::stream::{Readable, Stream};
use serde::de::DeserializeOwned;

use crate::engine::Engine;
use crate::time::Watermark;
use crate::vector::VersionVector;
use crate::{Frame, ReplicaId, SodError};

pub struct FoldEngine<D: Clone, P: Push<D>> {
    stream: Stream<D, P>,
    cursor_ks: fjall::SingleWriterTxKeyspace,
    applied: VersionVector,
    watermark: Watermark,
}

impl<D: Clone, P: Push<D>> FoldEngine<D, P> {
    /// Open the fold store at `path` with the app's `pipeline`, and load
    /// the applied cursor from the metadata keyspace.
    ///
    /// `watermark` is the handle the app also passes to any clock-taking
    /// pipeline operators; the engine advances it on every apply.
    pub fn open(path: impl AsRef<Path>, pipeline: P, watermark: Watermark) -> Self {
        let stream = Stream::new(path, pipeline);
        let cursor_ks = stream.meta_keyspace("sod_cursor");
        let mut applied = VersionVector::new();
        let snap = stream.meta_snapshot();
        for kv in snap.iter(&cursor_ks) {
            let (k, v) = kv.into_inner().unwrap();
            let origin = ReplicaId(k.as_ref().try_into().expect("cursor key is 16 bytes"));
            let seq = u64::from_be_bytes(v.as_ref().try_into().expect("cursor value is 8 bytes"));
            applied.set(origin, seq);
        }
        FoldEngine {
            stream,
            cursor_ks,
            applied,
            watermark,
        }
    }

    /// The wrapped stream, for [`rtx`](Stream::rtx) view reads.
    pub fn stream(&self) -> &Stream<D, P> {
        &self.stream
    }

    /// The engine's watermark handle.
    pub fn watermark(&self) -> &Watermark {
        &self.watermark
    }
}

impl<D, P> Engine for FoldEngine<D, P>
where
    D: Clone + DeserializeOwned,
    P: Push<D>,
{
    fn apply(&mut self, frame: &Frame, watermark: u64) -> Result<(), SodError> {
        // Decode every datum before touching the store, so a bad frame
        // fails cleanly without a partial transaction.
        let mut deltas = Vec::with_capacity(frame.payload.len());
        for (bytes, mult) in &frame.payload {
            let d: D = postcard::from_bytes(bytes)
                .map_err(|_| SodError::Corrupt("frame datum does not decode as pipeline type"))?;
            deltas.push((d, *mult));
        }
        self.watermark.advance(watermark);
        let ks = self.cursor_ks.clone();
        let origin = frame.origin.0;
        let seq = frame.seq.to_be_bytes();
        self.stream.wtx(|tx| {
            for (d, mult) in &deltas {
                tx.push(d, *mult as isize);
            }
            tx.meta().insert(&ks, origin, seq);
        });
        self.applied.set(frame.origin, frame.seq);
        Ok(())
    }

    fn applied(&self) -> VersionVector {
        self.applied.clone()
    }

    fn seed_watermark(&mut self, wm: u64) {
        self.watermark.advance(wm);
    }
}
