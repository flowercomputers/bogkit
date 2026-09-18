//! Unix-worker transport adapter. Public reads are restricted to declared operations.
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as RoutePath, Query as UrlQuery, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bog_definition::{Action, Definition, Limits, Terminal};
use bog_runtime::{Mutation, Query, Record, Runtime, RuntimeError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
struct Inner {
    runtime: Runtime,
    seq: u64,
}
struct Shared {
    inner: Mutex<Inner>,
    stopping: AtomicBool,
    revision: u64,
    data_dir: std::path::PathBuf,
    boot: String,
    changed: tokio::sync::watch::Sender<u64>,
    waits: tokio::sync::Semaphore,
}
#[derive(Clone)]
pub struct ConfiguredService {
    shared: Arc<Shared>,
}
impl ConfiguredService {
    pub fn open(
        path: &Path,
        definition: Definition,
        revision: u64,
        limit: u64,
    ) -> Result<Self, RuntimeError> {
        Self::open_with_limits(path, definition, revision, limit, Limits::default())
    }
    pub fn open_with_limits(
        path: &Path,
        definition: Definition,
        revision: u64,
        limit: u64,
        limits: Limits,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    runtime: Runtime::open_with_limits(path, definition, limit, limits)?,
                    seq: 0,
                }),
                stopping: AtomicBool::new(false),
                revision,
                data_dir: path.into(),
                boot: uuid::Uuid::new_v4().to_string(),
                changed: tokio::sync::watch::channel(0).0,
                waits: tokio::sync::Semaphore::new(8),
            }),
        })
    }
    pub fn router(&self) -> Router {
        Router::new()
            .route("/healthz", get(|| async { "ok" }))
            .route("/definition", get(definition))
            .route("/schema", get(schema))
            .route("/openapi.json", get(schema))
            .route("/operations/{name}", post(operation))
            .route("/docs/{key}", get(get_doc).put(put_doc).delete(remove_doc))
            .route("/batch", post(batch))
            .route("/views/{name}", get(view))
            .route("/views/{name}/{key}", get(view_key))
            .route("/_cloud/usage", get(usage))
            .route("/_cloud/export", get(export))
            .route("/_cloud/import", post(import))
            .route("/_cloud/sequence", get(sequence))
            .layer(DefaultBodyLimit::max(crate::MAX_REQUEST_BYTES))
            .with_state(self.shared.clone())
    }
    /// Public loopback surface; operator-only controls and full definitions stay private.
    pub fn local_router(&self) -> Router {
        self.router().layer(axum::middleware::from_fn(
            |req: axum::extract::Request, next: axum::middleware::Next| async move {
                if req.uri().path().starts_with("/_cloud/") || req.uri().path() == "/definition" {
                    return StatusCode::NOT_FOUND.into_response();
                }
                next.run(req).await
            },
        ))
    }
    pub fn begin_shutdown(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.changed.send_modify(|_| {});
    }
    pub fn shutdown(self) -> Result<(), RuntimeError> {
        self.shared
            .inner
            .lock()
            .map_err(|_| RuntimeError("runtime lock poisoned".into()))?
            .runtime
            .checkpoint()
    }
}
fn error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"error":msg}))).into_response()
}
fn runtime_error(e: RuntimeError) -> Response {
    let msg = e.0;
    if msg.contains("checkpoint") || msg.contains("I/O") || msg.contains("requires restart") {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage unavailable; restart required",
        );
    }
    error(
        if msg.contains("quota") || msg.contains("exceeds 1 MiB") {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        },
        &msg,
    )
}
fn locked(shared: &Shared, f: impl FnOnce(&mut Inner) -> Response) -> Response {
    if shared.stopping.load(Ordering::SeqCst) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "worker stopping");
    }
    match shared.inner.lock() {
        Ok(mut inner) => f(&mut inner),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "runtime unavailable"),
    }
}
fn permitted(inner: &Inner, target: &str, action: Action) -> bool {
    inner
        .runtime
        .definition()
        .expose
        .values()
        .any(|op| op.target == target && op.action == action)
}
fn response(inner: &Inner, value: Value) -> Response {
    let body = json!({"seq":inner.seq,"data":value});
    if body.to_string().len() > 4 * 1024 * 1024 - 4096 {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "reduce the requested page size",
        );
    }
    Json(body).into_response()
}
async fn definition(State(s): State<Arc<Shared>>) -> Response {
    locked(&s, |i| {
        Json(json!({"definition":i.runtime.definition(),"digest":i.runtime.definition().digest().unwrap(),"revision":s.revision,"limits":i.runtime.limits(),"operations":i.runtime.definition().operation_metadata_with_limits(i.runtime.limits())})).into_response()
    })
}
async fn schema(State(s): State<Arc<Shared>>) -> Response {
    locked(&s, |i| {
        Json(json!({"definition_version":i.runtime.definition().version,"revision":s.revision,"digest":i.runtime.definition().digest().unwrap(),"limits":i.runtime.limits(),"operations":i.runtime.definition().operation_metadata_with_limits(i.runtime.limits())})).into_response()
    })
}
async fn sequence(State(s): State<Arc<Shared>>) -> Response {
    locked(&s, |i| Json(json!({"seq":i.seq})).into_response())
}
async fn operation(
    State(s): State<Arc<Shared>>,
    RoutePath(name): RoutePath<String>,
    Json(body): Json<Value>,
) -> Response {
    let is_wait = match s.inner.lock() {
        Ok(i) => i
            .runtime
            .definition()
            .expose
            .get(&name)
            .is_some_and(|op| op.action == Action::Wait),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "runtime unavailable"),
    };
    if is_wait {
        return wait_operation(s, name, body).await;
    }
    locked(&s, |i| {
        let Some(op) = i.runtime.definition().expose.get(&name).cloned() else {
            return error(StatusCode::NOT_FOUND, "operation not exposed");
        };
        match i.runtime.execute(&name, body) {
            Ok(value) => {
                if matches!(op.action, Action::Put | Action::Remove | Action::Batch) {
                    i.seq += 1;
                    s.changed.send_replace(i.seq);
                }
                response(i, value)
            }
            Err(e) => runtime_error(e),
        }
    })
}
async fn get_doc(State(s): State<Arc<Shared>>, RoutePath(key): RoutePath<String>) -> Response {
    locked(&s, |i| {
        let input = i.runtime.definition().input.clone();
        if !permitted(i, &input, Action::Get) {
            return error(StatusCode::NOT_FOUND, "operation not exposed");
        }
        if let Err(e) = bog_runtime::validate_key(&key) {
            return runtime_error(e);
        }
        match i.runtime.query(
            &input,
            Action::Get,
            &Query {
                key: Some(key),
                ..Default::default()
            },
        ) {
            Ok(Value::Null) => error(StatusCode::NOT_FOUND, "not found"),
            Ok(v) => response(i, v),
            Err(e) => runtime_error(e),
        }
    })
}
async fn put_doc(
    State(s): State<Arc<Shared>>,
    RoutePath(key): RoutePath<String>,
    Json(data): Json<bog_runtime::JsonDocument>,
) -> Response {
    locked(&s, |i| {
        if !permitted(i, &i.runtime.definition().input, Action::Put) {
            return error(StatusCode::NOT_FOUND, "operation not exposed");
        }
        let replaced = i.runtime.get(&key).is_some();
        match i.runtime.mutate(&[Mutation::Upsert { key, data }]) {
            Ok(()) => {
                i.seq += 1;
                s.changed.send_replace(i.seq);
                Json(json!({"seq":i.seq,"replaced":replaced})).into_response()
            }
            Err(e) => runtime_error(e),
        }
    })
}
async fn remove_doc(State(s): State<Arc<Shared>>, RoutePath(key): RoutePath<String>) -> Response {
    locked(&s, |i| {
        if !permitted(i, &i.runtime.definition().input, Action::Remove) {
            return error(StatusCode::NOT_FOUND, "operation not exposed");
        }
        let removed = i.runtime.get(&key).is_some();
        match i.runtime.mutate(&[Mutation::Remove { key }]) {
            Ok(()) => {
                i.seq += 1;
                s.changed.send_replace(i.seq);
                Json(json!({"seq":i.seq,"removed":removed})).into_response()
            }
            Err(e) => runtime_error(e),
        }
    })
}
async fn batch(State(s): State<Arc<Shared>>, Json(ops): Json<Vec<Mutation>>) -> Response {
    locked(&s, |i| {
        if !permitted(i, &i.runtime.definition().input, Action::Batch) {
            return error(StatusCode::NOT_FOUND, "operation not exposed");
        }
        match i.runtime.mutate(&ops) {
            Ok(()) => {
                i.seq += 1;
                s.changed.send_replace(i.seq);
                Json(json!({"seq":i.seq,"applied":ops.len()})).into_response()
            }
            Err(e) => runtime_error(e),
        }
    })
}
#[derive(Default, Deserialize)]
struct Page {
    limit: Option<usize>,
    offset: Option<usize>,
    query: Option<String>,
}
async fn view(
    State(s): State<Arc<Shared>>,
    RoutePath(name): RoutePath<String>,
    UrlQuery(p): UrlQuery<Page>,
) -> Response {
    locked(&s, |i| {
        let action = match i
            .runtime
            .definition()
            .resources
            .get(&name)
            .map(|r| &r.terminal)
        {
            Some(Terminal::Table) => Action::List,
            Some(Terminal::Ranked { .. }) => Action::Top,
            Some(Terminal::Bm25 { .. } | Terminal::Semantic { .. }) => Action::Search,
            _ => Action::Read,
        };
        if !permitted(i, &name, action) {
            return error(StatusCode::NOT_FOUND, "operation not exposed");
        }
        match i.runtime.query(
            &name,
            action,
            &Query {
                limit: p.limit,
                offset: p.offset,
                query: p.query,
                ..Default::default()
            },
        ) {
            Ok(v) => response(i, v),
            Err(e) => runtime_error(e),
        }
    })
}
async fn view_key(
    State(s): State<Arc<Shared>>,
    RoutePath((name, key)): RoutePath<(String, String)>,
) -> Response {
    locked(&s, |i| {
        if !permitted(i, &name, Action::Get) {
            return error(StatusCode::NOT_FOUND, "operation not exposed");
        }
        match i.runtime.query(
            &name,
            Action::Get,
            &Query {
                key: Some(key),
                ..Default::default()
            },
        ) {
            Ok(Value::Null) => error(StatusCode::NOT_FOUND, "not found"),
            Ok(v) => response(i, v),
            Err(e) => runtime_error(e),
        }
    })
}
async fn usage(State(s): State<Arc<Shared>>) -> Response {
    locked(&s, |i| {
        let used = i.runtime.logical_bytes();
        let count = i.runtime.export(0).record_count;
        let vectors = i.runtime.vector_count();
        response(
            i,
            json!({"limits":i.runtime.limits(),"logical_bytes":used,"limit_bytes":i.runtime.limit(),"over_limit":used>i.runtime.limit(),"source_records":count,"resource_count":i.runtime.definition().resources.len(),"vector_count":vectors,"vector_payload_bytes":vectors*512*4,"vector_payload_scope":"raw f32 payload only; excludes graph and storage overhead","physical_store_bytes":physical_bytes(&s.data_dir),"derived_data_bytes":null,"derived_data_bytes_reason":"Fjall shares files across source and derived keyspaces","encoder":if vectors>0{Some(bog_definition::SEMANTIC_MODEL)}else{None}}),
        )
    })
}
#[derive(Deserialize)]
struct Offset {
    #[serde(default)]
    offset: usize,
}
async fn export(State(s): State<Arc<Shared>>, UrlQuery(q): UrlQuery<Offset>) -> Response {
    locked(&s, |i| Json(i.runtime.export(q.offset)).into_response())
}
#[derive(Deserialize)]
struct Import {
    records: Vec<Record>,
}
async fn import(State(s): State<Arc<Shared>>, Json(body): Json<Import>) -> Response {
    locked(&s, |i| {
        let ops: Vec<_> = body
            .records
            .into_iter()
            .map(|r| Mutation::Upsert {
                key: r.key,
                data: r.value,
            })
            .collect();
        match i.runtime.mutate(&ops) {
            Ok(()) => {
                i.seq += 1;
                s.changed.send_replace(i.seq);
                Json(json!({"seq":i.seq,"applied":ops.len()})).into_response()
            }
            Err(e) => runtime_error(e),
        }
    })
}

fn physical_bytes(path: &Path) -> Option<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path).ok()? {
        let entry = entry.ok()?;
        let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
        if metadata.is_symlink() {
            continue;
        }
        total = total.checked_add(if metadata.is_dir() {
            physical_bytes(&entry.path())?
        } else {
            metadata.len()
        })?;
    }
    Some(total)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitRequest {
    cursor: Option<String>,
    timeout_ms: Option<u64>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitCursor {
    v: u8,
    boot: String,
    operation: String,
    seq: u64,
}
async fn wait_operation(s: Arc<Shared>, name: String, body: Value) -> Response {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let request: WaitRequest = match serde_json::from_value(body) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid wait request"),
    };
    let timeout = request.timeout_ms.unwrap_or(25000);
    if timeout > 30000 {
        return error(StatusCode::BAD_REQUEST, "wait timeout exceeds 30000 ms");
    }
    let previous = match request.cursor {
        None => None,
        Some(cursor) => {
            if cursor.len() > 1024 {
                return error(StatusCode::BAD_REQUEST, "invalid wait cursor");
            }
            let decoded = URL_SAFE_NO_PAD
                .decode(cursor)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<WaitCursor>(&bytes).ok());
            match decoded {
                Some(c) if c.v == 1 => Some(c),
                _ => return error(StatusCode::BAD_REQUEST, "invalid wait cursor"),
            }
        }
    };
    let _permit = match s.waits.try_acquire() {
        Ok(p) => p,
        Err(_) => return error(StatusCode::TOO_MANY_REQUESTS, "wait capacity reached"),
    };
    let mut notify = s.changed.subscribe();
    let initial = *notify.borrow_and_update();
    let reset = previous
        .as_ref()
        .is_some_and(|p| p.boot != s.boot || p.operation != name || p.seq > initial);
    let changed = previous.as_ref().is_some_and(|p| reset || p.seq != initial);
    if previous.is_some() && !changed && !s.stopping.load(Ordering::SeqCst) {
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(timeout), notify.changed()).await;
    }
    if s.stopping.load(Ordering::SeqCst) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "worker stopping");
    }
    let seq = *notify.borrow();
    let cursor = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&WaitCursor {
            v: 1,
            boot: s.boot.clone(),
            operation: name,
            seq,
        })
        .unwrap(),
    );
    Json(json!({"seq":seq,"data":{"seq":seq,"cursor":cursor,"changed":previous.as_ref().is_some_and(|p|reset||p.seq!=seq),"reset":reset}})).into_response()
}
