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
//! use bog_serve::App;
//! use fold::pipeline::terminal;
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
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
//!   paginate list-shaped views)
//! - `GET /views/{name}/{key}` — point lookup on keyed views
//! - `GET|POST /views/{name}/search` — ranked search on searchable views
//!   (`?q=&k=` text, or `{"vector": [...], "k": n}` by raw vector)
//! - `GET /watch` — SSE, one `{"seq": n}` event per commit
//! - `GET /openapi.json`, `GET /schema`, `GET /healthz`
//!
//! Every write response carries the commit `seq`; every read response
//! carries the `seq` its snapshot reflects.
//!
//! Custom read routes compose with the generated ones via
//! [`App::get`]/[`KeyedApp::get`]: the handler receives the pipeline's
//! readers (same tuple shape as `rtx`) and the query parameters.

mod http;
mod openapi;
mod search;
mod views;

pub use http::{CustomReq, CustomResult};
pub use search::{TextQuery, TextQueryReader, VectorSearch};
pub use views::{ViewQuery, ViewRead, ViewSpec, Views};

use std::sync::Arc;

use fold::fjall::Snapshot;
use fold::pipeline::{Keyed, Push};
use fold::stream::{KeyedStream, Stream};
use http::CustomHandler;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// A fold [`Stream`] wrapped in a generated HTTP server.
pub struct App<D: Clone, P: Push<D>> {
    stream: Stream<D, P>,
    custom: Vec<(String, CustomHandler<D, P>)>,
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

    /// Add a custom GET route alongside the generated ones. The handler
    /// receives the pipeline's readers — the same tuple `rtx` closures see,
    /// on one consistent snapshot — plus the request's query parameters,
    /// and its result is wrapped in the standard `{seq, data}` envelope.
    pub fn get(
        mut self,
        path: impl Into<String>,
        handler: impl for<'tx> Fn(P::Reader<'tx, Snapshot>, &CustomReq) -> CustomResult
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.custom.push((path.into(), Arc::new(handler)));
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
    pub fn run(self) {
        serve_blocking(self.into_router())
    }
}

/// A fold [`KeyedStream`] wrapped in a generated HTTP server: writes are
/// upsert/remove by primary key, and replacing or deleting a record
/// retracts the old one from every view automatically.
pub struct KeyedApp<K: Clone, V: Clone, P: Push<Keyed<K, V>>> {
    stream: KeyedStream<K, V, P>,
    custom: Vec<(String, CustomHandler<Keyed<K, V>, P>)>,
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

    /// Add a custom GET route; see [`App::get`].
    pub fn get(
        mut self,
        path: impl Into<String>,
        handler: impl for<'tx> Fn(P::Reader<'tx, Snapshot>, &CustomReq) -> CustomResult
        + Send
        + Sync
        + 'static,
    ) -> Self {
        self.custom.push((path.into(), Arc::new(handler)));
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
