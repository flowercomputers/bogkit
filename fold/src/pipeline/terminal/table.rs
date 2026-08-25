use std::{cell::RefCell, marker::PhantomData};

use fjall::Readable;
use fxhash::FxHashMap;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    pipeline::{Keyed, Push},
    stream::{PipelineInitCtx, WriteTx},
};

/// Persistent last-writer-wins register per key: point-read the newest value
/// posted under each `K`.
///
/// Within a transaction the most recent push to a key decides its fate: a
/// positive delta upserts that value, a non-positive one deletes the key.
/// No prior state is read. This is exactly the changelog discipline
/// [`Aggregate`](crate::pipeline::Aggregate) emits (`-old` then `+new` per
/// key), making `Table` the natural sink for materializing a group-by view:
///
/// ```no_run
/// use fold::pipeline::{Aggregate, KeyBy, terminal};
/// use fold::stream::Stream;
///
/// // sum per user id, point-readable
/// let mut st = Stream::new(
///     "spend.db",
///     KeyBy::new(
///         |(user, _): &(u32, i64)| *user,
///         Aggregate::new(
///             "sums",
///             |acc: &mut i64, (_, amount): &(u32, i64), d| *acc += amount * d as i64,
///             terminal::Table::new("spend"),
///         ),
///     ),
/// );
/// st.wtx(|tx| tx.insert(&(7, 100)));
/// st.rtx(|spend| assert_eq!(spend.get(&7), Some(100)));
/// ```
pub struct Table<K, V> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    // encoded key -> (value, delta) of the last push this tx
    pending: FxHashMap<Vec<u8>, (V, isize)>,
    _p: PhantomData<K>,
}

impl<K, V> Table<K, V> {
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Table {
            name: name.into(),
            ks: None,
            pending: FxHashMap::default(),
            _p: PhantomData,
        }
    }
}

impl<K, V> Push<Keyed<K, V>> for Table<K, V>
where
    K: Clone + Serialize + DeserializeOwned,
    V: Clone + Serialize + DeserializeOwned,
{
    type Reader<'tx, R: Readable + 'tx> = TableReader<'tx, R, K, V>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, V>, delta: isize) {
        tx.buf.clear();
        postcard::to_io(&data.key, &mut tx.buf).unwrap();
        self.pending
            .insert(tx.buf.clone(), (data.val.clone(), delta));
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        let ks = self.ks.clone().unwrap();
        for (key, (val, delta)) in self.pending.drain() {
            if delta > 0 {
                tx.buf.clear();
                postcard::to_io(&val, &mut tx.buf).unwrap();
                let v = std::mem::take(&mut tx.buf);
                tx.insert(&ks, &key, &v);
                tx.buf = v;
            } else {
                tx.remove(&ks, &key);
            }
        }
    }

    fn abort(&mut self) {
        self.pending.clear();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        TableReader {
            tx,
            ks: self.ks.clone().unwrap(),
            name: self.name.clone(),
            _p: PhantomData,
        }
    }
}

/// Read handle for [`Table`], pinned to one snapshot.
pub struct TableReader<'tx, R: Readable, K, V> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    name: String,
    _p: PhantomData<(K, V)>,
}

impl<'tx, R: Readable, K, V> TableReader<'tx, R, K, V> {
    /// The sink name this reader serves, as given to [`Table::new`].
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl<'tx, R: Readable, K: Serialize, V: DeserializeOwned> TableReader<'tx, R, K, V> {
    fn with_key<T>(&self, key: &K, f: impl FnOnce(&Self, &[u8]) -> T) -> T {
        thread_local! {
            static KEY_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        }
        KEY_BUF.with_borrow_mut(|buf| {
            buf.clear();
            postcard::to_io(key, &mut *buf).unwrap();
            f(self, buf)
        })
    }

    /// The current value under `key`, if any.
    pub fn get(&self, key: &K) -> Option<V> {
        self.with_key(key, |s, k| {
            s.tx.get(&s.ks, k)
                .unwrap()
                .map(|v| postcard::from_bytes(&v).unwrap())
        })
    }

    /// Whether `key` holds a value.
    pub fn contains(&self, key: &K) -> bool {
        self.with_key(key, |s, k| s.tx.contains_key(&s.ks, k).unwrap())
    }

    /// Iterate all `(key, value)` pairs, ordered by the key's `postcard`
    /// encoding.
    pub fn iter(&self) -> impl Iterator<Item = (K, V)> + '_
    where
        K: DeserializeOwned,
    {
        self.tx.iter(&self.ks).map(|kv| {
            let (key, val) = kv.into_inner().unwrap();
            (
                postcard::from_bytes(&key).unwrap(),
                postcard::from_bytes(&val).unwrap(),
            )
        })
    }
}
