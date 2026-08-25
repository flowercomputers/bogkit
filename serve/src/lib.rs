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
//! # Daemon mode
//!
//! The store is exclusively locked per process, so this server is the
//! natural shared access point for multiple client processes (agents,
//! CLIs). Three builder options support running it as a spawn-on-demand
//! daemon:
//!
//! - [`bind`](App::bind) — listen on a unix domain socket instead of TCP
//!   ([`Bind::Unix`]).
//! - [`idle_timeout`](App::idle_timeout) — checkpoint the store and exit 0
//!   after a quiet period, so daemons don't accumulate.
//! - [`on_schema_drift`](App::on_schema_drift) — wipe and rebuild instead
//!   of panicking when the pipeline changed
//!   ([`SchemaDrift::WipeAndRebuild`]), for stores holding derived state.
//!
//! While running, the server advertises itself in a discovery sidecar
//! (see [`sidecar_path`]). The spawn-or-connect protocol for clients:
//! try the sidecar's address; if that fails, spawn the server; if the
//! spawned server dies because the store is locked
//! ([`fjall::Error::Locked`](fold::fjall::Error) from
//! [`Stream::try_new`](fold::stream::Stream::try_new) — someone else won
//! the race), re-read the sidecar and connect.
//!
//! Custom [`App::get`]/[`KeyedApp::get`] handlers receive the pipeline's
//! readers plus a typed query string; [`App::post`]/[`KeyedApp::post`]
//! handlers receive the write transaction plus a typed body — `Err` rolls
//! the transaction back. Request and response schemas are captured at
//! registration so custom routes appear fully typed in `/openapi.json`.

mod http;
mod openapi;
mod search;
mod views;

pub use search::{TextQuery, TextQueryReader, VectorSearch};
pub use views::{SearchMode, ViewKind, ViewQuery, ViewRead, ViewSpec, Views};

use fold::fjall::Snapshot;
use fold::pipeline::{Keyed, Push};
use fold::stream::{KeyedStream, Stream};
use http::{CustomRoute, KeyedCustom};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Typed "no query parameters" for custom GET routes that take none:
/// `.get("/path", |readers, _: NoParams| ...)`.
#[derive(serde::Deserialize, JsonSchema)]
pub struct NoParams {}

/// Where [`run`](App::run) listens. Defaults to TCP on `$PORT` (7877).
#[derive(Clone, Debug)]
pub enum Bind {
    /// TCP on `0.0.0.0:port`.
    Tcp(u16),
    /// A unix domain socket at this path (created on bind, removed on
    /// graceful shutdown; a stale file from a crashed run is replaced).
    Unix(std::path::PathBuf),
}

/// What to do when the data dir was written by a pipeline whose schema
/// fingerprint no longer matches (see the `/schema` route).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchemaDrift {
    /// Refuse to start, loudly. The right default for durable data.
    #[default]
    Panic,
    /// Delete all persisted state and start empty. The right mode for
    /// derived state (caches, search indices) that the writers can simply
    /// re-feed after an upgrade.
    WipeAndRebuild,
}

/// Options consumed by [`run`](App::run); see the builder methods on
/// [`App`]/[`KeyedApp`].
#[derive(Default)]
struct ServeOpts {
    bind: Option<Bind>,
    idle_timeout: Option<std::time::Duration>,
    drift: SchemaDrift,
}

pub(crate) fn to_value<T: Serialize>(t: T) -> Value {
    serde_json::to_value(t).expect("custom route Ok type serializes to JSON")
}

/// A fold [`Stream`] wrapped in a generated HTTP server.
pub struct App<D: Clone, P: Push<D>> {
    stream: Stream<D, P>,
    custom: Vec<CustomRoute<D, P, Stream<D, P>>>,
    db_path: std::path::PathBuf,
    opts: ServeOpts,
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
            opts: ServeOpts::default(),
        }
    }

    /// Where [`run`](App::run) listens; overrides `$PORT`.
    pub fn bind(mut self, bind: Bind) -> Self {
        self.opts.bind = Some(bind);
        self
    }

    /// Exit cleanly after this long without a request (and with no live SSE
    /// watchers): drains writes, checkpoints the store, removes the
    /// discovery sidecar and socket file, and exits 0. For daemons spawned
    /// on demand; see the crate docs.
    pub fn idle_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.opts.idle_timeout = Some(timeout);
        self
    }

    /// What to do when the data dir's schema fingerprint doesn't match this
    /// pipeline; the default is [`SchemaDrift::Panic`].
    pub fn on_schema_drift(mut self, drift: SchemaDrift) -> Self {
        self.opts.drift = drift;
        self
    }

    /// Typed custom GET: readers + query struct in, `Ok` value in the
    /// `{seq, data}` envelope. Both schemas land in `/openapi.json`.
    pub fn get<Q, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        Q: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'tx> Fn(P::Reader<'tx, Snapshot>, Q) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        self.custom.push(http::custom_get(path.into(), handler));
        self
    }

    /// Typed custom POST over a write transaction. `Err` rolls the whole
    /// transaction back (see crate-level docs).
    pub fn post<B, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        B: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'g, 'tx> Fn(&mut fold::stream::Tx<'g, 'tx, D, P>, B) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        self.custom.push(http::custom_write(
            path.into(),
            move |stream: &mut Stream<D, P>, body| stream.try_wtx(|tx| handler(tx, body)),
        ));
        self
    }

    /// Every generated route as an [`axum::Router`], no listener attached.
    ///
    /// # Panics
    /// If the data dir was written by a different pipeline (schema
    /// fingerprint mismatch) and the drift mode is [`SchemaDrift::Panic`]
    /// — see the `/schema` route.
    pub fn into_router(self) -> axum::Router {
        http::router(self.stream, self.custom, &self.db_path, self.opts.drift).0
    }

    /// Serve blocking forever on the configured [`bind`](App::bind)
    /// (default: TCP `0.0.0.0:$PORT`, port 7877), or until the configured
    /// [`idle_timeout`](App::idle_timeout) exits the process.
    pub fn run(mut self) {
        let opts = std::mem::take(&mut self.opts);
        let db_path = self.db_path.clone();
        let (router, lifecycle) = http::router(self.stream, self.custom, &self.db_path, opts.drift);
        serve_blocking(router, lifecycle, opts, &db_path)
    }
}

/// A fold [`KeyedStream`] wrapped in a generated HTTP server: writes are
/// upsert/remove by primary key, and replacing or deleting a record
/// retracts the old one from every view automatically.
pub struct KeyedApp<K: Clone, V: Clone, P: Push<Keyed<K, V>>> {
    stream: KeyedStream<K, V, P>,
    custom: Vec<KeyedCustom<K, V, P>>,
    db_path: std::path::PathBuf,
    opts: ServeOpts,
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
            opts: ServeOpts::default(),
        }
    }

    /// Where [`run`](KeyedApp::run) listens; see [`App::bind`].
    pub fn bind(mut self, bind: Bind) -> Self {
        self.opts.bind = Some(bind);
        self
    }

    /// Exit cleanly when idle; see [`App::idle_timeout`].
    pub fn idle_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.opts.idle_timeout = Some(timeout);
        self
    }

    /// Schema-drift handling; see [`App::on_schema_drift`].
    pub fn on_schema_drift(mut self, drift: SchemaDrift) -> Self {
        self.opts.drift = drift;
        self
    }

    /// Typed custom GET; see [`App::get`].
    pub fn get<Q, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        Q: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'tx> Fn(P::Reader<'tx, Snapshot>, Q) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        self.custom.push(http::custom_get(path.into(), handler));
        self
    }

    /// Typed custom POST over the keyed write transaction; see [`App::post`].
    pub fn post<B, T, F>(mut self, path: impl Into<String>, handler: F) -> Self
    where
        B: DeserializeOwned + JsonSchema,
        T: Serialize + JsonSchema,
        F: for<'a, 'g, 'tx> Fn(
                &mut fold::stream::KeyedTx<'a, 'g, 'tx, K, V, P>,
                B,
            ) -> Result<T, (u16, String)>
            + Send
            + Sync
            + 'static,
    {
        self.custom.push(http::custom_write(
            path.into(),
            move |stream: &mut KeyedStream<K, V, P>, body| stream.try_wtx(|tx| handler(tx, body)),
        ));
        self
    }

    /// Every generated route as an [`axum::Router`], no listener attached.
    ///
    /// # Panics
    /// If the data dir was written by a different pipeline (schema
    /// fingerprint mismatch) and the drift mode is [`SchemaDrift::Panic`]
    /// — see the `/schema` route.
    pub fn into_router(self) -> axum::Router {
        http::router_keyed(self.stream, self.custom, &self.db_path, self.opts.drift).0
    }

    /// Serve blocking forever; see [`App::run`].
    pub fn run(mut self) {
        let opts = std::mem::take(&mut self.opts);
        let db_path = self.db_path.clone();
        let (router, lifecycle) =
            http::router_keyed(self.stream, self.custom, &self.db_path, opts.drift);
        serve_blocking(router, lifecycle, opts, &db_path)
    }
}

/// Where state lives: `$BOG_DATA_DIR` (set by `bogkit dev`) or `./bog.db`.
pub fn data_dir() -> std::path::PathBuf {
    std::env::var_os("BOG_DATA_DIR")
        .map(Into::into)
        .unwrap_or_else(|| "bog.db".into())
}

/// Where a running server advertises itself: a `<data dir>.serve.json`
/// sibling of the data dir (like the `.schema` sidecar) holding
/// `{"pid", "bind": {"tcp": port} | {"unix": path}, "fingerprint",
/// "version"}`.
///
/// The sidecar is written after the listener binds and removed on graceful
/// (idle-timeout) shutdown. It can outlive a crashed server: treat it as
/// advisory and a dead `pid` as "not running" — the store's file lock, not
/// this file, is what guarantees at most one server.
pub fn sidecar_path(db_path: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    let mut p = db_path.as_ref().as_os_str().to_owned();
    p.push(".serve.json");
    p.into()
}

/// Wall-clock idle tracking: the request middleware stamps it, the watchdog
/// reads it. Millisecond resolution is plenty for multi-minute timeouts.
struct Activity {
    start: std::time::Instant,
    last_ms: std::sync::atomic::AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Activity {
            start: std::time::Instant::now(),
            last_ms: 0.into(),
        }
    }

    fn touch(&self) {
        let ms = self.start.elapsed().as_millis() as u64;
        self.last_ms
            .store(ms, std::sync::atomic::Ordering::Relaxed);
    }

    fn idle_for(&self) -> std::time::Duration {
        let now = self.start.elapsed().as_millis() as u64;
        let last = self.last_ms.load(std::sync::atomic::Ordering::Relaxed);
        std::time::Duration::from_millis(now.saturating_sub(last))
    }
}

fn serve_blocking(
    router: axum::Router,
    lifecycle: http::Lifecycle,
    opts: ServeOpts,
    db_path: &std::path::Path,
) {
    let bind = opts.bind.unwrap_or_else(|| {
        Bind::Tcp(
            std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(7877),
        )
    });

    let activity = std::sync::Arc::new(Activity::new());
    let touch = activity.clone();
    let router = router.layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            touch.touch();
            next.run(req)
        },
    ));

    let sidecar = sidecar_path(db_path);
    // files a graceful shutdown must remove so clients don't chase a ghost
    let mut cleanup = vec![sidecar.clone()];
    if let Bind::Unix(path) = &bind {
        cleanup.push(path.clone());
    }

    let rt = tokio::runtime::Runtime::new().expect("starting tokio runtime");
    rt.block_on(async move {
        match &bind {
            Bind::Tcp(port) => {
                let listener = tokio::net::TcpListener::bind(("0.0.0.0", *port))
                    .await
                    .unwrap_or_else(|e| panic!("binding port {port}: {e}"));
                write_sidecar(&sidecar, &bind, &lifecycle.fingerprint);
                if let Some(timeout) = opts.idle_timeout {
                    tokio::spawn(watchdog(activity, timeout, lifecycle, cleanup));
                }
                println!("bog-serve on http://localhost:{port} — routes at /openapi.json");
                axum::serve(listener, router).await.expect("serving");
            }
            Bind::Unix(path) => {
                // a leftover socket from a crashed run refuses rebinding;
                // the store's file lock already guarantees we're alone here
                let _ = std::fs::remove_file(path);
                let listener = tokio::net::UnixListener::bind(path)
                    .unwrap_or_else(|e| panic!("binding {}: {e}", path.display()));
                write_sidecar(&sidecar, &bind, &lifecycle.fingerprint);
                if let Some(timeout) = opts.idle_timeout {
                    tokio::spawn(watchdog(activity, timeout, lifecycle, cleanup));
                }
                println!(
                    "bog-serve on unix socket {} — routes at /openapi.json",
                    path.display()
                );
                axum::serve(listener, router).await.expect("serving");
            }
        }
    });
}

fn write_sidecar(path: &std::path::Path, bind: &Bind, fingerprint: &str) {
    let bind = match bind {
        Bind::Tcp(port) => serde_json::json!({ "tcp": port }),
        Bind::Unix(path) => serde_json::json!({ "unix": path }),
    };
    let doc = serde_json::json!({
        "pid": std::process::id(),
        "bind": bind,
        "fingerprint": fingerprint,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if let Err(e) = std::fs::write(path, doc.to_string()) {
        eprintln!("bog-serve: could not write discovery sidecar: {e}");
    }
}

/// Exit the process once no request has arrived for `timeout` and no SSE
/// watcher is connected: drain in-flight writes, checkpoint the store (so
/// the next open replays a minimal journal), remove the sidecar and socket
/// file, exit 0. In-flight response bodies are cut — spawn-or-connect
/// clients must treat a dropped connection as "daemon gone, respawn".
async fn watchdog(
    activity: std::sync::Arc<Activity>,
    timeout: std::time::Duration,
    lifecycle: http::Lifecycle,
    cleanup: Vec<std::path::PathBuf>,
) {
    let period = (timeout / 10).clamp(
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(60),
    );
    let mut interval = tokio::time::interval(period);
    loop {
        interval.tick().await;
        if activity.idle_for() >= timeout && (lifecycle.watchers)() == 0 {
            break;
        }
    }
    eprintln!("bog-serve: idle for {timeout:?}, shutting down");
    tokio::task::spawn_blocking(lifecycle.checkpoint)
        .await
        .expect("checkpoint on idle shutdown");
    for path in &cleanup {
        let _ = std::fs::remove_file(path);
    }
    std::process::exit(0);
}
