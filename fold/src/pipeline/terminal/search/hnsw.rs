use std::{
    cell::RefCell,
    io::{Read, Write},
    path::{Path, PathBuf},
    rc::Rc,
};

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

// graph snapshot blob: this header + the key->node-id table, then the anny
// graph via Hnsw::write_to
const SNAP_MAGIC: [u8; 8] = *b"FOLDHNSW";
const SNAP_VERSION: u32 = 1;

fn bad_snap() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "bad hnsw graph snapshot")
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
/// transaction deltas accumulate and the net sign decides — positive
/// (re)indexes the key under its latest embedding, non-positive deletes it —
/// with no read of prior state. Retraction genuinely removes the node from
/// the graph (anny repairs the neighborhood), so recall does not decay
/// under churn the way tombstoning indexes do.
///
/// The graph lives in memory: it is rebuilt from the persisted vectors when
/// the stream opens, and again if a transaction aborts after a mid-tx flush
/// (a panic cannot un-mutate the graph, so it is marked stale and rebuilt
/// from committed state on next use). Under fold's single-writer discipline
/// readers otherwise always observe a graph consistent with their snapshot.
/// [`with_graph_snapshot`](Hnsw::with_graph_snapshot) plus
/// [`HnswReader::save_graph`] lets reopening skip the rebuild by loading a
/// serialized graph, validated against the committed rows.
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
    snapshot_path: Option<PathBuf>,
    state: Rc<RefCell<State<K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>>>,
    // encoded key -> (key, latest embedding, net delta this tx)
    pending: FxHashMap<Vec<u8>, (K, [T; DIM], i64)>,
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
            snapshot_path: None,
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

    /// Persist/restore the in-memory graph at `path`. Save via
    /// [`HnswReader::save_graph`] after your last commit; on reopen a blob
    /// that exactly matches the committed rows is adopted instead of
    /// rebuilding the graph, and anything else silently falls back to the
    /// rebuild.
    pub fn with_graph_snapshot(mut self, path: impl Into<PathBuf>) -> Self {
        self.snapshot_path = Some(path.into());
        self
    }

    // blob layout: [SNAP_MAGIC, version u32, count u32,
    // (klen u32, kenc, node id u32)*] then the anny graph
    fn load_blob(
        path: &Path,
        metric: M,
        seed: u64,
    ) -> std::io::Result<(
        FxHashMap<Vec<u8>, u32>,
        anny::hnsw::Hnsw<T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>,
    )> {
        let mut r = std::io::BufReader::new(std::fs::File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        let mut b4 = [0u8; 4];
        r.read_exact(&mut b4)?;
        if magic != SNAP_MAGIC || u32::from_le_bytes(b4) != SNAP_VERSION {
            return Err(bad_snap());
        }
        r.read_exact(&mut b4)?;
        let count = u32::from_le_bytes(b4) as usize;
        let mut ids = FxHashMap::default();
        ids.reserve(count);
        for _ in 0..count {
            r.read_exact(&mut b4)?;
            let mut kenc = vec![0u8; u32::from_le_bytes(b4) as usize];
            r.read_exact(&mut kenc)?;
            r.read_exact(&mut b4)?;
            if ids.insert(kenc, u32::from_le_bytes(b4)).is_some() {
                return Err(bad_snap());
            }
        }
        let index = anny::hnsw::Hnsw::read_from(&mut r, metric, seed)?;
        Ok((ids, index))
    }

    // fast path: adopt the snapshot blob iff it exactly covers the committed
    // rows (every row key mapped, counts equal); any failure means the
    // caller must rebuild from the rows instead
    fn try_restore(
        &mut self,
        init: &PipelineInitCtx<'_>,
        ks: &fjall::SingleWriterTxKeyspace,
    ) -> bool
    where
        K: DeserializeOwned,
        M: Copy,
    {
        let Some(path) = &self.snapshot_path else {
            return false;
        };
        let Ok((ids, index)) = Self::load_blob(path, self.metric, self.seed) else {
            return false;
        };
        if index.len() != ids.len() {
            return false;
        }
        let mut rows = 0usize;
        for kv in init.snapshot().iter(ks) {
            let Ok((k, _)) = kv.into_inner() else {
                return false;
            };
            if !ids.contains_key(&*k) {
                return false;
            }
            rows += 1;
        }
        if rows != ids.len() {
            return false;
        }
        let mut keys = FxHashMap::default();
        for (kenc, &id) in &ids {
            let Ok(key) = postcard::from_bytes::<K>(kenc) else {
                return false;
            };
            keys.insert(id, key);
        }
        let mut state = self.state.borrow_mut();
        state.index = index;
        state.ids = ids;
        state.keys = keys;
        state.stale = false;
        true
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
        // prefer a validated graph snapshot; otherwise recover the graph
        // from the vectors persisted by earlier runs
        if !self.try_restore(init, &ks) {
            self.state.borrow_mut().rebuild(
                self.metric,
                self.seed,
                init.snapshot().iter(&ks).map(|kv| {
                    let (k, v) = kv.into_inner().unwrap();
                    (k.to_vec(), v.to_vec())
                }),
            );
        }
        self.ks = Some(ks);
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, [T; DIM]>, delta: isize) {
        tx.buf.clear();
        postcard::to_io(&data.key, &mut tx.buf).unwrap();
        let e = self
            .pending
            .entry(tx.buf.clone())
            .or_insert_with(|| (data.key.clone(), data.val, 0));
        e.1 = data.val;
        e.2 += delta as i64;
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
                0 => {}
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
            snapshot_path: self.snapshot_path.clone(),
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
    snapshot_path: Option<PathBuf>,
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

    /// Write the graph (and its key mapping) to the configured snapshot
    /// path, via a `.tmp` sibling and rename for atomicity. Goes through
    /// [`with_state`](Self::with_state), so a stale graph resyncs from this
    /// reader's committed rows before it is captured. Errors with
    /// [`std::io::ErrorKind::InvalidInput`] if the sink was built without
    /// [`Hnsw::with_graph_snapshot`].
    pub fn save_graph(&self) -> std::io::Result<()> {
        let path = self.snapshot_path.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "no graph snapshot path configured",
            )
        })?;
        self.with_state(|state| {
            let mut tmp = path.clone().into_os_string();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            w.write_all(&SNAP_MAGIC)?;
            w.write_all(&SNAP_VERSION.to_le_bytes())?;
            let count = u32::try_from(state.ids.len()).map_err(|_| bad_snap())?;
            w.write_all(&count.to_le_bytes())?;
            for (kenc, id) in &state.ids {
                let klen = u32::try_from(kenc.len()).map_err(|_| bad_snap())?;
                w.write_all(&klen.to_le_bytes())?;
                w.write_all(kenc)?;
                w.write_all(&id.to_le_bytes())?;
            }
            state.index.write_to(&mut w)?;
            w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            std::fs::rename(&tmp, path)
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
