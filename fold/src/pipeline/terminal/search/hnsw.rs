use std::{cell::RefCell, rc::Rc};

use anny::metric::{Metric, Scalar};
use fjall::Readable;
use fxhash::FxHashMap;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    pipeline::{Keyed, Push, Scored},
    stream::{PipelineInitCtx, WriteTx},
};

fn decode_vector<T: DeserializeOwned + Copy, const DIM: usize>(bytes: &[u8]) -> [T; DIM] {
    let v: Vec<T> = postcard::from_bytes(bytes).unwrap();
    std::array::from_fn(|i| v[i])
}

// The in-memory side of the sink, shared with readers. `ids`/`keys` tie the
// persisted rows to anny's ephemeral node ids; `stale` marks the graph as
// diverged from the store (an aborted transaction cannot un-mutate it), to
// be rebuilt from the persisted vectors on next use.
struct State<
    K,
    T,
    M: Metric<T>,
    const DIM: usize,
    const M0: usize,
    const TOP_K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> {
    index: anny::hnsw::Hnsw<T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>,
    ids: FxHashMap<Vec<u8>, u32>, // postcard(K) -> node id
    keys: FxHashMap<u32, K>,      // node id -> key
    stale: bool,
}

impl<
    K,
    T,
    M,
    const DIM: usize,
    const M0: usize,
    const TOP_K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> State<K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>
where
    K: DeserializeOwned,
    T: Scalar + DeserializeOwned,
    M: Metric<T> + Copy,
{
    fn upsert(&mut self, kenc: Vec<u8>, key: K, vec: [T; DIM]) {
        if let Some(old) = self.ids.remove(&kenc) {
            self.index.remove(old);
            self.keys.remove(&old);
        }
        let id = self.index.insert(vec);
        self.ids.insert(kenc, id);
        self.keys.insert(id, key);
    }

    fn remove(&mut self, kenc: &[u8]) -> bool {
        match self.ids.remove(kenc) {
            Some(old) => {
                self.index.remove(old);
                self.keys.remove(&old);
                true
            }
            None => false,
        }
    }

    // reconstruct the graph from the persisted `postcard(K) -> vector` rows
    fn rebuild(&mut self, metric: M, seed: u64, entries: impl Iterator<Item = (Vec<u8>, Vec<u8>)>) {
        self.index = anny::hnsw::Hnsw::new(metric, seed);
        self.ids.clear();
        self.keys.clear();
        for (kenc, venc) in entries {
            let key: K = postcard::from_bytes(&kenc).unwrap();
            self.upsert(kenc, key, decode_vector::<T, DIM>(&venc));
        }
        self.stale = false;
    }
}

/// Persistent approximate-nearest-neighbor index over [`Keyed`]`<K, [T;
/// DIM]>` embeddings, backed by [anny](anny)'s retractable HNSW graph.
///
/// Accepts `Keyed { key: document, val: embedding }` and maintains two
/// coupled structures: the vectors persist in this sink's keyspace (so the
/// index recovers on reopen), and an in-memory HNSW graph mirrors them for
/// sub-linear search. [`HnswReader::search`] returns the approximately
/// nearest keys ascending by distance under the metric `M` (see
/// [`anny::metric`] — smaller is closer).
///
/// Like the posting sinks, documents are set-semantic per key: within a
/// transaction the most recent push decides — positive (re)indexes the key
/// under that embedding, non-positive deletes it — with no read of prior
/// state. Retraction genuinely removes the node from the graph (anny repairs
/// the neighborhood), so recall does not decay under churn the way
/// tombstoning indexes do.
///
/// The graph lives in memory: it is rebuilt from the persisted vectors when
/// the stream opens, and again if a transaction aborts after a mid-tx flush
/// (a panic cannot un-mutate the graph, so it is marked stale and rebuilt
/// from committed state on next use). Under fold's single-writer discipline
/// readers otherwise always observe a graph consistent with their snapshot.
///
/// Tuning lives in the const parameters (`M0`, `TOP_K`, `EF_SEARCH`,
/// `EF_BUILD`, `MAX_LEVEL`), with usable defaults; `TOP_K` fixes the number
/// of results per search at compile time.
///
/// ```no_run
/// use anny::metric::L2;
/// use fold::pipeline::{Keyed, terminal::search::Hnsw};
/// use fold::stream::Stream;
///
/// let mut st = Stream::new("vecs.db", Hnsw::<u32, f32, L2, 4>::new("vecs", L2, 42));
/// st.wtx(|tx| tx.insert(&Keyed::new(7, [0.1, 0.2, 0.3, 0.4])));
/// st.rtx(|idx| {
///     for hit in idx.search(&[0.1, 0.2, 0.3, 0.4]) {
///         println!("{}: {}", hit.val, hit.score);
///     }
/// });
/// ```
pub struct Hnsw<
    K,
    T,
    M: Metric<T>,
    const DIM: usize,
    const M0: usize = 32,
    const TOP_K: usize = 10,
    const EF_SEARCH: usize = 40,
    const EF_BUILD: usize = 80,
    const MAX_LEVEL: usize = 16,
> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    metric: M,
    seed: u64,
    state: Rc<RefCell<State<K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>>>,
    // encoded key -> (key, embedding, delta) of the last push this tx
    pending: FxHashMap<Vec<u8>, (K, [T; DIM], isize)>,
    vec_buf: Vec<u8>,
}

impl<
    K,
    T,
    M,
    const DIM: usize,
    const M0: usize,
    const TOP_K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> Hnsw<K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>
where
    T: Scalar,
    M: Metric<T>,
{
    /// `name` identifies this sink's keyspace and must be unique among all
    /// named nodes in the pipeline. `seed` fixes the graph's level
    /// randomness, making builds deterministic.
    pub fn new(name: impl Into<String>, metric: M, seed: u64) -> Self
    where
        M: Copy,
    {
        Hnsw {
            name: name.into(),
            ks: None,
            metric,
            seed,
            state: Rc::new(RefCell::new(State {
                index: anny::hnsw::Hnsw::new(metric, seed),
                ids: FxHashMap::default(),
                keys: FxHashMap::default(),
                stale: false,
            })),
            pending: FxHashMap::default(),
            vec_buf: Default::default(),
        }
    }
}

impl<
    K,
    T,
    M,
    const DIM: usize,
    const M0: usize,
    const TOP_K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> Push<Keyed<K, [T; DIM]>> for Hnsw<K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>
where
    K: Clone + Serialize + DeserializeOwned,
    T: Scalar + Serialize + DeserializeOwned,
    M: Metric<T> + Copy,
{
    type Reader<'tx, R: Readable + 'tx> =
        HnswReader<'tx, R, K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        let ks = init.keyspace(&self.name);
        // recover the graph from the vectors persisted by earlier runs
        self.state.borrow_mut().rebuild(
            self.metric,
            self.seed,
            init.snapshot().iter(&ks).map(|kv| {
                let (k, v) = kv.into_inner().unwrap();
                (k.to_vec(), v.to_vec())
            }),
        );
        self.ks = Some(ks);
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, [T; DIM]>, delta: isize) {
        tx.buf.clear();
        postcard::to_io(&data.key, &mut tx.buf).unwrap();
        self.pending
            .insert(tx.buf.clone(), (data.key.clone(), data.val, delta));
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if self.pending.is_empty() {
            return;
        }
        let ks = self.ks.clone().unwrap();
        let mut state = self.state.borrow_mut();
        if state.stale {
            // the previous transaction aborted: this one sees clean
            // committed state, so resync the graph before applying
            let entries = tx.iter(&ks).map(|kv| {
                let (k, v) = kv.into_inner().unwrap();
                (k.to_vec(), v.to_vec())
            });
            let (metric, seed) = (self.metric, self.seed);
            state.rebuild(metric, seed, entries);
        }
        for (kenc, (key, vec, delta)) in self.pending.drain() {
            match delta {
                1.. => {
                    self.vec_buf.clear();
                    postcard::to_io(&vec[..], &mut self.vec_buf).unwrap();

                    tx.insert(&ks, &kenc, &self.vec_buf);
                    state.upsert(kenc, key, vec);
                }
                _ => {
                    if state.remove(&kenc) {
                        tx.remove(&ks, &kenc);
                    }
                }
            }
        }
    }

    fn abort(&mut self) {
        self.pending.clear();
        // graph mutations from any mid-tx flush cannot be undone in place
        self.state.borrow_mut().stale = true;
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        HnswReader {
            tx,
            ks: self.ks.clone().unwrap(),
            metric: self.metric,
            seed: self.seed,
            state: Rc::clone(&self.state),
        }
    }
}

/// Read handle for [`Hnsw`], pinned to one snapshot.
pub struct HnswReader<
    'tx,
    R: Readable,
    K,
    T,
    M: Metric<T>,
    const DIM: usize,
    const M0: usize,
    const TOP_K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    metric: M,
    seed: u64,
    state: Rc<RefCell<State<K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>>>,
}

impl<
    'tx,
    R,
    K,
    T,
    M,
    const DIM: usize,
    const M0: usize,
    const TOP_K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> HnswReader<'tx, R, K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>
where
    R: Readable,
    K: Clone + DeserializeOwned,
    T: Scalar + DeserializeOwned,
    M: Metric<T> + Copy,
{
    fn with_state<Ret>(
        &self,
        f: impl FnOnce(&mut State<K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>) -> Ret,
    ) -> Ret {
        let mut state = self.state.borrow_mut();
        if state.stale {
            let entries = self.tx.iter(&self.ks).map(|kv| {
                let (k, v) = kv.into_inner().unwrap();
                (k.to_vec(), v.to_vec())
            });
            state.rebuild(self.metric, self.seed, entries);
        }
        f(&mut state)
    }

    /// The up-to-`TOP_K` approximately nearest keys to `q`, ascending by
    /// distance (smaller is closer).
    pub fn search(&self, q: &[T; DIM]) -> Vec<Scored<M::Out, K>> {
        self.with_state(|state| {
            state
                .index
                .search(&q[..])
                .into_iter()
                .map(|(d, id)| Scored::new(d, state.keys[&id].clone()))
                .collect()
        })
    }

    /// The number of live embeddings.
    pub fn len(&self) -> usize {
        self.with_state(|state| state.ids.len())
    }

    /// Whether the index holds no embeddings.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
