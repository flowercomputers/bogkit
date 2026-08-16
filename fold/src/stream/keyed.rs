use std::cell::RefCell;
use std::path::Path;

use serde::{Serialize, de::DeserializeOwned};

use super::{Stream, Tx};
use crate::pipeline::{Keyed, Push};

thread_local! {
    /// Scratch key buffer for the `&self` read paths ([`KeyedStream::get`],
    /// [`KeyedStream::contains`]), which can't borrow the stream's own
    /// buffers mutably.
    static READ_KEY_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Serialize `value` into `buf`, reusing its capacity.
fn enc<T: Serialize + ?Sized>(buf: &mut Vec<u8>, value: &T) {
    buf.clear();
    postcard::to_io(value, &mut *buf).unwrap();
}

/// A [`Stream`] fronted by a primary-key table: each key holds at most one
/// record, and the table decides what flows into the graph.
///
/// [`upsert`](KeyedTx::upsert) replaces: a key's old record is first
/// retracted from the pipeline, so downstream state always reflects the
/// table's current rows, and re-upserting an unchanged record doesn't
/// churn the graph at all. [`remove`](KeyedTx::remove) is by key:
/// the stored record is looked up and retracted for you, so callers never
/// reproduce a record to delete it — the usual database contract, in
/// contrast to [`Stream`]'s raw delta interface.
///
/// The pipeline receives [`Keyed`]`<K, D>` deltas: the primary key rides
/// along for keyed sinks ([`Table`](crate::pipeline::terminal::Table),
/// [`Bm25`](crate::pipeline::terminal::search::Bm25), ...); use
/// [`Unkey`](crate::pipeline::Unkey) where downstream only wants the record.
///
/// Built on an owned [`Stream`]: the table lives in its own keyspace in the
/// same store, so table updates and graph effects commit atomically, and
/// reopening resumes both together.
///
/// ```no_run
/// use fold::pipeline::terminal;
/// use fold::stream::KeyedStream;
///
/// let mut st = KeyedStream::new("rows.db", terminal::Table::new("rows"));
/// st.wtx(|tx| {
///     tx.upsert(&1u32, &"alice".to_string());
///     tx.upsert(&1u32, &"bob".to_string()); // retracts "alice"
///     tx.remove(&2u32); // absent: no-op
/// });
/// assert_eq!(st.get(&1), Some("bob".to_string()));
/// ```
pub struct KeyedStream<K: Clone, D: Clone, P: Push<Keyed<K, D>>> {
    inner: Stream<Keyed<K, D>, P>,
    table: fjall::SingleWriterTxKeyspace,
    key_buf: Vec<u8>,
    val_buf: Vec<u8>,
}

impl<K, D, P> KeyedStream<K, D, P>
where
    K: Clone + Serialize,
    D: Clone + Serialize + DeserializeOwned,
    P: Push<Keyed<K, D>>,
{
    /// Open (or create) the store at `path` and initialize the pipeline;
    /// see [`Stream::new`].
    pub fn new(path: impl AsRef<Path>, pipeline: P) -> Self {
        let inner = Stream::new(path, pipeline);
        let table = inner
            .store()
            .keyspace("keyed_root", fjall::KeyspaceCreateOptions::default)
            .unwrap();
        KeyedStream {
            inner,
            table,
            key_buf: Default::default(),
            val_buf: Default::default(),
        }
    }

    /// Run a write transaction over the table and the pipeline: every
    /// upsert and removal commits atomically when `f` returns, and rolls
    /// back together if it panics. See [`Stream::wtx`].
    pub fn wtx<R>(&mut self, f: impl FnOnce(&mut KeyedTx<'_, '_, '_, K, D, P>) -> R) -> R {
        let table = self.table.clone();
        let key_buf = &mut self.key_buf;
        let val_buf = &mut self.val_buf;
        self.inner.wtx(move |tx| {
            f(&mut KeyedTx {
                tx,
                table,
                key_buf,
                val_buf,
            })
        })
    }

    /// Run a read transaction over one consistent snapshot across all
    /// sinks; see [`Stream::rtx`].
    pub fn rtx<R>(&self, f: impl for<'tx> FnOnce(P::Reader<'tx, fjall::Snapshot>) -> R) -> R {
        self.inner.rtx(f)
    }

    /// The committed record under `key`, if any.
    pub fn get(&self, key: &K) -> Option<D> {
        use fjall::Readable;
        READ_KEY_BUF.with_borrow_mut(|buf| {
            enc(buf, key);
            self.inner
                .store()
                .read_tx()
                .get(&self.table, &*buf)
                .unwrap()
                .map(|v| postcard::from_bytes(&v).unwrap())
        })
    }

    /// Whether `key` holds a committed record.
    pub fn contains(&self, key: &K) -> bool {
        use fjall::Readable;
        READ_KEY_BUF.with_borrow_mut(|buf| {
            enc(buf, key);
            self.inner
                .store()
                .read_tx()
                .contains_key(&self.table, &*buf)
                .unwrap()
        })
    }

    /// Fsync all committed state to disk; see [`Stream::checkpoint`].
    pub fn checkpoint(&mut self) {
        self.inner.checkpoint()
    }

    /// The underlying fjall database; see [`Stream::db`].
    pub fn db(&self) -> &fjall::SingleWriterTxDatabase {
        self.inner.db()
    }
}

/// Write handle passed to [`KeyedStream::wtx`] closures.
pub struct KeyedTx<'a, 'g, 'tx, K: Clone, D: Clone, P: Push<Keyed<K, D>>> {
    tx: &'a mut Tx<'g, 'tx, Keyed<K, D>, P>,
    table: fjall::SingleWriterTxKeyspace,
    key_buf: &'a mut Vec<u8>,
    val_buf: &'a mut Vec<u8>,
}

impl<'tx, K, D, P> KeyedTx<'_, '_, 'tx, K, D, P>
where
    K: Clone + Serialize,
    D: Clone + Serialize + DeserializeOwned,
    P: Push<Keyed<K, D>>,
{
    /// Look up whatever key is currently encoded in `key_buf`.
    fn get_raw(&mut self) -> Option<D> {
        self.tx
            .tx
            .get(&self.table, &*self.key_buf)
            .map(|v| postcard::from_bytes(&v).unwrap())
    }

    /// Insert or replace the record under `key`, returning the record it
    /// replaced.
    ///
    /// A replaced record is retracted from the pipeline before the new one
    /// is inserted; replacing a record with an equal one leaves the graph
    /// untouched.
    pub fn upsert(&mut self, key: &K, data: &D) -> Option<D> {
        enc(self.key_buf, key);
        enc(self.val_buf, data);
        let old = match self.tx.tx.get(&self.table, &*self.key_buf) {
            Some(v) if v.as_ref() == self.val_buf.as_slice() => {
                return Some(data.clone()); // unchanged: no graph churn
            }
            Some(v) => {
                let old: D = postcard::from_bytes(&v).unwrap();
                self.tx.push(&Keyed::new(key.clone(), old.clone()), -1);
                Some(old)
            }
            None => None,
        };
        self.tx
            .tx
            .insert(&self.table, &*self.key_buf, &*self.val_buf);
        self.tx.push(&Keyed::new(key.clone(), data.clone()), 1);
        old
    }

    /// Remove and return the record under `key`, retracting it from the
    /// pipeline. Removing an absent key is a no-op.
    pub fn remove(&mut self, key: &K) -> Option<D> {
        enc(self.key_buf, key);
        let old = self.get_raw()?;
        self.tx.tx.remove(&self.table, &*self.key_buf);
        self.tx.push(&Keyed::new(key.clone(), old.clone()), -1);
        Some(old)
    }

    /// The record under `key`, seeing this transaction's own writes.
    pub fn get(&mut self, key: &K) -> Option<D> {
        enc(self.key_buf, key);
        self.get_raw()
    }

    /// Whether `key` holds a record, seeing this transaction's own writes.
    pub fn contains(&mut self, key: &K) -> bool {
        enc(self.key_buf, key);
        self.tx.tx.get(&self.table, &*self.key_buf).is_some()
    }

    /// Read every sink from this write transaction's own uncommitted
    /// state; see [`Tx::rtx`].
    pub fn rtx<'s, R>(
        &'s mut self,
        f: impl FnOnce(P::Reader<'s, fjall::SingleWriterWriteTx<'tx>>) -> R,
    ) -> R {
        self.tx.rtx(f)
    }
}
