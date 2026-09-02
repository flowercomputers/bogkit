use std::cell::Cell;
use std::rc::Rc;

use crate::pipeline::{terminal, Push};
use crate::stream::{PipelineInitCtx, Readable, Stream, WriteTx};
use crate::tests::fresh_db;

/// Panics in `commit` while `armed`, then behaves; buffers one count like a
/// stateful node would, so a skipped `abort` leaves visible orphan state.
struct PanicOnCommit<G> {
    armed: Rc<Cell<bool>>,
    pending: isize,
    next: G,
}

impl<G: Push<u32>> Push<u32> for PanicOnCommit<G> {
    type Reader<'tx, R: Readable + 'tx> = G::Reader<'tx, R>;
    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.next.init(init);
    }
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &u32, delta: isize) {
        self.pending += delta;
        let _ = (tx, data);
    }
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if self.armed.get() {
            panic!("simulated commit failure");
        }
        for _ in 0..self.pending {
            self.next.push(tx, &1, 1);
        }
        self.pending = 0;
        self.next.commit(tx);
    }
    fn abort(&mut self) {
        self.pending = 0;
        self.next.abort();
    }
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        self.next.reader(tx)
    }
}

/// A panic during the pipeline's final commit must reach `abort()`: without
/// it, buffered deltas survive the rolled-back transaction and replay into
/// the next one as orphans.
#[test]
fn panic_in_commit_aborts_pending_state() {
    let armed = Rc::new(Cell::new(true));
    let mut st = Stream::new(
        fresh_db("abort_commit_panic"),
        PanicOnCommit {
            armed: Rc::clone(&armed),
            pending: 0,
            next: terminal::Count::new("n"),
        },
    );

    let boom = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        st.wtx(|tx| tx.insert(&7u32));
    }));
    assert!(boom.is_err(), "armed commit must panic");

    // disarm; the failed transaction's buffered delta must NOT replay
    armed.set(false);
    st.wtx(|tx| tx.insert(&7u32));
    st.rtx(|count| assert_eq!(count.get(), 1, "orphan delta from aborted tx replayed"));
}
