//! Generic axum handlers over any served pipeline.
//!
//! Reads are identical for plain and keyed streams, so they're generic over
//! [`ViewSource`] — the small trait both `Shared` (wrapping a fold
//! `Stream`) and `SharedKeyed` (wrapping a `KeyedStream`) implement. Writes
//! differ by stream flavor (raw insert/remove vs upsert/remove-by-key) and
//! stay per-kind. Nothing here knows what the user's pipeline looks like:
//! writes go through fold's `wtx`, reads dispatch through [`Views`].

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

use crate::openapi::WriteStyle;
use crate::views::{ViewQuery, ViewRead, Views, parse_key, schema_of};

const DEFAULT_LIMIT: usize = 100;
const DEFAULT_K: usize = 10;

// ---- custom routes ----------------------------------------------------------
//
// Handlers are registered typed (`Q`/`B` in, `T` out) and stored erased so
// `App` can hold a heterogeneous list. The typed layer lives in lib.rs; it
// captures the JSON schemas into a `CustomDoc` *before* boxing erases the
// types, then wraps the handler so the stored closure speaks raw query
// strings / body bytes and `Value` results.

/// An erased custom GET handler: pipeline readers + the raw query string.
pub(crate) type ReadHandler<D, P> = Arc<
    dyn for<'tx> Fn(<P as Push<D>>::Reader<'tx, Snapshot>, &str) -> Result<Value, (u16, String)>
        + Send
        + Sync,
>;

/// An erased custom POST handler over a plain stream: exclusive stream
/// access + the raw body. Runs inside `try_wtx`, so `Err` rolls back.
pub(crate) type StreamWriteHandler<D, P> =
    Arc<dyn Fn(&mut Stream<D, P>, &[u8]) -> Result<Value, (u16, String)> + Send + Sync>;

/// As [`StreamWriteHandler`], over a keyed stream.
pub(crate) type KeyedWriteHandler<K, V, P> =
    Arc<dyn Fn(&mut KeyedStream<K, V, P>, &[u8]) -> Result<Value, (u16, String)> + Send + Sync>;

/// One registered custom route: its OpenAPI contribution plus the erased
/// handler. `W` is the write-handler type, which differs by stream flavor.
pub(crate) struct CustomRoute<D: Clone, P: Push<D>, W> {
    pub doc: crate::openapi::CustomDoc,
    pub action: CustomAction<D, P, W>,
}

pub(crate) enum CustomAction<D: Clone, P: Push<D>, W> {
    Read(ReadHandler<D, P>),
    Write(W),
}

/// The documents computed once at startup and served verbatim.
struct Docs {
    openapi: Value,
    schema: Value,
}

/// Read access shared by every view route, implemented per stream flavor.
trait ViewSource: Send + Sync + 'static {
    /// One consistent observation: the commit seq and the view's answer
    /// come from the same snapshot.
    fn view(&self, view: &str, q: &ViewQuery) -> (u64, ViewRead);
    fn docs(&self) -> &Docs;
    fn subscribe(&self) -> watch::Receiver<u64>;
}

// ---- unkeyed: Stream<D, P> --------------------------------------------------

struct Shared<D: Clone, P: Push<D>> {
    /// RwLock matches fold's access model exactly: `rtx` takes `&self`
    /// (any number of parallel readers, each on its own pinned snapshot),
    /// `wtx` takes `&mut self` (one writer at a time).
    inner: RwLock<Inner<D, P>>,
    docs: Docs,
    /// Broadcasts the latest commit seq to `/watch` subscribers.
    notify: watch::Sender<u64>,
}

struct Inner<D: Clone, P: Push<D>> {
    stream: Stream<D, P>,
    /// Bumped once per committed transaction. In-memory for now: it orders
    /// reads against writes within one server run (read-your-writes), and
    /// becomes persistent when the delta log lands.
    seq: u64,
}

impl<D, P> ViewSource for Shared<D, P>
where
    D: Clone + Send + Sync + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
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

pub(crate) fn router<D, P>(
    stream: Stream<D, P>,
    custom: Vec<CustomRoute<D, P, StreamWriteHandler<D, P>>>,
    db_path: &std::path::Path,
) -> Router
where
    D: Clone + Send + Sync + DeserializeOwned + JsonSchema + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let specs = stream.rtx(|r| {
        let mut specs = Vec::new();
        r.specs(&mut specs);
        specs
    });
    let input_schema = schema_of::<D>();
    let custom_docs: Vec<_> = custom.iter().map(|c| c.doc.clone()).collect();

    let schema = crate::openapi::schema_doc(&input_schema, &specs, WriteStyle::Unkeyed);
    check_fingerprint(db_path, &schema);

    let shared = Arc::new(Shared {
        docs: Docs {
            openapi: crate::openapi::openapi_doc(
                &input_schema,
                &specs,
                WriteStyle::Unkeyed,
                &custom_docs,
            ),
            schema,
        },
        notify: watch::channel(0).0,
        inner: RwLock::new(Inner { stream, seq: 0 }),
    });

    let mut app = read_routes::<Shared<D, P>>()
        .route("/insert", post(insert::<D, P>))
        .route("/remove", post(remove::<D, P>))
        .route("/batch", post(batch::<D, P>))
        .with_state(shared.clone());

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
                            let out =
                                inner.stream.rtx(|r| handler(r, query.as_deref().unwrap_or("")));
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
                                // parse failure or handler Err: the whole
                                // transaction rolled back — no seq, no notify
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

/// Run one write transaction and return the new commit seq, notifying
/// `/watch` subscribers. Everything pushed inside `f` commits atomically.
fn write<D, P>(shared: &Shared<D, P>, f: impl FnOnce(&mut fold::stream::Tx<'_, '_, D, P>)) -> u64
where
    D: Clone,
    P: Push<D>,
{
    let mut inner = shared.inner.write().unwrap();
    inner.stream.wtx(f);
    inner.seq += 1;
    let seq = inner.seq;
    drop(inner); // wake subscribers only after the exclusive guard is gone
    // send_replace, not send: send() refuses to store when no /watch
    // client is connected yet, and late subscribers must still see the
    // latest committed seq
    shared.notify.send_replace(seq);
    seq
}

async fn insert<D, P>(
    State(shared): State<Arc<Shared<D, P>>>,
    body: Result<Json<D>, JsonRejection>,
) -> Response
where
    D: Clone + Send + Sync + DeserializeOwned + 'static,
    P: Push<D> + Send + Sync + 'static,
{
    let data = match require_json(body) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let seq = write(&shared, |tx| tx.insert(&data));
    Json(json!({ "seq": seq })).into_response()
}

async fn remove<D, P>(
    State(shared): State<Arc<Shared<D, P>>>,
    body: Result<Json<D>, JsonRejection>,
) -> Response
where
    D: Clone + Send + Sync + DeserializeOwned + 'static,
    P: Push<D> + Send + Sync + 'static,
{
    let data = match require_json(body) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let seq = write(&shared, |tx| tx.remove(&data));
    Json(json!({ "seq": seq })).into_response()
}

/// One operation in an unkeyed POST /batch body:
/// `{ "op": "insert", "data": ... }` or `{ "op": "remove", "data": ... }`.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Op<D> {
    Insert { data: D },
    Remove { data: D },
}

async fn batch<D, P>(
    State(shared): State<Arc<Shared<D, P>>>,
    body: Result<Json<Vec<Op<D>>>, JsonRejection>,
) -> Response
where
    D: Clone + Send + Sync + DeserializeOwned + 'static,
    P: Push<D> + Send + Sync + 'static,
{
    // the whole body deserialized before the transaction opens: a malformed
    // batch is rejected in full, a well-formed one commits in full
    let ops = match require_json(body) {
        Ok(ops) => ops,
        Err(resp) => return resp,
    };
    let applied = ops.len();
    let seq = write(&shared, |tx| {
        for op in &ops {
            match op {
                Op::Insert { data } => tx.insert(data),
                Op::Remove { data } => tx.remove(data),
            }
        }
    });
    Json(json!({ "seq": seq, "applied": applied })).into_response()
}

// ---- keyed: KeyedStream<K, V, P> --------------------------------------------

struct SharedKeyed<K: Clone, V: Clone, P: Push<Keyed<K, V>>> {
    inner: RwLock<InnerKeyed<K, V, P>>,
    docs: Docs,
    notify: watch::Sender<u64>,
}

struct InnerKeyed<K: Clone, V: Clone, P: Push<Keyed<K, V>>> {
    stream: KeyedStream<K, V, P>,
    seq: u64,
}

impl<K, V, P> ViewSource for SharedKeyed<K, V, P>
where
    K: Clone + Send + Sync + Serialize + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
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

pub(crate) fn router_keyed<K, V, P>(
    stream: KeyedStream<K, V, P>,
    custom: Vec<CustomRoute<Keyed<K, V>, P, KeyedWriteHandler<K, V, P>>>,
    db_path: &std::path::Path,
) -> Router
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + JsonSchema + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + JsonSchema + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let specs = stream.rtx(|r| {
        let mut specs = Vec::new();
        r.specs(&mut specs);
        specs
    });
    let input_schema = schema_of::<V>();
    let key_schema = schema_of::<K>();
    let custom_docs: Vec<_> = custom.iter().map(|c| c.doc.clone()).collect();
    let style = WriteStyle::Keyed {
        key_schema: &key_schema,
    };

    let schema = crate::openapi::schema_doc(&input_schema, &specs, style);
    check_fingerprint(db_path, &schema);

    let shared = Arc::new(SharedKeyed {
        docs: Docs {
            openapi: crate::openapi::openapi_doc(&input_schema, &specs, style, &custom_docs),
            schema,
        },
        notify: watch::channel(0).0,
        inner: RwLock::new(InnerKeyed { stream, seq: 0 }),
    });

    let mut app = read_routes::<SharedKeyed<K, V, P>>()
        .route(
            "/docs/{key}",
            put(put_doc::<K, V, P>)
                .delete(delete_doc::<K, V, P>)
                .get(get_doc::<K, V, P>),
        )
        .route("/batch", post(batch_keyed::<K, V, P>))
        .with_state(shared.clone());

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
                            let out =
                                inner.stream.rtx(|r| handler(r, query.as_deref().unwrap_or("")));
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

/// Run one keyed write transaction; returns the new seq and `f`'s result.
fn write_keyed<K, V, P, R>(
    shared: &SharedKeyed<K, V, P>,
    f: impl FnOnce(&mut fold::stream::KeyedTx<'_, '_, '_, K, V, P>) -> R,
) -> (u64, R)
where
    K: Clone + Serialize,
    V: Clone + Serialize + DeserializeOwned,
    P: Push<Keyed<K, V>>,
{
    let mut inner = shared.inner.write().unwrap();
    let out = inner.stream.wtx(f);
    inner.seq += 1;
    let seq = inner.seq;
    drop(inner);
    shared.notify.send_replace(seq); // see write(): send() drops the value receiverless
    (seq, out)
}

async fn put_doc<K, V, P>(
    State(shared): State<Arc<SharedKeyed<K, V, P>>>,
    Path(raw): Path<String>,
    body: Result<Json<V>, JsonRejection>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
{
    let data = match require_json(body) {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let Some(key) = parse_key::<K>(&raw) else {
        return error(StatusCode::BAD_REQUEST, key_parse_msg(&raw));
    };
    let (seq, old) = write_keyed(&shared, |tx| tx.upsert(&key, &data));
    Json(json!({ "seq": seq, "replaced": old.is_some() })).into_response()
}

async fn delete_doc<K, V, P>(
    State(shared): State<Arc<SharedKeyed<K, V, P>>>,
    Path(raw): Path<String>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
{
    let Some(key) = parse_key::<K>(&raw) else {
        return error(StatusCode::BAD_REQUEST, key_parse_msg(&raw));
    };
    let (seq, old) = write_keyed(&shared, |tx| tx.remove(&key));
    Json(json!({ "seq": seq, "removed": old.is_some() })).into_response()
}

async fn get_doc<K, V, P>(
    State(shared): State<Arc<SharedKeyed<K, V, P>>>,
    Path(raw): Path<String>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
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

/// One operation in a keyed POST /batch body:
/// `{ "op": "upsert", "key": ..., "data": ... }` or
/// `{ "op": "remove", "key": ... }`.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum KeyedOp<K, V> {
    Upsert { key: K, data: V },
    Remove { key: K },
}

async fn batch_keyed<K, V, P>(
    State(shared): State<Arc<SharedKeyed<K, V, P>>>,
    body: Result<Json<Vec<KeyedOp<K, V>>>, JsonRejection>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
{
    let ops = match require_json(body) {
        Ok(ops) => ops,
        Err(resp) => return resp,
    };
    let applied = ops.len();
    let (seq, ()) = write_keyed(&shared, |tx| {
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
    });
    Json(json!({ "seq": seq, "applied": applied })).into_response()
}

fn key_parse_msg(raw: &str) -> String {
    format!("cannot parse {raw:?} as this stream's key type")
}

// ---- reads (generic over both flavors) --------------------------------------

fn read_routes<S: ViewSource>() -> Router<Arc<S>> {
    Router::new()
        .route("/healthz", get(async || "ok"))
        .route("/views/{name}", get(view_list::<S>))
        // static "search"/"watch" outrank the {key} capture in axum's router
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
    fn query(&self) -> ViewQuery {
        ViewQuery::list(
            self.limit.unwrap_or(DEFAULT_LIMIT),
            self.offset.unwrap_or(0),
            self.desc.unwrap_or(false),
        )
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
) -> Response {
    respond(shared.view(&name, &ViewQuery::point(key)))
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

/// Watch one view: an SSE stream that re-reads the view after every commit
/// and pushes the fresh `{seq, data}` payload. `?limit`/`?desc` shape the
/// read, so `/views/leaderboard/watch?desc=true&limit=10` is a live top-10.
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
    // probe once so a bad view name is an immediate 404, not a silent stream
    match shared.view(&name, &q).1 {
        ViewRead::NotFound => return error(StatusCode::NOT_FOUND, "not found"),
        ViewRead::BadRequest(msg) => return error(StatusCode::BAD_REQUEST, msg),
        ViewRead::Data(_) => {}
    }

    let events = tokio_stream::wrappers::WatchStream::new(shared.subscribe()).map(move |_| {
        // re-read rather than reuse the probe: each commit notification
        // triggers a fresh consistent snapshot of this view
        let (seq, out) = shared.view(&name, &q);
        let data = match out {
            ViewRead::Data(data) => data,
            // can't happen after a successful probe; keep the stream alive
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

/// SSE commit feed: one `{"seq": n}` event per committed transaction (the
/// current seq arrives immediately on connect). Slow consumers see the
/// latest seq, not every intermediate one — it's a level, not a log.
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

/// Unwrap a JSON body or produce the standard error shape. The rejection's
/// text carries serde's message, which names the offending field.
fn require_json<T>(body: Result<Json<T>, JsonRejection>) -> Result<T, Response> {
    match body {
        Ok(Json(v)) => Ok(v),
        Err(rej) => Err(error(
            StatusCode::BAD_REQUEST,
            format!("invalid body: {}", rej.body_text()),
        )),
    }
}

/// Unwrap query parameters or produce the standard error shape.
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
/// misinterpret persisted state — so refuse to start, loudly.
fn check_fingerprint(db_path: &std::path::Path, schema: &Value) {
    let fingerprint = schema["fingerprint"].as_str().unwrap();
    let mut marker = db_path.as_os_str().to_owned();
    marker.push(".schema");

    match std::fs::read_to_string(&marker) {
        Ok(stored) if stored.trim() == fingerprint => {}
        Ok(stored) => panic!(
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
        // first open (or unreadable marker): record the fingerprint;
        // best-effort — a read-only fs shouldn't stop the server
        Err(_) => {
            if let Err(e) = std::fs::write(&marker, fingerprint) {
                eprintln!("bog-serve: could not persist schema fingerprint: {e}");
            }
        }
    }
}
