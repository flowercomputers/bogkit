//! Serve a fold pipeline over HTTP, with the API generated from the
//! pipeline itself.
//!
//! The API surface of any fold program is exactly two things: the input
//! type at the front (which can describe itself via
//! [schemars](https://docs.rs/schemars)) and the named terminal sinks at
//! the back (which readers dispatch to via the [`Views`] trait). Everything
//! between is user closures that never appear in the API — so the whole
//! HTTP layer is generated, and the OpenAPI doc is assembled from the same
//! values the router dispatches with.
//!
//! ```no_run
//! use bog_serve::{App, NoParams};
//! use fold::pipeline::terminal;
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//! use serde_json::json;
//!
//! #[derive(Clone, Serialize, Deserialize, JsonSchema)]
//! struct Entry {
//!     text: String,
//! }
//!
//! App::stream(
//!     bog_serve::data_dir(),
//!     (
//!         terminal::Count::new("total"),
//!         terminal::Bag::<Entry>::new("entries"),
//!     ),
//! )
//! // custom routes are typed; their schemas land in /openapi.json.
//! // GET handlers read one consistent snapshot:
//! .get("/summary", |(count, _entries), _: NoParams| {
//!     Ok(json!({ "total": count.get() }))
//! })
//! // POST handlers run inside one write transaction; Err rolls it back:
//! .post("/insert_nonempty", |tx, e: Entry| {
//!     if e.text.is_empty() {
//!         return Err((422, "empty entries rejected".into()));
//!     }
//!     tx.insert(&e);
//!     Ok(json!("ok"))
//! })
//! .run()
//! ```
//!
//! Generated routes:
//!
//! - unkeyed ([`App`]): `POST /insert`, `POST /remove`, `POST /batch`
//!   (`[{"op": "insert"|"remove", "data": ...}]`) — each one atomic
//!   transaction
//! - keyed ([`KeyedApp`]): `PUT|GET|DELETE /docs/{key}`, `POST /batch`
//!   (`[{"op": "upsert"|"remove", "key": ..., "data": ...}]`)
//! - `GET /views/{name}` — read the sink with that name (`?limit`/`?offset`
//!   paginate list-shaped views; `?desc=true` lists ordered views
//!   highest-first)
//! - `GET /views/{name}/{key}` — point lookup on keyed views
//! - `GET|POST /views/{name}/search` — ranked search on searchable views
//!   (`?q=&k=` text, or `{"vector": [...], "k": n}` by raw vector)
//! - `GET /watch` — SSE, one `{"seq": n}` event per commit
//! - `GET /views/{name}/watch` — SSE, the view's fresh payload after every
//!   commit (`?limit`/`?desc` shape the read — e.g. a live top-10)
//! - `GET /openapi.json`, `GET /schema`, `GET /healthz`
//!
//! Every write response carries the commit `seq`; every read response
//! carries the `seq` its snapshot reflects. The seq is in-memory: it
//! orders reads against writes within one server run and restarts at 0
//! with the process.
//!
//! Custom routes compose with the generated ones, fully typed:
//! [`App::get`]/[`KeyedApp::get`] handlers receive the pipeline's readers
//! (same tuple shape as `rtx`) plus the query string deserialized into a
//! caller-chosen struct, and [`App::post`]/[`KeyedApp::post`] handlers
//! receive the write transaction plus a typed body — returning `Err` rolls
//! the whole transaction back. Both response and request schemas are
//! captured at registration, so custom routes appear fully typed in
//! `/openapi.json`.

mod http;
mod openapi;
mod search;
mod views;

pub use search::{TextQuery, TextQueryReader, VectorSearch};
pub use views::{ViewQuery, ViewRead, ViewSpec, Views};

use std::sync::Arc;

use fold::fjall::Snapshot;
use fold::pipeline::{Keyed, Push};
use fold::stream::{KeyedStream, KeyedTx, Stream, Tx};
use http::{CustomAction, CustomRoute, KeyedWriteHandler, ReadHandler, StreamWriteHandler};
use openapi::CustomDoc;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Typed "no query parameters" for custom GET routes that take none:
/// `.get("/path", |readers, _: NoParams| ...)`.
#[derive(serde::Deserialize, JsonSchema)]
pub struct NoParams {}

fn to_value<T: Serialize>(t: T) -> Value {
    serde_json::to_value(t).expect("custom route Ok type serializes to JSON")
}

/// A fold [`Stream`] wrapped in a generated HTTP server.
pub struct App<D: Clone, P: Push<D>> {
    stream: Stream<D, P>,
    custom: Vec<CustomRoute<D, P, StreamWriteHandler<D, P>>>,
    db_path: std::path::PathBuf,
}

impl<D, P> App<D, P>
where
    D: Clone + Send + Sync + DeserializeOwned + JsonSchema + 'static,
    P: Push<D> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    /// Open (or create) the database at `path` and wrap `pipeline` in a
    /// server. Reopening the same path with the same pipeline resumes
    /// prior state.
    pub fn stream(path: impl AsRef<std::path::Path>, pipeline: P) -> Self {
        App {
            stream: Stream::new(&path, pipeline),
            custom: Vec::new(),
            db_path: path.as_ref().to_path_buf(),
        }
    }

    /// Add a typed custom GET route alongside the generated ones.
    ///
    /// The handler receives the pipeline's readers — the same tuple `rtx`
    /// closures see, on one consistent snapshot — plus the query string
    /// deserialized into `Q` (use [`NoParams`] when there are none). Its
    /// `Ok` value lands in the standard `{seq, data}` envelope. Both
    /// schemas are captured here, while the types are still known, so the
    /// route appears fully typed in `/openapi.json` and the doc cannot
    /// drift from what the handler actually returns.
    pub fn get<Q, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        Q: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'tx> Fn(P::Reader<'tx, Snapshot>, Q) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        let doc = CustomDoc {
            path: path.into(),
            method: "get",
            params: Some(views::schema_of::<Q>()),
            body: None,
            response: views::schema_of::<T>(),
        };
        let erased: ReadHandler<D, P> = Arc::new(move |readers, raw_query| {
            let q: Q = serde_urlencoded::from_str(raw_query)
                .map_err(|e| (400, format!("invalid query: {e}")))?;
            handler(readers, q).map(to_value)
        });
        self.custom.push(CustomRoute {
            doc,
            action: CustomAction::Read(erased),
        });
        self
    }

    /// Add a typed custom POST route: a write transaction with app logic.
    ///
    /// The handler receives fold's write transaction and the body
    /// deserialized into `B`. Returning `Ok` commits everything pushed and
    /// bumps the seq; returning `Err(status, message)` **rolls the whole
    /// transaction back** — even deltas pushed before the error — making
    /// this the building block for atomic check-and-set over HTTP
    /// (read mid-transaction via [`Tx::rtx`], bail to abort).
    pub fn post<B, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        B: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'g, 'tx> Fn(&mut Tx<'g, 'tx, D, P>, B) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        let doc = CustomDoc {
            path: path.into(),
            method: "post",
            params: None,
            body: Some(views::schema_of::<B>()),
            response: views::schema_of::<T>(),
        };
        let erased: StreamWriteHandler<D, P> = Arc::new(move |stream, raw| {
            let body: B = serde_json::from_slice(raw)
                .map_err(|e| (400, format!("invalid body: {e}")))?;
            stream.try_wtx(|tx| handler(tx, body)).map(to_value)
        });
        self.custom.push(CustomRoute {
            doc,
            action: CustomAction::Write(erased),
        });
        self
    }

    /// Every generated route as an [`axum::Router`], no listener attached.
    /// This is the seam tests drive requests through.
    ///
    /// # Panics
    /// If the data dir was written by a different pipeline (schema
    /// fingerprint mismatch) — see the `/schema` route.
    pub fn into_router(self) -> axum::Router {
        http::router(self.stream, self.custom, &self.db_path)
    }

    /// Serve on `0.0.0.0:$PORT` (default 7877), blocking forever.
    ///
    /// # Panics
    /// On a schema fingerprint mismatch, as [`App::into_router`], and if
    /// the port cannot be bound.
    pub fn run(self) {
        serve_blocking(self.into_router())
    }
}

/// A fold [`KeyedStream`] wrapped in a generated HTTP server: writes are
/// upsert/remove by primary key, and replacing or deleting a record
/// retracts the old one from every view automatically.
pub struct KeyedApp<K: Clone, V: Clone, P: Push<Keyed<K, V>>> {
    stream: KeyedStream<K, V, P>,
    custom: Vec<CustomRoute<Keyed<K, V>, P, KeyedWriteHandler<K, V, P>>>,
    db_path: std::path::PathBuf,
}

impl<K, V, P> KeyedApp<K, V, P>
where
    K: Clone + Send + Sync + Serialize + DeserializeOwned + JsonSchema + 'static,
    V: Clone + Send + Sync + Serialize + DeserializeOwned + JsonSchema + 'static,
    P: Push<Keyed<K, V>> + Send + Sync + 'static,
    for<'tx> P::Reader<'tx, Snapshot>: Views,
{
    /// Open (or create) the database at `path` and wrap `pipeline` — which
    /// receives [`Keyed`]`<K, V>` deltas — in a server.
    pub fn stream(path: impl AsRef<std::path::Path>, pipeline: P) -> Self {
        KeyedApp {
            stream: KeyedStream::new(&path, pipeline),
            custom: Vec::new(),
            db_path: path.as_ref().to_path_buf(),
        }
    }

    /// Add a typed custom GET route; see [`App::get`].
    pub fn get<Q, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        Q: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'tx> Fn(P::Reader<'tx, Snapshot>, Q) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        let doc = CustomDoc {
            path: path.into(),
            method: "get",
            params: Some(views::schema_of::<Q>()),
            body: None,
            response: views::schema_of::<T>(),
        };
        let erased: ReadHandler<Keyed<K, V>, P> = Arc::new(move |readers, raw_query| {
            let q: Q = serde_urlencoded::from_str(raw_query)
                .map_err(|e| (400, format!("invalid query: {e}")))?;
            handler(readers, q).map(to_value)
        });
        self.custom.push(CustomRoute {
            doc,
            action: CustomAction::Read(erased),
        });
        self
    }

    /// Add a typed custom POST route over the keyed write transaction;
    /// see [`App::post`]. `Err` rolls the whole transaction back, so
    /// upsert-unless-present and other check-and-set flows are atomic.
    pub fn post<B, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        B: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'a, 'g, 'tx> Fn(&mut KeyedTx<'a, 'g, 'tx, K, V, P>, B) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        let doc = CustomDoc {
            path: path.into(),
            method: "post",
            params: None,
            body: Some(views::schema_of::<B>()),
            response: views::schema_of::<T>(),
        };
        let erased: KeyedWriteHandler<K, V, P> = Arc::new(move |stream, raw| {
            let body: B = serde_json::from_slice(raw)
                .map_err(|e| (400, format!("invalid body: {e}")))?;
            stream.try_wtx(|tx| handler(tx, body)).map(to_value)
        });
        self.custom.push(CustomRoute {
            doc,
            action: CustomAction::Write(erased),
        });
        self
    }

    /// Every generated route as an [`axum::Router`], no listener attached.
    ///
    /// # Panics
    /// If the data dir was written by a different pipeline (schema
    /// fingerprint mismatch) — see the `/schema` route.
    pub fn into_router(self) -> axum::Router {
        http::router_keyed(self.stream, self.custom, &self.db_path)
    }

    /// Serve on `0.0.0.0:$PORT` (default 7877), blocking forever.
    ///
    /// # Panics
    /// On a schema fingerprint mismatch, as [`KeyedApp::into_router`], and
    /// if the port cannot be bound.
    pub fn run(self) {
        serve_blocking(self.into_router())
    }
}

/// Where state lives: `$BOG_DATA_DIR` (set by `bogkit dev`) or `./bog.db`.
pub fn data_dir() -> std::path::PathBuf {
    std::env::var_os("BOG_DATA_DIR")
        .map(Into::into)
        .unwrap_or_else(|| "bog.db".into())
}

/// Serve on `0.0.0.0:$PORT` (default 7877), blocking forever. The runtime
/// lives here so user main stays a plain fn.
fn serve_blocking(router: axum::Router) {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7877);

    let rt = tokio::runtime::Runtime::new().expect("starting tokio runtime");
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
            .await
            .unwrap_or_else(|e| panic!("binding port {port}: {e}"));
        println!("bog-serve on http://localhost:{port} — routes at /openapi.json");
        axum::serve(listener, router).await.expect("serving");
    });
}
