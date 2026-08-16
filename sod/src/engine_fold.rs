//! [`FoldEngine`]: the fold-backed [`Engine`] (feature `fold-engine`).
//!
//! Built entirely on fold's **public** API — sod consumes fold, it never
//! modifies it. The applied-cursor (which frame each origin's feed has been
//! applied through) is an ordinary pipeline node, [`AppliedCursor`], that
//! sod wraps around the app's pipeline:
//!
//! - at [`init`](fold::pipeline::Push::init) it claims the sink name
//!   `sod_cursor` (collision-checked by fold like any sink) and recovers
//!   the persisted cursor from the startup snapshot — the same pattern
//!   fold's `Retain` uses for its sequence counter;
//! - at [`commit`](fold::pipeline::Push::commit) it writes the `(origin,
//!   seq)` deposited by [`FoldEngine::apply`] into its keyspace, **inside
//!   the same fold transaction** as the frame's deltas.
//!
//! That one-transaction property is what makes crash healing exact
//! (SOD-5): on open, the cursor tells the replica precisely which log
//! suffix the fold db has not yet seen.
//!
//! Cursor layout: key = 16-byte origin id, value = 8-byte big-endian seq.
//! App pipelines must not name a sink `sod_cursor`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use fold::pipeline::Push;
use fold::stream::{PipelineInitCtx, Readable, Stream, WriteTx};
use serde::de::DeserializeOwned;

use crate::engine::Engine;
use crate::time::Watermark;
use crate::vector::VersionVector;
use crate::{Frame, ReplicaId, SodError};

/// State shared between [`FoldEngine`] and its [`AppliedCursor`] node.
#[derive(Default)]
struct CursorState {
    /// Cursor recovered from the store at init.
    loaded: VersionVector,
    /// Cursor advance deposited by `apply` for the in-flight transaction.
    pending: Option<([u8; 16], u64)>,
}

/// The pipeline node that persists the applied-cursor transactionally with
/// the app pipeline it wraps. Public only because it appears in
/// [`FoldEngine`]'s stream type; apps never construct it.
pub struct AppliedCursor<G> {
    ks: Option<fjall::SingleWriterTxKeyspace>,
    shared: Arc<Mutex<CursorState>>,
    next: G,
}

impl<D, G> Push<D> for AppliedCursor<G>
where
    D: Clone,
    G: Push<D>,
{
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        let ks = init.keyspace("sod_cursor");
        let snapshot = init.snapshot();
        {
            let mut shared = self.shared.lock().unwrap();
            for kv in snapshot.iter(&ks) {
                let (k, v) = kv.into_inner().unwrap();
                let origin = ReplicaId(k.as_ref().try_into().expect("cursor key is 16 bytes"));
                let seq =
                    u64::from_be_bytes(v.as_ref().try_into().expect("cursor value is 8 bytes"));
                shared.loaded.set(origin, seq);
            }
        }
        self.ks = Some(ks);
        self.next.init(init);
    }

    #[inline]
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        self.next.push(tx, data, delta);
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if let Some((origin, seq)) = self.shared.lock().unwrap().pending.take() {
            tx.insert(self.ks.as_ref().unwrap(), origin, seq.to_be_bytes());
        }
        self.next.commit(tx);
    }

    fn abort(&mut self) {
        self.shared.lock().unwrap().pending = None;
        self.next.abort();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}

pub struct FoldEngine<D: Clone, P: Push<D>> {
    stream: Stream<D, AppliedCursor<P>>,
    shared: Arc<Mutex<CursorState>>,
    applied: VersionVector,
    watermark: Watermark,
}

impl<D: Clone, P: Push<D>> FoldEngine<D, P> {
    /// Open the fold store at `path` with the app's `pipeline` (wrapped in
    /// the cursor node), recovering the applied cursor from the store.
    ///
    /// `watermark` is the handle the app also passes to any clock-taking
    /// pipeline operators; the engine advances it on every apply.
    pub fn open(path: impl AsRef<Path>, pipeline: P, watermark: Watermark) -> Self {
        let shared = Arc::new(Mutex::new(CursorState::default()));
        let stream = Stream::new(
            path,
            AppliedCursor {
                ks: None,
                shared: shared.clone(),
                next: pipeline,
            },
        );
        let applied = shared.lock().unwrap().loaded.clone();
        FoldEngine {
            stream,
            shared,
            applied,
            watermark,
        }
    }

    /// The wrapped stream, for [`rtx`](Stream::rtx) view reads. The cursor
    /// node is reader-transparent: `rtx` closures see the app pipeline's
    /// reader shape unchanged.
    pub fn stream(&self) -> &Stream<D, AppliedCursor<P>> {
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
        self.shared.lock().unwrap().pending = Some((frame.origin.0, frame.seq));
        self.stream.wtx(|tx| {
            for (d, mult) in &deltas {
                tx.push(d, *mult as isize);
            }
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
