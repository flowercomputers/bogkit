//! Generic axum handlers over any served pipeline.
//!
//! Reads are identical for plain and keyed streams, so they're generic over
//! [`ViewSource`]. Writes differ by stream flavor (raw insert/remove vs
//! upsert/remove-by-key) and stay per-kind.

use std::convert::Infallible;
use std::sync::{Arc, RwLock};

use axum::body::Bytes;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use fold::fjall::Snapshot;
use fold::pipeline::{Keyed, Push};
use fold::stream::{KeyedStream, Stream};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_stream::StreamExt;

use crate::openapi::{CustomDoc, WriteStyle};
use crate::to_value;
use crate::views::{ViewQuery, ViewRead, Views, parse_key, schema_of};

const DEFAULT_LIMIT: usize = 100;
const DEFAULT_K: usize = 10;

/// Read access shared by every view route, implemented per stream flavor.
trait Rtx: Send + Sync + 'static {
    type Reader<'tx>: Views
    where
        Self: 'tx;

    fn rtx<R>(&self, f: impl for<'tx> FnOnce(Self::Reader<'tx>) -> R) -> R;

    /// Fsync all committed state; see [`fold::stream::Stream::checkpoint`].
    fn checkpoint(&mut self);

    /// Wipe all persisted state and re-initialize; see
    /// [`fold::stream::Stream::reset`].
    fn reset(&mut self);
}

trait CustomRead<D, P>: Rtx
where
    D: Clone + 'static,
    P: Push<D> + 'static,
{
    fn custom_read(&self, h: &ReadHandler<D, P>, query: &str) -> Result<Value, (u16, String)>;
}

impl<D, P> Rtx for Stream<D, P>
where
    D: Clone + Send + Sync + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    type Reader<'tx> = P::Reader<'tx, Snapshot>;

    fn rtx<R>(&self, f: impl for<'tx> FnOnce(Self::Reader<'tx>) -> R) -> R {
        Stream::rtx(self, f)
    }

    fn checkpoint(&mut self) {
        Stream::checkpoint(self)
    }

    fn reset(&mut self) {
        Stream::reset(self)
    }
}

impl<D, P> CustomRead<D, P> for Stream<D, P>
where
    D: Clone + Send + Sync + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    fn custom_read(&self, h: &ReadHandler<D, P>, query: &str) -> Result<Value, (u16, String)> {
        Stream::rtx(self, |r| h(r, query))
    }
}

impl<K, V, P> Rtx for KeyedStream<K, V, P>
where
    K: Clone + Send + Sync + Serialize + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    type Reader<'tx> = P::Reader<'tx, Snapshot>;

    fn rtx<R>(&self, f: impl for<'tx> FnOnce(Self::Reader<'tx>) -> R) -> R {
        KeyedStream::rtx(self, f)
    }

    fn checkpoint(&mut self) {
        KeyedStream::checkpoint(self)
    }

    fn reset(&mut self) {
        KeyedStream::reset(self)
    }
}

impl<K, V, P> CustomRead<Keyed<K, V>, P> for KeyedStream<K, V, P>
where
    K: Clone + Send + Sync + Serialize + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    fn custom_read(
        &self,
        h: &ReadHandler<Keyed<K, V>, P>,
        query: &str,
    ) -> Result<Value, (u16, String)> {
        KeyedStream::rtx(self, |r| h(r, query))
    }
}

pub(crate) type ReadHandler<D, P> = Arc<
    dyn for<'tx> Fn(<P as Push<D>>::Reader<'tx, Snapshot>, &str) -> Result<Value, (u16, String)>
        + Send
        + Sync,
>;

pub(crate) type WriteHandler<S> =
    Arc<dyn Fn(&mut S, &[u8]) -> Result<Value, (u16, String)> + Send + Sync>;

pub(crate) struct CustomRoute<D: Clone, P: Push<D>, S> {
    pub doc: CustomDoc,
    pub action: CustomAction<D, P, S>,
}

pub(crate) enum CustomAction<D: Clone, P: Push<D>, S> {
    Read(ReadHandler<D, P>),
    Write(WriteHandler<S>),
}

pub(crate) type StreamCustom<D, P> = CustomRoute<D, P, Stream<D, P>>;
pub(crate) type KeyedCustom<K, V, P> = CustomRoute<Keyed<K, V>, P, KeyedStream<K, V, P>>;

pub(crate) fn custom_get<D, P, S, Q, T, F>(path: String, handler: F) -> CustomRoute<D, P, S>
where
    D: Clone,
    P: Push<D> + 'static,
    Q: DeserializeOwned + JsonSchema,
    T: Serialize + JsonSchema,
    F: for<'tx> Fn(P::Reader<'tx, Snapshot>, Q) -> Result<T, (u16, String)> + Send + Sync + 'static,
{
    CustomRoute {
        doc: CustomDoc {
            path,
            method: "get",
            params: Some(schema_of::<Q>()),
            body: None,
            response: schema_of::<T>(),
        },
        action: CustomAction::Read(Arc::new(move |readers, raw_query| {
            let q: Q = serde_urlencoded::from_str(raw_query)
                .map_err(|e| (400, format!("invalid query: {e}")))?;
            handler(readers, q).map(to_value)
        })),
    }
}

pub(crate) fn custom_write<D, P, S, B, T, F>(path: String, run: F) -> CustomRoute<D, P, S>
where
    D: Clone,
    P: Push<D>,
    B: DeserializeOwned + JsonSchema,
    T: Serialize + JsonSchema,
    F: Fn(&mut S, B) -> Result<T, (u16, String)> + Send + Sync + 'static,
{
    CustomRoute {
        doc: CustomDoc {
            path,
            method: "post",
            params: None,
            body: Some(schema_of::<B>()),
            response: schema_of::<T>(),
        },
        action: CustomAction::Write(Arc::new(move |stream, raw| {
            let body: B =
                serde_json::from_slice(raw).map_err(|e| (400, format!("invalid body: {e}")))?;
            run(stream, body).map(to_value)
        })),
    }
}

struct Docs {
    openapi: Value,
    schema: Value,
}

trait ViewSource: Send + Sync + 'static {
    fn view(&self, view: &str, q: &ViewQuery) -> (u64, ViewRead);
    fn docs(&self) -> &Docs;
    fn subscribe(&self) -> watch::Receiver<u64>;
}

struct Shared<S> {
    inner: RwLock<Inner<S>>,
    docs: Docs,
    notify: watch::Sender<u64>,
}

struct Inner<S> {
    stream: S,
    seq: u64,
}

impl<S: Rtx> ViewSource for Shared<S> {
    fn view(&self, view: &str, q: &ViewQuery) -> (u64, ViewRead) {
        let inner = self.inner.read().unwrap();
        (inner.seq, inner.stream.rtx(|r| r.read(view, q)))
    }

    fn docs(&self) -> &Docs {
        &self.docs
    }

    fn subscribe(&self) -> watch::Receiver<u64> {
        self.notify.subscribe()
    }
}

/// Everything [`run`](crate::App::run) needs beyond the router itself:
/// hooks into the shared stream for the idle watchdog, type-erased so the
/// serve loop stays non-generic.
pub(crate) struct Lifecycle {
    /// The pipeline's schema fingerprint (also persisted in the `.schema`
    /// sidecar), for the discovery sidecar.
    pub fingerprint: String,
    /// Drains in-flight writes (takes the write lock) and fsyncs all
    /// committed state.
    pub checkpoint: Box<dyn FnOnce() + Send>,
    /// Live SSE subscriptions (`/watch`, `/views/{name}/watch`); the idle
    /// watchdog won't exit while any are connected.
    pub watchers: Box<dyn Fn() -> usize + Send + Sync>,
}

impl Lifecycle {
    fn new<S: Rtx>(shared: Arc<Shared<S>>, fingerprint: String) -> Self {
        let cp = shared.clone();
        Lifecycle {
            fingerprint,
            checkpoint: Box::new(move || cp.inner.write().unwrap().stream.checkpoint()),
            watchers: Box::new(move || shared.notify.receiver_count()),
        }
    }
}

fn shared_from<D, P, S: Rtx>(
    mut stream: S,
    custom: &[CustomRoute<D, P, S>],
    db_path: &std::path::Path,
    input_schema: &Value,
    style: WriteStyle<'_>,
    drift: crate::SchemaDrift,
) -> (Arc<Shared<S>>, String)
where
    D: Clone,
    P: Push<D>,
{
    let specs = stream.rtx(|r| {
        let mut specs = Vec::new();
        r.specs(&mut specs);
        specs
    });
    let custom_docs: Vec<_> = custom.iter().map(|c| c.doc.clone()).collect();
    let schema = crate::openapi::schema_doc(input_schema, &specs, style);
    let fingerprint = schema["fingerprint"].as_str().unwrap().to_string();
    check_fingerprint(db_path, &fingerprint, drift, &mut stream);
    let shared = Arc::new(Shared {
        docs: Docs {
            openapi: crate::openapi::openapi_doc(input_schema, &specs, style, &custom_docs),
            schema,
        },
        notify: watch::channel(0).0,
        inner: RwLock::new(Inner { stream, seq: 0 }),
    });
    (shared, fingerprint)
}

fn attach_custom<D, P, S>(
    mut app: Router,
    shared: Arc<Shared<S>>,
    custom: Vec<CustomRoute<D, P, S>>,
) -> Router
where
    D: Clone + 'static,
    P: Push<D> + 'static,
    S: Rtx + CustomRead<D, P>,
{
    for route in custom {
        let path = route.doc.path.clone();
        match route.action {
            CustomAction::Read(handler) => {
                let shared = shared.clone();
                app = app.route(
                    &path,
                    get(move |RawQuery(query): RawQuery| {
                        let shared = shared.clone();
                        let handler = handler.clone();
                        async move {
                            let inner = shared.inner.read().unwrap();
                            let seq = inner.seq;
                            let out = inner
                                .stream
                                .custom_read(&handler, query.as_deref().unwrap_or(""));
                            drop(inner);
                            respond_custom(seq, out)
                        }
                    }),
                );
            }
            CustomAction::Write(handler) => {
                let shared = shared.clone();
                app = app.route(
                    &path,
                    post(move |body: Bytes| {
                        let shared = shared.clone();
                        let handler = handler.clone();
                        async move {
                            let mut inner = shared.inner.write().unwrap();
                            match handler(&mut inner.stream, &body) {
                                Ok(data) => {
                                    inner.seq += 1;
                                    let seq = inner.seq;
                                    drop(inner);
                                    shared.notify.send_replace(seq);
                                    Json(json!({ "seq": seq, "data": data })).into_response()
                                }
                                Err((code, msg)) => {
                                    drop(inner);
                                    error(
                                        StatusCode::from_u16(code)
                                            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                                        msg,
                                    )
                                }
                            }
                        }
                    }),
                );
            }
        }
    }
    app
}

pub(crate) fn router<D, P>(
    stream: Stream<D, P>,
    custom: Vec<StreamCustom<D, P>>,
    db_path: &std::path::Path,
    drift: crate::SchemaDrift,
) -> (Router, Lifecycle)
where
    D: Clone + Send + Sync + DeserializeOwned + JsonSchema + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let input_schema = schema_of::<D>();
    let (shared, fingerprint) = shared_from::<D, P, _>(
        stream,
        &custom,
        db_path,
        &input_schema,
        WriteStyle::Unkeyed,
        drift,
    );
    let app = read_routes::<Shared<Stream<D, P>>>()
        .route("/insert", post(insert::<D, P>))
        .route("/remove", post(remove::<D, P>))
        .route("/batch", post(batch::<D, P>))
        .with_state(shared.clone());
    let lifecycle = Lifecycle::new(shared.clone(), fingerprint);
    (attach_custom(app, shared, custom), lifecycle)
}

pub(crate) fn router_keyed<K, V, P>(
    stream: KeyedStream<K, V, P>,
    custom: Vec<KeyedCustom<K, V, P>>,
    db_path: &std::path::Path,
    drift: crate::SchemaDrift,
) -> (Router, Lifecycle)
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + JsonSchema + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + JsonSchema + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let input_schema = schema_of::<V>();
    let key_schema = schema_of::<K>();
    let (shared, fingerprint) = shared_from::<Keyed<K, V>, P, _>(
        stream,
        &custom,
        db_path,
        &input_schema,
        WriteStyle::Keyed {
            key_schema: &key_schema,
        },
        drift,
    );
    let app = read_routes::<Shared<KeyedStream<K, V, P>>>()
        .route(
            "/docs/{key}",
            put(put_doc::<K, V, P>)
                .delete(delete_doc::<K, V, P>)
                .get(get_doc::<K, V, P>),
        )
        .route("/batch", post(batch_keyed::<K, V, P>))
        .with_state(shared.clone());
    let lifecycle = Lifecycle::new(shared.clone(), fingerprint);
    (attach_custom(app, shared, custom), lifecycle)
}

/// Run one write on `stream`, bump seq, notify `/watch`.
///
/// `send_replace`, not `send`: `send()` refuses to store when no client is
/// connected yet, and late subscribers must still see the latest seq.
fn commit<S>(shared: &Shared<S>, f: impl FnOnce(&mut S)) -> u64 {
    let mut inner = shared.inner.write().unwrap();
    f(&mut inner.stream);
    inner.seq += 1;
    let seq = inner.seq;
    drop(inner);
    shared.notify.send_replace(seq);
    seq
}

fn commit_with<S, R>(shared: &Shared<S>, f: impl FnOnce(&mut S) -> R) -> (u64, R) {
    let mut inner = shared.inner.write().unwrap();
    let out = f(&mut inner.stream);
    inner.seq += 1;
    let seq = inner.seq;
    drop(inner);
    shared.notify.send_replace(seq);
    (seq, out)
}

async fn insert<D, P>(
    State(shared): State<Arc<Shared<Stream<D, P>>>>,
    body: Result<Json<D>, JsonRejection>,
) -> Response
where
    D: Clone + Send + Sync + DeserializeOwned + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let data = match require_json(body) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let seq = commit(&shared, |stream| stream.wtx(|tx| tx.insert(&data)));
    Json(json!({ "seq": seq })).into_response()
}

async fn remove<D, P>(
    State(shared): State<Arc<Shared<Stream<D, P>>>>,
    body: Result<Json<D>, JsonRejection>,
) -> Response
where
    D: Clone + Send + Sync + DeserializeOwned + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let data = match require_json(body) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let seq = commit(&shared, |stream| stream.wtx(|tx| tx.remove(&data)));
    Json(json!({ "seq": seq })).into_response()
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Op<D> {
    Insert { data: D },
    Remove { data: D },
}

async fn batch<D, P>(
    State(shared): State<Arc<Shared<Stream<D, P>>>>,
    body: Result<Json<Vec<Op<D>>>, JsonRejection>,
) -> Response
where
    D: Clone + Send + Sync + DeserializeOwned + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let ops = match require_json(body) {
        Ok(ops) => ops,
        Err(resp) => return resp,
    };
    let applied = ops.len();
    let seq = commit(&shared, |stream| {
        stream.wtx(|tx| {
            for op in &ops {
                match op {
                    Op::Insert { data } => tx.insert(data),
                    Op::Remove { data } => tx.remove(data),
                }
            }
        })
    });
    Json(json!({ "seq": seq, "applied": applied })).into_response()
}

async fn put_doc<K, V, P>(
    State(shared): State<Arc<Shared<KeyedStream<K, V, P>>>>,
    Path(raw): Path<String>,
    body: Result<Json<V>, JsonRejection>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let data = match require_json(body) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let Some(key) = parse_key::<K>(&raw) else {
        return error(StatusCode::BAD_REQUEST, key_parse_msg(&raw));
    };
    let (seq, old) = commit_with(&shared, |stream| stream.wtx(|tx| tx.upsert(&key, &data)));
    Json(json!({ "seq": seq, "replaced": old.is_some() })).into_response()
}

async fn delete_doc<K, V, P>(
    State(shared): State<Arc<Shared<KeyedStream<K, V, P>>>>,
    Path(raw): Path<String>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let Some(key) = parse_key::<K>(&raw) else {
        return error(StatusCode::BAD_REQUEST, key_parse_msg(&raw));
    };
    let (seq, old) = commit_with(&shared, |stream| stream.wtx(|tx| tx.remove(&key)));
    Json(json!({ "seq": seq, "removed": old.is_some() })).into_response()
}

async fn get_doc<K, V, P>(
    State(shared): State<Arc<Shared<KeyedStream<K, V, P>>>>,
    Path(raw): Path<String>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let Some(key) = parse_key::<K>(&raw) else {
        return error(StatusCode::BAD_REQUEST, key_parse_msg(&raw));
    };
    let inner = shared.inner.read().unwrap();
    let seq = inner.seq;
    let record = inner.stream.get(&key);
    drop(inner);
    match record {
        Some(data) => Json(json!({ "seq": seq, "data": data })).into_response(),
        None => error(StatusCode::NOT_FOUND, "not found"),
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum KeyedOp<K, V> {
    Upsert { key: K, data: V },
    Remove { key: K },
}

async fn batch_keyed<K, V, P>(
    State(shared): State<Arc<Shared<KeyedStream<K, V, P>>>>,
    body: Result<Json<Vec<KeyedOp<K, V>>>, JsonRejection>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let ops = match require_json(body) {
        Ok(ops) => ops,
        Err(resp) => return resp,
    };
    let applied = ops.len();
    let (seq, ()) = commit_with(&shared, |stream| {
        stream.wtx(|tx| {
            for op in &ops {
                match op {
                    KeyedOp::Upsert { key, data } => {
                        tx.upsert(key, data);
                    }
                    KeyedOp::Remove { key } => {
                        tx.remove(key);
                    }
                }
            }
        })
    });
    Json(json!({ "seq": seq, "applied": applied })).into_response()
}

fn key_parse_msg(raw: &str) -> String {
    format!("cannot parse {raw:?} as this stream's key type")
}

fn read_routes<S: ViewSource>() -> Router<Arc<S>> {
    Router::new()
        .route("/healthz", get(async || "ok"))
        .route("/views/{name}", get(view_list::<S>))
        .route(
            "/views/{name}/search",
            get(search_get::<S>).post(search_post::<S>),
        )
        .route("/views/{name}/watch", get(watch_view::<S>))
        .route("/views/{name}/{key}", get(view_key::<S>))
        .route("/openapi.json", get(serve_openapi::<S>))
        .route("/schema", get(serve_schema::<S>))
        .route("/watch", get(watch_sse::<S>))
}

#[derive(Deserialize)]
struct Page {
    limit: Option<usize>,
    offset: Option<usize>,
    desc: Option<bool>,
}

impl Page {
    fn parts(&self) -> (usize, usize, bool) {
        (
            self.limit.unwrap_or(DEFAULT_LIMIT),
            self.offset.unwrap_or(0),
            self.desc.unwrap_or(false),
        )
    }

    fn query(&self) -> ViewQuery {
        let (limit, offset, desc) = self.parts();
        ViewQuery::list(limit, offset, desc)
    }
}

async fn view_list<S: ViewSource>(
    State(shared): State<Arc<S>>,
    Path(name): Path<String>,
    page: Result<Query<Page>, QueryRejection>,
) -> Response {
    let page = match require_query(page) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    respond(shared.view(&name, &page.query()))
}

async fn view_key<S: ViewSource>(
    State(shared): State<Arc<S>>,
    Path((name, key)): Path<(String, String)>,
    page: Result<Query<Page>, QueryRejection>,
) -> Response {
    let page = match require_query(page) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let (limit, offset, desc) = page.parts();
    respond(shared.view(&name, &ViewQuery::point_page(key, limit, offset, desc)))
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    k: Option<usize>,
}

async fn search_get<S: ViewSource>(
    State(shared): State<Arc<S>>,
    Path(name): Path<String>,
    params: Result<Query<SearchParams>, QueryRejection>,
) -> Response {
    let p = match require_query(params) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let q = ViewQuery::search(p.q, None, p.k.unwrap_or(DEFAULT_K));
    respond(shared.view(&name, &q))
}

#[derive(Deserialize)]
struct SearchBody {
    vector: Value,
    k: Option<usize>,
}

async fn search_post<S: ViewSource>(
    State(shared): State<Arc<S>>,
    Path(name): Path<String>,
    body: Result<Json<SearchBody>, JsonRejection>,
) -> Response {
    let body = match require_json(body) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let q = ViewQuery::search(None, Some(body.vector), body.k.unwrap_or(DEFAULT_K));
    respond(shared.view(&name, &q))
}

async fn watch_view<S: ViewSource>(
    State(shared): State<Arc<S>>,
    Path(name): Path<String>,
    page: Result<Query<Page>, QueryRejection>,
) -> Response {
    let page = match require_query(page) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    let q = page.query();
    match shared.view(&name, &q).1 {
        ViewRead::NotFound => return error(StatusCode::NOT_FOUND, "not found"),
        ViewRead::BadRequest(msg) => return error(StatusCode::BAD_REQUEST, msg),
        ViewRead::Data(_) => {}
    }

    let events = tokio_stream::wrappers::WatchStream::new(shared.subscribe()).map(move |_| {
        let (seq, out) = shared.view(&name, &q);
        let data = match out {
            ViewRead::Data(data) => data,
            _ => Value::Null,
        };
        Ok::<_, Infallible>(
            Event::default()
                .id(seq.to_string())
                .data(json!({ "seq": seq, "data": data }).to_string()),
        )
    });
    Sse::new(events)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn serve_openapi<S: ViewSource>(State(shared): State<Arc<S>>) -> Response {
    Json(shared.docs().openapi.clone()).into_response()
}

async fn serve_schema<S: ViewSource>(State(shared): State<Arc<S>>) -> Response {
    Json(shared.docs().schema.clone()).into_response()
}

async fn watch_sse<S: ViewSource>(State(shared): State<Arc<S>>) -> impl IntoResponse {
    let events = tokio_stream::wrappers::WatchStream::new(shared.subscribe()).map(|seq| {
        Ok::<_, Infallible>(
            Event::default()
                .id(seq.to_string())
                .data(json!({ "seq": seq }).to_string()),
        )
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

fn respond((seq, outcome): (u64, ViewRead)) -> Response {
    match outcome {
        ViewRead::Data(data) => Json(json!({ "seq": seq, "data": data })).into_response(),
        ViewRead::NotFound => error(StatusCode::NOT_FOUND, "not found"),
        ViewRead::BadRequest(msg) => error(StatusCode::BAD_REQUEST, msg),
    }
}

#[allow(clippy::result_large_err)]
fn require_json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    match body {
        Ok(Json(v)) => Ok(v),
        Err(rej) => Err(error(
            StatusCode::BAD_REQUEST,
            format!("invalid body: {}", rej.body_text()),
        )),
    }
}

#[allow(clippy::result_large_err)]
fn require_query<T>(query: Result<Query<T>, QueryRejection>) -> Result<T, Response> {
    match query {
        Ok(Query(v)) => Ok(v),
        Err(rej) => Err(error(
            StatusCode::BAD_REQUEST,
            format!("invalid query: {}", rej.body_text()),
        )),
    }
}

fn respond_custom(seq: u64, out: Result<Value, (u16, String)>) -> Response {
    match out {
        Ok(data) => Json(json!({ "seq": seq, "data": data })).into_response(),
        Err((code, msg)) => error(
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            msg,
        ),
    }
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

/// Guard a data dir against pipeline drift. The schema fingerprint is
/// persisted in a sidecar file (`<data dir>.schema`) on first open; a
/// mismatch on a later open means the pipeline's input type or sink
/// structure changed, and reads through the new pipeline could silently
/// misinterpret persisted state. What happens then is the caller's choice
/// ([`SchemaDrift`](crate::SchemaDrift)): refuse to start loudly, or wipe
/// the store and rebuild from empty.
fn check_fingerprint<S: Rtx>(
    db_path: &std::path::Path,
    fingerprint: &str,
    drift: crate::SchemaDrift,
    stream: &mut S,
) {
    let mut marker = db_path.as_os_str().to_owned();
    marker.push(".schema");

    match std::fs::read_to_string(&marker) {
        Ok(stored) if stored.trim() == fingerprint => return,
        Ok(stored) => match drift {
            crate::SchemaDrift::Panic => panic!(
                "pipeline changed since this data dir was written\n\
                 \n\
                 data dir:            {}\n\
                 stored fingerprint:  {}\n\
                 current fingerprint: {}\n\
                 \n\
                 The input type or sink structure no longer matches the persisted\n\
                 state. Either restore the previous pipeline, or start fresh\n\
                 (`bogkit dev --fresh`, or delete the data dir and its .schema file).",
                db_path.display(),
                stored.trim(),
                fingerprint,
            ),
            crate::SchemaDrift::WipeAndRebuild => {
                eprintln!(
                    "bog-serve: pipeline changed since {} was written \
                     (stored {}, current {fingerprint}); wiping and rebuilding",
                    db_path.display(),
                    stored.trim(),
                );
                stream.reset();
            }
        },
        Err(_) => {}
    }
    if let Err(e) = std::fs::write(&marker, fingerprint) {
        eprintln!("bog-serve: could not persist schema fingerprint: {e}");
    }
}
