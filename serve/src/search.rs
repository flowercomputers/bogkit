//! [`Views`] for the search sinks, and the [`TextQuery`] wrapper that adds
//! query-time text encoding to a vector index.
//!
//! BM25 is text-in, text-searched — its `Views` impl is direct. HNSW is
//! vector-searched, and the text→vector mapping lives in a user `Map`
//! closure *upstream* of the sink, where no generic layer can see it. So
//! text search over HNSW is opt-in: wrap the sink in
//! [`TextQuery::new(hnsw, encoder)`](TextQuery), handing the server the
//! same encoder the pipeline uses. Without the wrapper the view still
//! serves raw-vector searches (`POST /views/{name}/search`).

use fold::pipeline::terminal::search::{Bm25Reader, HnswReader};
use fold::pipeline::{Push, Scored};
use fold::stream::{PipelineInitCtx, Readable, WriteTx};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::views::{SearchMode, ViewKind, ViewQuery, ViewRead, ViewSpec, Views, schema_of, spec};

/// One search hit: `{ "score": <number>, "key": <K> }`.
fn hit_schema(key_schema: Value) -> Value {
    json!({
        "type": "object",
        "properties": {
            "score": { "type": "number" },
            "key": key_schema,
        },
        "required": ["score", "key"],
    })
}

fn hits_json<S: Serialize, K: Serialize>(hits: &[Scored<S, K>], k: usize) -> Value {
    Value::Array(
        hits.iter()
            .take(k)
            .map(|h| json!({ "score": h.score, "key": h.val }))
            .collect(),
    )
}

impl<R, K, T> Views for Bm25Reader<'_, R, K, T>
where
    R: Readable,
    K: Serialize + DeserializeOwned + JsonSchema,
    T: Fn(&str, &mut Vec<u8>),
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Bm25,
            false,
            Some(SearchMode::Text),
            hit_schema(schema_of::<K>()),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search {
                q: Some(text), k, ..
            } => ViewRead::Data(hits_json(&self.search(text, *k), *k)),
            ViewQuery::Search {
                vector: Some(_), ..
            } => ViewRead::BadRequest("bm25 searches text: GET .../search?q=...".into()),
            ViewQuery::Search { .. } => ViewRead::BadRequest("q required".into()),
            _ => ViewRead::BadRequest(format!(
                "this view is searched: GET /views/{view}/search?q=..."
            )),
        }
    }
}

/// Typed vector search behind a JSON boundary — the shared surface between
/// a bare [`HnswReader`] and one wrapped in [`TextQuery`]. `Query` is the
/// embedding type (`[T; DIM]`), so a wrapper's encoder output is checked
/// against the index it feeds at compile time.
pub trait VectorSearch {
    type Query;

    fn name(&self) -> &str;
    /// Parse a JSON value (`[0.1, 0.2, ...]`) as a query vector.
    fn parse(&self, v: &Value) -> Result<Self::Query, String>;
    /// Up to `k` nearest hits, ascending by distance.
    fn hits(&self, q: &Self::Query, k: usize) -> Value;
    fn key_schema(&self) -> Value;
}

impl<
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
> VectorSearch for HnswReader<'_, R, K, T, M, DIM, M0, TOP_K, EF_SEARCH, EF_BUILD, MAX_LEVEL>
where
    R: Readable,
    K: Clone + Serialize + DeserializeOwned + JsonSchema,
    T: anny::metric::Scalar + Copy + DeserializeOwned,
    M: anny::metric::Metric<T> + Copy,
    M::Out: Serialize,
{
    type Query = [T; DIM];

    fn name(&self) -> &str {
        self.name()
    }

    fn parse(&self, v: &Value) -> Result<[T; DIM], String> {
        let vec: Vec<T> = serde_json::from_value(v.clone())
            .map_err(|e| format!("vector must be an array of numbers: {e}"))?;
        let got = vec.len();
        <[T; DIM]>::try_from(vec).map_err(|_| format!("vector must have {DIM} dims, got {got}"))
    }

    fn hits(&self, q: &[T; DIM], k: usize) -> Value {
        hits_json(&self.search(q), k)
    }

    fn key_schema(&self) -> Value {
        schema_of::<K>()
    }
}

/// Serve a search request against any [`VectorSearch`] index, with an
/// optional text encoder for `?q=` queries.
fn vector_read<S: VectorSearch>(
    index: &S,
    encoder: Option<fn(&str) -> S::Query>,
    view: &str,
    q: &ViewQuery,
) -> ViewRead {
    if view != index.name() {
        return ViewRead::NotFound;
    }
    match q {
        ViewQuery::Search {
            q: Some(text), k, ..
        } => match encoder {
            Some(encode) => ViewRead::Data(index.hits(&encode(text), *k)),
            None => ViewRead::BadRequest(
                "this view searches by vector: POST {\"vector\": [...]} — or wrap the sink \
                 in TextQuery to enable ?q="
                    .into(),
            ),
        },
        ViewQuery::Search {
            vector: Some(raw),
            k,
            ..
        } => match index.parse(raw) {
            Ok(vec) => ViewRead::Data(index.hits(&vec, *k)),
            Err(msg) => ViewRead::BadRequest(msg),
        },
        ViewQuery::Search { .. } => ViewRead::BadRequest("a q or vector query is required".into()),
        _ => ViewRead::BadRequest(format!("this view is searched: POST /views/{view}/search")),
    }
}

impl<I: VectorSearch> Views for I {
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Hnsw,
            false,
            Some(SearchMode::Vector),
            hit_schema(self.key_schema()),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        vector_read(self, None, view, q)
    }
}

/// Wrap a vector sink with the encoder that turns query text into an
/// embedding, enabling `GET /views/{name}/search?q=...`.
///
/// The encoder is a plain `fn` pointer (e.g. `|q| ese::encode_single(q)`),
/// so it must not capture state — the same purity fold requires of the
/// pipeline's own embedding `Map`. Pass the *same* encoding both places or
/// query vectors won't live in the document vector space.
///
/// Transparent to the pipeline: pushes, commits, and retraction all
/// delegate to the wrapped sink.
pub struct TextQuery<S, E> {
    inner: S,
    encoder: fn(&str) -> E,
}

impl<S, E> TextQuery<S, E> {
    pub fn new(inner: S, encoder: fn(&str) -> E) -> Self {
        TextQuery { inner, encoder }
    }
}

impl<D: Clone, S: Push<D>, E> Push<D> for TextQuery<S, E> {
    type Reader<'tx, R: Readable + 'tx> = TextQueryReader<S::Reader<'tx, R>, E>;

    fn init(&mut self, init: &mut PipelineInitCtx<'_>) {
        self.inner.init(init);
    }

    fn push(&mut self, tx: &mut WriteTx<'_>, data: &D, delta: isize) {
        self.inner.push(tx, data, delta);
    }

    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        self.inner.commit(tx);
    }

    fn abort(&mut self) {
        self.inner.abort();
    }

    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        TextQueryReader {
            inner: self.inner.reader(tx),
            encoder: self.encoder,
        }
    }
}

/// Read handle for [`TextQuery`]: the wrapped reader plus the encoder.
pub struct TextQueryReader<I, E> {
    inner: I,
    encoder: fn(&str) -> E,
}

impl<I, E> TextQueryReader<I, E> {
    /// The wrapped reader, for custom routes that search directly.
    pub fn inner(&self) -> &I {
        &self.inner
    }

    /// Encode query text the way this view's searches do.
    pub fn encode(&self, text: &str) -> E {
        (self.encoder)(text)
    }
}

impl<I, E> Views for TextQueryReader<I, E>
where
    I: VectorSearch<Query = E>,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.inner.name(),
            ViewKind::Hnsw,
            false,
            Some(SearchMode::TextAndVector),
            hit_schema(self.inner.key_schema()),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        vector_read(&self.inner, Some(self.encoder), view, q)
    }
}
