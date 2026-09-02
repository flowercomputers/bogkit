//! Ranked search sinks: full-text relevance ([`Bm25`]) and vector
//! nearest-neighbor ([`Hnsw`]).

use std::{cell::RefCell, collections::hash_map::Entry, marker::PhantomData};

use fjall::Readable;
use fxhash::FxHashMap;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    pipeline::{Keyed, Push, Scored},
    stream::{PipelineInitCtx, WriteTx},
};

mod flat;
mod hnsw;
pub use flat::*;
pub use hnsw::*;

/// Default tokenizer: split on whitespace, strip non-ASCII-alphanumerics,
/// lowercase. Appends each nonempty token to `tokens` terminated by `\0`
/// (see [`Bm25::with_tokenizer`] for the buffer contract).
pub fn tokenize(text: &str, tokens: &mut Vec<u8>) {
    tokens.clear();
    for tok in text.split_whitespace() {
        let start = tokens.len();
        tokens.extend(
            tok.bytes()
                .filter(u8::is_ascii_alphanumeric)
                .map(|b| b.to_ascii_lowercase()),
        );
        if tokens.len() > start {
            tokens.push(0);
        }
    }
}

// Keyspace layout, discriminated by a leading tag byte:
//   [STATS]                              -> n_docs i64 ++ total_len i64
//   [DOCLEN] postcard(K)                 -> token count i64
//   [POSTING] postcard(term) postcard(K) -> term frequency i64
// No separator is needed after the term: postcard string encodings are
// prefix-free (varint length, then exactly that many bytes), so a prefix
// scan of `[POSTING] postcard(term)` matches only that term's postings.
const STATS: u8 = 0;
const DOCLEN: u8 = 1;
const POSTING: u8 = 2;

/// Persistent BM25 index over [`Keyed`]`<K, V>` documents: rank keys `K` by
/// Okapi BM25 relevance to a free-text query.
///
/// Accepts `Keyed { key: document_key, val: text }` and tokenizes the text
/// on ingest (see [`tokenize`] for the default; swap it via
/// [`with_tokenizer`](Bm25::with_tokenizer)), maintaining postings with term
/// frequencies, per-document lengths, and corpus statistics. Queries are
/// tokenized the same way by [`Bm25Reader::search`], which scores with the
/// Lucene-style non-negative IDF `ln(1 + (N - df + 0.5) / (df + 0.5))`.
///
/// Like [`InvertedIndex`](super::InvertedIndex), documents are set-semantic:
/// within a transaction deltas accumulate, and the net sign decides — a
/// positive delta (re)writes the document's postings, a non-positive one
/// deletes them, without reading prior state. Corpus statistics do
/// accumulate deltas, so insert each document once with delta `+1` and
/// retract it once with `-1`. Like [`Map`](crate::pipeline::Map), this
/// relies on determinism: a retraction must present the same `(key, text)`
/// that was inserted, and the tokenizer must be a pure function, or index
/// state will not cancel.
///
/// ```no_run
/// use fold::pipeline::{Keyed, terminal::search::Bm25};
/// use fold::stream::Stream;
///
/// let mut st = Stream::new("docs.db", Bm25::new("idx"));
/// st.wtx(|tx| tx.insert(&Keyed::new(1u32, "a quick brown fox".to_string())));
/// st.rtx(|idx| {
///     for hit in idx.search("quick fox", 10) {
///         println!("{}: {}", hit.val, hit.score);
///     }
/// });
/// ```
pub struct Bm25<K, V, T = fn(&str, &mut Vec<u8>)> {
    name: String,
    ks: Option<fjall::SingleWriterTxKeyspace>,
    tok: T,
    tokens: Vec<u8>,
    k1: f64,
    b: f64,
    // per-document resolution of this transaction's pushes, keyed by the
    // doclen store key; posting writes are decided per document at commit
    pending: FxHashMap<Vec<u8>, DocPending>,
    docs: i64,
    len: i64,
    _p: PhantomData<(K, V)>,
}

impl<K, V> Bm25<K, V> {
    /// Index with the default [`tokenize`]r. `name` identifies this sink's
    /// keyspace and must be unique among all named nodes in the pipeline.
    pub fn new(name: impl Into<String>) -> Self {
        Self::with_tokenizer(name, tokenize)
    }
}

impl<K, V, T> Bm25<K, V, T> {
    /// Index with a custom tokenizer, applied to documents on ingest and to
    /// queries in [`Bm25Reader::search`]. The tokenizer clears the buffer,
    /// then appends each token's bytes terminated by `\0`; tokens must be
    /// nonempty and must not contain `\0`. Must be a pure function — see
    /// the type-level determinism note.
    pub fn with_tokenizer(name: impl Into<String>, tok: T) -> Self {
        Bm25 {
            name: name.into(),
            ks: None,
            tok,
            tokens: Vec::default(),
            k1: 1.2,
            b: 0.75,
            pending: FxHashMap::default(),
            docs: 0,
            len: 0,
            _p: PhantomData,
        }
    }

    /// Set the BM25 free parameters: `k1` bounds term-frequency saturation,
    /// `b` scales document-length normalization. Defaults `1.2` / `0.75`.
    pub fn params(mut self, k1: f64, b: f64) -> Self {
        self.k1 = k1;
        self.b = b;
        self
    }
}

/// One document's pushes within a transaction, resolved at commit with no
/// store reads. A same-transaction replacement (retract old text, insert
/// new) must delete exactly the old terms and write the new term
/// frequencies absolutely — accumulating per-term net deltas corrupts every
/// term the two texts share.
#[derive(Default)]
struct DocPending {
    net: i64,
    last_was_positive: bool,
    /// full posting keys -> term frequency, from the latest positive push
    added: FxHashMap<Vec<u8>, i64>,
    added_dl: i64,
    /// posting keys named by retractions (frequencies irrelevant)
    removed: FxHashMap<Vec<u8>, i64>,
}

impl<K, V, T> Push<Keyed<K, V>> for Bm25<K, V, T>
where
    K: Clone + Serialize + DeserializeOwned,
    V: Clone + AsRef<str>,
    T: Fn(&str, &mut Vec<u8>) + Clone,
{
    type Reader<'tx, R: Readable + 'tx> = Bm25Reader<'tx, R, K, T>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.ks = Some(init.keyspace(&self.name));
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Keyed<K, V>, delta: isize) {
        let Keyed { key, val } = data;
        let delta = delta as i64;
        (self.tok)(val.as_ref(), &mut self.tokens);

        let mut dl = 0i64;
        let mut tf: FxHashMap<&[u8], i64> = FxHashMap::default();
        for t in self.tokens.split(|&b| b == 0) {
            if t.is_empty() {
                continue;
            }
            *tf.entry(t).or_insert(0) += 1;
            dl += 1;
        }

        let mut keys: FxHashMap<Vec<u8>, i64> = FxHashMap::default();
        for (term, n) in tf {
            tx.buf.clear();
            tx.buf.push(POSTING);
            postcard::to_io(term, &mut tx.buf).unwrap();
            postcard::to_io(key, &mut tx.buf).unwrap();
            keys.insert(tx.buf.clone(), n);
        }

        tx.buf.clear();
        tx.buf.push(DOCLEN);
        postcard::to_io(key, &mut tx.buf).unwrap();
        let e = self.pending.entry(tx.buf.clone()).or_default();
        e.net += delta;
        e.last_was_positive = delta > 0;
        if delta > 0 {
            e.added = keys;
            e.added_dl = dl;
        } else {
            e.removed.extend(keys);
        }

        self.docs += delta;
        self.len += dl * delta;
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        if self.pending.is_empty() {
            return;
        }
        let ks = self.ks.clone().unwrap();
        for (dl_key, p) in self.pending.drain() {
            // same per-key discipline as the Hnsw sink: net > 0 or a
            // trailing positive push means the document lives under its
            // latest text; net < 0 deletes it; a cancelled insert is a no-op
            if p.net > 0 || (p.net == 0 && p.last_was_positive) {
                for k in p.removed.keys() {
                    if !p.added.contains_key(k) {
                        tx.remove(&ks, k);
                    }
                }
                for (k, tf) in &p.added {
                    tx.insert(&ks, k, tf.to_be_bytes());
                }
                tx.insert(&ks, &dl_key, p.added_dl.to_be_bytes());
            } else if p.net < 0 {
                for k in p.removed.keys().chain(p.added.keys()) {
                    tx.remove(&ks, k);
                }
                tx.remove(&ks, &dl_key);
            }
        }

        let (mut n, mut l) = tx
            .get(&ks, [STATS])
            .map(|v| {
                let v = v.as_ref();
                (
                    i64::from_be_bytes(v[..8].try_into().unwrap()),
                    i64::from_be_bytes(v[8..].try_into().unwrap()),
                )
            })
            .unwrap_or((0, 0));
        n += self.docs;
        l += self.len;
        if n != 0 || l != 0 {
            let mut v = [0u8; 16];
            v[..8].copy_from_slice(&n.to_be_bytes());
            v[8..].copy_from_slice(&l.to_be_bytes());
            tx.insert(&ks, [STATS], v);
        } else {
            tx.remove(&ks, [STATS]);
        }
        self.docs = 0;
        self.len = 0;
    }

    fn abort(&mut self) {
        self.pending.clear();
        self.docs = 0;
        self.len = 0;
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        Bm25Reader {
            tx,
            ks: self.ks.clone().unwrap(),
            tok: self.tok.clone(),
            k1: self.k1,
            b: self.b,
            _p: PhantomData,
        }
    }
}

/// Read handle for [`Bm25`], pinned to one snapshot.
pub struct Bm25Reader<'tx, R: Readable, K, T> {
    tx: &'tx R,
    ks: fjall::SingleWriterTxKeyspace,
    tok: T,
    k1: f64,
    b: f64,
    _p: PhantomData<K>,
}

impl<'tx, R: Readable, K: DeserializeOwned, T: Fn(&str, &mut Vec<u8>)> Bm25Reader<'tx, R, K, T> {
    fn stats(&self) -> (i64, i64) {
        self.tx
            .get(&self.ks, [STATS])
            .unwrap()
            .map(|v| {
                let v = v.as_ref();
                (
                    i64::from_be_bytes(v[..8].try_into().unwrap()),
                    i64::from_be_bytes(v[8..].try_into().unwrap()),
                )
            })
            .unwrap_or((0, 0))
    }

    /// The number of live documents.
    pub fn doc_count(&self) -> i64 {
        self.stats().0
    }

    /// The top `limit` document keys by BM25 relevance to `query`, scored
    /// descending. The query is tokenized with the index's tokenizer;
    /// duplicate query terms count once. Documents matching no term are
    /// omitted, so fewer than `limit` hits may return.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Scored<f64, K>> {
        // per-thread scratch reused across calls: terms are overwritten by
        // the tokenizer, postings drained per term, docs drained per call
        #[derive(Default)]
        struct Bufs {
            terms: Vec<u8>,
            key: Vec<u8>,
            postings: Vec<(Vec<u8>, i64)>,
            // encoded doc key -> (accumulated score, cached doc length)
            docs: FxHashMap<Vec<u8>, (f64, f64)>,
        }
        thread_local! {
            static BUFS: RefCell<Bufs> = RefCell::new(Bufs::default());
        }

        let (n, total_len) = self.stats();
        if n <= 0 {
            return Vec::new();
        }
        let avgdl = total_len as f64 / n as f64;

        BUFS.with_borrow_mut(|bufs| {
            (self.tok)(query, &mut bufs.terms);

            for (i, term) in bufs.terms.split(|&b| b == 0).enumerate() {
                // count duplicate query terms once: queries are short, so a
                // rescan of prior terms beats sorting or hashing
                if term.is_empty() || bufs.terms.split(|&b| b == 0).take(i).any(|p| p == term) {
                    continue;
                }
                bufs.key.clear();
                bufs.key.push(POSTING);
                postcard::to_io(term, &mut bufs.key).unwrap();
                let plen = bufs.key.len();
                bufs.postings
                    .extend(self.tx.prefix(&self.ks, &bufs.key[..]).map(|kv| {
                        let (key, val) = kv.into_inner().unwrap();
                        (
                            key[plen..].to_vec(),
                            i64::from_be_bytes(*val.as_array::<8>().unwrap()),
                        )
                    }));
                if bufs.postings.is_empty() {
                    continue;
                }
                let df = bufs.postings.len() as f64;
                let idf = ((n as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();
                for (kenc, tf) in bufs.postings.drain(..) {
                    // the posting key's allocation moves into the map, which
                    // doubles as the doc-length cache across terms
                    let (score, dl) = match bufs.docs.entry(kenc) {
                        Entry::Occupied(e) => e.into_mut(),
                        Entry::Vacant(e) => {
                            bufs.key.clear();
                            bufs.key.push(DOCLEN);
                            bufs.key.extend_from_slice(e.key());
                            let dl = self
                                .tx
                                .get(&self.ks, &bufs.key)
                                .unwrap()
                                .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()) as f64)
                                .unwrap_or(0.0);
                            e.insert((0.0, dl))
                        }
                    };
                    let tf = tf as f64;
                    let norm = self.k1 * (1.0 - self.b + self.b * *dl / avgdl);
                    *score += idf * tf * (self.k1 + 1.0) / (tf + norm);
                }
            }

            let mut hits: Vec<Scored<f64, K>> = bufs
                .docs
                .drain()
                .map(|(kenc, (score, _))| Scored::new(score, postcard::from_bytes(&kenc).unwrap()))
                .collect();
            hits.sort_by(|a, b| b.score.total_cmp(&a.score));
            hits.truncate(limit);
            hits
        })
    }
}
