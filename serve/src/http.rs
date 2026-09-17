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
use crate::views::{ViewQuery, ViewRead, Views, parse_key_mode, schema_of};

const DEFAULT_LIMIT: usize = 100;
const DEFAULT_K: usize = 10;

/// Read access shared by every view route, implemented per stream flavor.
trait Rtx: Send + Sync + 'static {
    type Reader<'tx>: Views
    where
        Self: 'tx;

    fn rtx<R>(&self, f: impl for<'tx> FnOnce(Self::Reader<'tx>) -> R) -> R;

    /// Fsync all committed state; see [`fold::stream::Stream::checkpoint`].
    fn checkpoint(&mut self) -> Result<(), fold::fjall::Error>;

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

    fn checkpoint(&mut self) -> Result<(), fold::fjall::Error> {
        Stream::try_checkpoint(self)
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

    fn checkpoint(&mut self) -> Result<(), fold::fjall::Error> {
        KeyedStream::try_checkpoint(self)
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
    fn raw_string_keys(&self) -> bool;
    fn stopped(&self) -> watch::Receiver<bool>;
}

#[cfg(test)]
type CheckpointHook = Arc<dyn Fn() -> Result<(), fold::fjall::Error> + Send + Sync>;
#[cfg(test)]
thread_local! { static CHECKPOINT_HOOK: std::cell::RefCell<Option<CheckpointHook>> = const { std::cell::RefCell::new(None) }; }

struct Shared<S> {
    inner: RwLock<Inner<S>>,
    docs: Docs,
    notify: watch::Sender<u64>,
    raw_string_keys: bool,
    durability: crate::Durability,
    #[cfg(test)]
    checkpoint_hook: Option<CheckpointHook>,
    stopped: watch::Sender<bool>,
}

impl<S: Rtx> Shared<S> {
    fn checkpoint(&self, stream: &mut S) -> Result<(), fold::fjall::Error> {
        #[cfg(test)]
        if let Some(hook) = &self.checkpoint_hook {
            hook()?;
        }
        stream.checkpoint()
    }
}

struct Inner<S> {
    stream: S,
    seq: u64,
    failed: bool,
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

    fn stopped(&self) -> watch::Receiver<bool> {
        self.stopped.subscribe()
    }

    fn raw_string_keys(&self) -> bool {
        self.raw_string_keys
    }
}

/// Everything [`run`](crate::App::run) needs beyond the router itself:
/// hooks into the shared stream for the idle watchdog, type-erased so the
/// serve loop stays non-generic.
pub(crate) struct Lifecycle {
    /// The pipeline's schema fingerprint (also persisted in the `.schema`
    /// sidecar), for the discovery sidecar.
    pub fingerprint: String,
    pub stop: Box<dyn Fn() + Send + Sync>,
    /// Drains in-flight writes (takes the write lock) and fsyncs all
    /// committed state.
    pub checkpoint: Box<dyn FnOnce() -> Result<(), crate::ServeError> + Send>,
    /// Live SSE subscriptions (`/watch`, `/views/{name}/watch`); the idle
    /// watchdog won't exit while any are connected.
    pub watchers: Box<dyn Fn() -> usize + Send + Sync>,
}

impl Lifecycle {
    fn new<S: Rtx>(shared: Arc<Shared<S>>, fingerprint: String) -> Self {
        let cp = shared.clone();
        let stop = shared.clone();
        Lifecycle {
            fingerprint,
            stop: Box::new(move || {
                stop.stopped.send_replace(true);
            }),
            checkpoint: Box::new(move || {
                cp.stopped.send_replace(true);
                cp.checkpoint(&mut cp.inner.write().unwrap().stream)
                    .map_err(|_| {
                        crate::ServeError(
                            "checkpoint failed; committed outcome is uncertain".into(),
                        )
                    })
            }),
            watchers: Box::new(move || shared.notify.receiver_count()),
        }
    }
}

// Keep both router constructors explicit about their schema and durability policy.
#[allow(clippy::too_many_arguments)]
fn shared_from<D, P, S: Rtx>(
    mut stream: S,
    custom: &[CustomRoute<D, P, S>],
    db_path: &std::path::Path,
    input_schema: &Value,
    style: WriteStyle<'_>,
    drift: crate::SchemaDrift,
    raw_string_keys: bool,
    durability: crate::Durability,
) -> Result<(Arc<Shared<S>>, String), crate::ServeError>
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
    check_fingerprint(db_path, &fingerprint, drift, &mut stream)?;
    let shared = Arc::new(Shared {
        docs: Docs {
            openapi: crate::openapi::openapi_doc(input_schema, &specs, style, &custom_docs),
            schema,
        },
        notify: watch::channel(0).0,
        raw_string_keys,
        durability,
        #[cfg(test)]
        checkpoint_hook: CHECKPOINT_HOOK.with(|hook| hook.borrow().clone()),
        stopped: watch::channel(false).0,
        inner: RwLock::new(Inner {
            stream,
            seq: 0,
            failed: false,
        }),
    });
    Ok((shared, fingerprint))
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
                            match try_commit(&shared, |stream| handler(stream, &body)) {
                                Ok((seq, data)) => {
                                    Json(json!({ "seq": seq, "data": data })).into_response()
                                }
                                Err(resp) => resp,
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
    durability: crate::Durability,
) -> Result<(Router, Lifecycle), crate::ServeError>
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
        false,
        durability,
    )?;
    let app = read_routes::<Shared<Stream<D, P>>>()
        .route("/insert", post(insert::<D, P>))
        .route("/remove", post(remove::<D, P>))
        .route("/batch", post(batch::<D, P>))
        .with_state(shared.clone());
    let lifecycle = Lifecycle::new(shared.clone(), fingerprint);
    let guard = shared.clone();
    let app = attach_custom(app, shared, custom).layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let guard = guard.clone();
            async move {
                if *guard.stopped.borrow() || guard.inner.read().unwrap().failed {
                    return unavailable();
                }
                next.run(req).await
            }
        },
    ));
    Ok((app, lifecycle))
}

pub(crate) fn router_keyed<K, V, P>(
    quota: crate::QuotaConfig<K, V, P>,
    stream: KeyedStream<K, V, P>,
    custom: Vec<KeyedCustom<K, V, P>>,
    db_path: &std::path::Path,
    drift: crate::SchemaDrift,
    raw_string_keys: bool,
    durability: crate::Durability,
) -> Result<(Router, Lifecycle), crate::ServeError>
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
        raw_string_keys,
        durability,
    )?;
    let app = read_routes::<Shared<KeyedStream<K, V, P>>>()
        .route(
            "/docs/{key}",
            put(put_doc::<K, V, P>)
                .delete(delete_doc::<K, V, P>)
                .get(get_doc::<K, V, P>),
        )
        .route("/batch", post(batch_keyed::<K, V, P>))
        .layer(axum::Extension(quota))
        .with_state(shared.clone());
    let lifecycle = Lifecycle::new(shared.clone(), fingerprint);
    let guard = shared.clone();
    let app = attach_custom(app, shared, custom).layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let guard = guard.clone();
            async move {
                if *guard.stopped.borrow() || guard.inner.read().unwrap().failed {
                    return unavailable();
                }
                next.run(req).await
            }
        },
    ));
    Ok((app, lifecycle))
}

/// Run one write on `stream`, bump seq, notify `/watch`.
///
/// `send_replace`, not `send`: `send()` refuses to store when no client is
/// connected yet, and late subscribers must still see the latest seq.
fn unavailable() -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error": {"code":"storage_unavailable", "message":"storage unavailable; a committed write may not have been checkpointed"}}))).into_response()
}

#[allow(clippy::result_large_err)]
fn try_commit<S: Rtx, R>(
    shared: &Shared<S>,
    f: impl FnOnce(&mut S) -> Result<R, (u16, String)>,
) -> Result<(u64, R), Response> {
    let mut inner = shared.inner.write().unwrap();
    if inner.failed || *shared.stopped.borrow() {
        return Err(unavailable());
    }
    let out = f(&mut inner.stream).map_err(|(code, msg)| {
        error(
            StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            msg,
        )
    })?;
    if shared.durability == crate::Durability::CheckpointBeforeAck
        && shared.checkpoint(&mut inner.stream).is_err()
    {
        inner.failed = true;
        shared.stopped.send_replace(true);
        return Err(unavailable());
    }
    inner.seq += 1;
    let seq = inner.seq;
    shared.notify.send_replace(seq);
    Ok((seq, out))
}

#[allow(clippy::result_large_err)]
fn commit<S: Rtx>(shared: &Shared<S>, f: impl FnOnce(&mut S)) -> Result<u64, Response> {
    try_commit(shared, |stream| {
        f(stream);
        Ok(())
    })
    .map(|(seq, ())| seq)
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
    let seq = match commit(&shared, |stream| stream.wtx(|tx| tx.insert(&data))) {
        Ok(value) => value,
        Err(response) => return response,
    };
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
    let seq = match commit(&shared, |stream| stream.wtx(|tx| tx.remove(&data))) {
        Ok(value) => value,
        Err(response) => return response,
    };
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
    let seq = match commit(&shared, |stream| {
        stream.wtx(|tx| {
            for op in &ops {
                match op {
                    Op::Insert { data } => tx.insert(data),
                    Op::Remove { data } => tx.remove(data),
                }
            }
        })
    }) {
        Ok(value) => value,
        Err(response) => return response,
    };
    Json(json!({ "seq": seq, "applied": applied })).into_response()
}

pub(crate) fn quota_wtx<K, V, P, R>(
    stream: &mut KeyedStream<K, V, P>,
    quota: &crate::QuotaConfig<K, V, P>,
    action: impl FnOnce(&mut fold::stream::KeyedTx<'_, '_, '_, K, V, P>) -> Result<R, (u16, String)>,
) -> Result<R, (u16, String)>
where
    K: Clone + Serialize,
    V: Clone + Serialize + DeserializeOwned,
    P: Push<Keyed<K, V>>,
{
    let quota = quota.read().unwrap();
    stream.try_wtx(|tx| {
        let before = quota.as_ref().map(|(_, measure)| measure(tx));
        let result = action(tx)?;
        if let Some((limit, measure)) = quota.as_ref() {
            let after = measure(tx);
            if after > *limit && after > before.unwrap() {
                return Err((413, "logical storage quota exceeded".into()));
            }
        }
        Ok(result)
    })
}

async fn put_doc<K, V, P>(
    axum::Extension(quota): axum::Extension<crate::QuotaConfig<K, V, P>>,
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
    let Some(key) = parse_key_mode::<K>(&raw, shared.raw_string_keys) else {
        return error(StatusCode::BAD_REQUEST, key_parse_msg(&raw));
    };
    let (seq, old) = match try_commit(&shared, |stream| {
        quota_wtx(stream, &quota, |tx| Ok(tx.upsert(&key, &data)))
    }) {
        Ok(value) => value,
        Err(response) => return response,
    };
    Json(json!({ "seq": seq, "replaced": old.is_some() })).into_response()
}

async fn delete_doc<K, V, P>(
    axum::Extension(quota): axum::Extension<crate::QuotaConfig<K, V, P>>,
    State(shared): State<Arc<Shared<KeyedStream<K, V, P>>>>,
    Path(raw): Path<String>,
) -> Response
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    let Some(key) = parse_key_mode::<K>(&raw, shared.raw_string_keys) else {
        return error(StatusCode::BAD_REQUEST, key_parse_msg(&raw));
    };
    let (seq, old) = match try_commit(&shared, |stream| {
        quota_wtx(stream, &quota, |tx| Ok(tx.remove(&key)))
    }) {
        Ok(value) => value,
        Err(response) => return response,
    };
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
    let Some(key) = parse_key_mode::<K>(&raw, shared.raw_string_keys) else {
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
    axum::Extension(quota): axum::Extension<crate::QuotaConfig<K, V, P>>,
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
    let (seq, ()) = match try_commit(&shared, |stream| {
        quota_wtx(stream, &quota, |tx| {
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
            Ok(())
        })
    }) {
        Ok(value) => value,
        Err(response) => return response,
    };
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
    let query = ViewQuery::point_page(key, limit, offset, desc)
        .with_raw_string_key(shared.raw_string_keys());
    respond(shared.view(&name, &query))
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

    let events = tokio_stream::wrappers::WatchStream::new(shared.subscribe())
        .map(Some)
        .merge(
            tokio_stream::wrappers::WatchStream::new(shared.stopped())
                .filter(|stop| *stop)
                .map(|_| None),
        )
        .take_while(|seq| seq.is_some())
        .map(move |_| {
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
    let events = tokio_stream::wrappers::WatchStream::new(shared.subscribe())
        .map(Some)
        .merge(
            tokio_stream::wrappers::WatchStream::new(shared.stopped())
                .filter(|stop| *stop)
                .map(|_| None),
        )
        .take_while(|seq| seq.is_some())
        .map(|seq| {
            Ok::<_, Infallible>(
                Event::default()
                    .id(seq.unwrap().to_string())
                    .data(json!({ "seq": seq.unwrap() }).to_string()),
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
) -> Result<(), crate::ServeError> {
    let mut marker = db_path.as_os_str().to_owned();
    marker.push(".schema");

    match std::fs::read_to_string(&marker) {
        Ok(stored) if stored.trim() == fingerprint => return Ok(()),
        Ok(stored) => match drift {
            crate::SchemaDrift::Panic => {
                return Err(crate::ServeError(
                    "pipeline changed since this data dir was written".into(),
                ));
            }
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(crate::ServeError(e.to_string())),
    }
    if let Err(e) = std::fs::write(&marker, fingerprint) {
        return Err(crate::ServeError(format!(
            "could not persist schema fingerprint: {e}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod durability_tests {
    use super::*;
    use crate::{App, Durability, KeyedApp};
    use fold::pipeline::terminal;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tower::ServiceExt;
    async fn send(router: &Router, method: &str, path: &str, body: Value) -> Response {
        router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn every_mutation_checkpoints_and_failed_custom_does_not() {
        let calls = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(false));
        let hook: CheckpointHook = {
            let calls = calls.clone();
            let fail = fail.clone();
            Arc::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    Err(std::io::Error::other("injected checkpoint failure").into())
                } else {
                    Ok(())
                }
            })
        };
        CHECKPOINT_HOOK.with(|value| *value.borrow_mut() = Some(hook));
        let dir = tempfile::tempdir().unwrap();
        let router =
            App::<String, _>::stream(dir.path().join("plain"), terminal::Count::new("total"))
                .durability(Durability::CheckpointBeforeAck)
                .post("/custom", |tx, value: String| {
                    tx.insert(&value);
                    Ok(value)
                })
                .post("/reject", |tx, value: String| {
                    tx.insert(&value);
                    Err::<String, _>((422, "rejected".into()))
                })
                .into_router();
        for (n, path, body) in [
            (1, "/insert", json!("a")),
            (2, "/remove", json!("a")),
            (3, "/batch", json!([{"op":"insert","data":"b"}])),
            (4, "/custom", json!("c")),
        ] {
            assert_eq!(send(&router, "POST", path, body).await.status(), 200);
            assert_eq!(calls.load(Ordering::SeqCst), n);
        }
        assert_eq!(
            send(&router, "POST", "/reject", json!("d")).await.status(),
            422
        );
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        let before = axum::body::to_bytes(
            send(&router, "GET", "/views/total", Value::Null)
                .await
                .into_body(),
            4096,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&before).unwrap(),
            json!({"seq":4,"data":{"value":2}})
        );
        let keyed = KeyedApp::<String, String, _>::stream(
            dir.path().join("keyed"),
            terminal::Table::new("docs"),
        )
        .durability(Durability::CheckpointBeforeAck)
        .post("/custom", |tx, value: String| {
            tx.upsert(&"custom".into(), &value);
            Ok(value)
        })
        .post("/reject", |tx, value: String| {
            tx.upsert(&"bad".into(), &value);
            Err::<String, _>((422, "rejected".into()))
        })
        .into_router();
        let default_router =
            App::<String, _>::stream(dir.path().join("default"), terminal::Count::new("total"))
                .into_router();
        assert_eq!(
            send(&default_router, "POST", "/insert", json!("default"))
                .await
                .status(),
            200
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "default mode must not add a checkpoint"
        );
        CHECKPOINT_HOOK.with(|value| *value.borrow_mut() = None);
        for (n, method, path, body) in [
            (5, "PUT", "/docs/a", json!("a")),
            (6, "DELETE", "/docs/a", Value::Null),
            (
                7,
                "POST",
                "/batch",
                json!([{"op":"upsert","key":"b","data":"b"}]),
            ),
            (8, "POST", "/custom", json!("c")),
        ] {
            assert_eq!(send(&keyed, method, path, body).await.status(), 200);
            assert_eq!(calls.load(Ordering::SeqCst), n);
        }
        assert_eq!(
            send(&keyed, "POST", "/reject", json!("d")).await.status(),
            422
        );
        assert_eq!(calls.load(Ordering::SeqCst), 8);
        fail.store(true, Ordering::SeqCst);
        assert_eq!(
            send(&keyed, "PUT", "/docs/fail", json!("f")).await.status(),
            503
        );
        assert_eq!(
            send(&keyed, "GET", "/docs/b", Value::Null).await.status(),
            503
        );
        assert_eq!(
            send(&keyed, "PUT", "/docs/later", json!("g"))
                .await
                .status(),
            503
        );
        assert_eq!(calls.load(Ordering::SeqCst), 9);
    }
    #[test]
    fn acknowledgement_waits_for_checkpoint() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = std::sync::Mutex::new(release_rx);
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            CHECKPOINT_HOOK.with(|value| {
                *value.borrow_mut() = Some(Arc::new(move || {
                    entered_tx.send(()).unwrap();
                    release.lock().unwrap().recv().unwrap();
                    Ok(())
                }))
            });
            let dir = tempfile::tempdir().unwrap();
            let app = App::<String, _>::stream(dir.path(), terminal::Count::new("total"))
                .durability(Durability::CheckpointBeforeAck)
                .into_router();
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                result_tx
                    .send(send(&app, "POST", "/insert", json!("a")).await.status())
                    .unwrap()
            });
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        assert!(result_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        assert_eq!(
            result_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap(),
            200
        );
        worker.join().unwrap();
    }
}
