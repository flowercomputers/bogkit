use std::{marker::PhantomData, path::Path};

use super::*;

/// A pipeline bound to a persistent store; the crate's entry point.
///
/// Owns the [`Push`] graph and the backing fjall database, and exposes the
/// two ways in: [`wtx`](Stream::wtx) to feed deltas through the pipeline
/// atomically, and [`rtx`](Stream::rtx) to read every sink from one
/// consistent snapshot.
///
/// Because all sink state is persistent, reopening a `Stream` on an existing
/// `path` (with the same pipeline shape and sink names) resumes exactly
/// where the last committed transaction left off.
pub struct Stream<D: Clone, P: Push<D>> {
    pipeline: P,
    store: fjall::SingleWriterTxDatabase,
    _p: PhantomData<D>,
}

impl<D: Clone, P: Push<D>> Stream<D, P> {
    /// Open (or create) the store at `path` and initialize the pipeline,
    /// resolving each named node's keyspace.
    ///
    /// # Panics
    /// Panics if the store cannot be opened or if two nodes claim the same
    /// name.
    pub fn new(path: impl AsRef<Path>, mut pipeline: P) -> Self {
        let store = fjall::SingleWriterTxDatabase::builder(path).open().unwrap();

        let mut init = PipelineInitCtx::new(&store);
        pipeline.init(&mut init);

        Stream {
            pipeline,
            store,
            _p: PhantomData,
        }
    }

    /// Run a write transaction: every delta pushed through the [`Tx`] handle
    /// commits atomically when `f` returns.
    ///
    /// On commit, stateful nodes flush their buffered updates to the store.
    /// If `f` panics, the transaction rolls back — the store is untouched,
    /// pipeline nodes reset their pending state, and the panic resumes.
    ///
    /// [`Tx::rtx`] reads the sinks mid-transaction, seeing every delta
    /// pushed so far.
    pub fn wtx<R>(&mut self, f: impl FnOnce(&mut Tx<'_, '_, D, P>) -> R) -> R {
        let mut wtx = WriteTx::new(self.store.write_tx());

        let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            f(&mut Tx {
                pipeline: &mut self.pipeline,
                tx: &mut wtx,
                _p: PhantomData,
            })
        })) {
            Ok(r) => r,
            Err(p) => {
                // fjall tx rolls back on drop
                self.pipeline.abort();
                std::panic::resume_unwind(p);
            }
        };

        self.pipeline.commit(&mut wtx);
        wtx.commit();
        r
    }

    /// Run a read transaction over one consistent snapshot across all sinks.
    ///
    /// The closure receives the pipeline's reader, which mirrors its sink
    /// structure: a lone sink yields its reader, tuple branches yield tuples
    /// of readers. The snapshot is pinned for the duration of `f`.
    pub fn rtx<R>(&self, f: impl for<'tx> FnOnce(P::Reader<'tx, fjall::Snapshot>) -> R) -> R {
        let tx = self.store.read_tx();
        f(self.pipeline.reader(&tx))
        // tx drops here
    }

    /// Fsync all committed state to disk.
    ///
    /// Commits are durable against process crashes as soon as `wtx` returns;
    /// checkpointing additionally hardens them against OS/power failure.
    pub fn checkpoint(&mut self) {
        self.store.persist(fjall::PersistMode::SyncAll).unwrap();
    }

    pub(crate) fn store(&self) -> &fjall::SingleWriterTxDatabase {
        &self.store
    }
}
