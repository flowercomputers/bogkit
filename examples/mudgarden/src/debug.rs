use std::convert::Infallible;
use std::future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::commands::{self, Command};
use crate::content::GameContent;
use crate::domain::{ActorId, ActorState, DebugSnapshot, WorldEvent};
use crate::server::should_deliver_live_event;
use crate::service::{WorldClockControl, WorldHandle};

const INDEX_HTML: &str = include_str!("../debug/index.html");
const SESSIONS_HTML: &str = include_str!("../debug/sessions.html");

#[derive(Serialize)]
struct Health {
    status: &'static str,
    world_time: u64,
}

#[derive(Serialize)]
struct ClockControlResponse {
    paused: bool,
    tick_seconds: u64,
}

impl From<WorldClockControl> for ClockControlResponse {
    fn from(control: WorldClockControl) -> Self {
        Self {
            paused: control.paused,
            tick_seconds: control.tick_interval.as_secs(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateClockRequest {
    paused: Option<bool>,
    tick_seconds: Option<u64>,
}

#[derive(Serialize)]
struct LiveSnapshot {
    snapshot: DebugSnapshot,
    clock: ClockControlResponse,
}

#[derive(Clone)]
struct DebugState {
    world: WorldHandle,
    copy: Arc<RwLock<CopyState>>,
    content: Arc<GameContent>,
    content_path: Arc<PathBuf>,
}

struct CopyState {
    content: GameContent,
    restart_required: bool,
}

#[derive(Serialize)]
struct CopyDocument<'a> {
    path: String,
    restart_required: bool,
    content: &'a GameContent,
}

#[derive(Deserialize)]
struct SaveCopyRequest {
    content: serde_json::Value,
}

#[derive(Serialize)]
struct SaveCopyResponse {
    saved: bool,
    path: String,
    restart_required: bool,
}

#[derive(Serialize)]
struct ApiError {
    error: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateSessionRequest {
    name: String,
}

#[derive(Serialize)]
struct BrowserSessionResponse {
    actor: ActorState,
    lines: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionCommandRequest {
    command: String,
}

#[derive(Serialize)]
struct SessionCommandResponse {
    lines: Vec<String>,
    quit: bool,
}

pub async fn serve(
    world: WorldHandle,
    bind: Option<SocketAddr>,
    content: Arc<GameContent>,
    content_path: PathBuf,
) -> Result<()> {
    let Some(bind) = bind else {
        return future::pending::<Result<()>>().await;
    };
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let state = DebugState {
        world,
        copy: Arc::new(RwLock::new(CopyState {
            content: content.as_ref().clone(),
            restart_required: false,
        })),
        content,
        content_path: Arc::new(content_path),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/sessions", get(sessions))
        .route("/api/debug/health", get(health))
        .route("/api/debug/snapshot", get(snapshot))
        .route("/api/debug/stream", get(snapshot_stream))
        .route("/api/debug/clock", get(get_clock).put(update_clock))
        .route("/api/debug/clock/step", post(step_clock))
        .route("/api/debug/content", get(get_content).put(save_content))
        .route("/api/debug/sessions", post(create_session))
        .route(
            "/api/debug/sessions/{actor_id}/command",
            post(run_session_command),
        )
        .route(
            "/api/debug/sessions/{actor_id}/stream",
            get(session_event_stream),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(state);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> impl IntoResponse {
    (
        [(CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Html(INDEX_HTML),
    )
}

async fn sessions() -> impl IntoResponse {
    (
        [(CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Html(SESSIONS_HTML),
    )
}

async fn create_session(
    State(state): State<DebugState>,
    axum::Json(request): axum::Json<CreateSessionRequest>,
) -> Response {
    let name = request.name.trim();
    if name.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "name is required".to_string());
    }
    let actor = match state.world.ensure_human(name.to_string(), None).await {
        Ok(actor) => actor,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, error),
    };
    let mut lines = vec![
        state.content.game.tagline.clone(),
        state.content.text("ui.command_hint").to_string(),
    ];
    for command in [Command::Changes, Command::Look(None)] {
        match state.world.execute(actor.id, command).await {
            Ok(output) => lines.extend(output.lines),
            Err(error) => return api_error(StatusCode::SERVICE_UNAVAILABLE, error),
        }
    }
    axum::Json(BrowserSessionResponse { actor, lines }).into_response()
}

async fn run_session_command(
    AxumPath(actor_id): AxumPath<u64>,
    State(state): State<DebugState>,
    axum::Json(request): axum::Json<SessionCommandRequest>,
) -> Response {
    let command = match commands::parse_with_content(request.command.trim(), &state.content) {
        Ok(command) => command,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, error),
    };
    match state.world.execute(ActorId(actor_id), command).await {
        Ok(output) => axum::Json(SessionCommandResponse {
            lines: output.lines,
            quit: output.quit,
        })
        .into_response(),
        Err(error) => api_error(StatusCode::BAD_REQUEST, error),
    }
}

async fn session_event_stream(
    AxumPath(actor_id): AxumPath<u64>,
    State(state): State<DebugState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let actor_id = ActorId(actor_id);
    let events = state.world.subscribe();
    let stream = futures::stream::unfold((events, actor_id), |(mut events, actor_id)| async move {
        loop {
            match events.recv().await {
                Ok(event) if should_deliver_live_event(&event, actor_id) => {
                    let event = session_event(event);
                    return Some((Ok(event), (events, actor_id)));
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

fn session_event(event: WorldEvent) -> Event {
    match serde_json::to_string(&event) {
        Ok(event) => Event::default().event("world-event").data(event),
        Err(error) => Event::default()
            .event("session-error")
            .data(error.to_string()),
    }
}

async fn health(State(state): State<DebugState>) -> Response {
    match state.world.debug_snapshot().await {
        Ok(snapshot) => axum::Json(Health {
            status: "ok",
            world_time: snapshot.clock.now,
        })
        .into_response(),
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    }
}

async fn snapshot(State(state): State<DebugState>) -> Response {
    match state.world.debug_snapshot().await {
        Ok(snapshot) => json_no_store(snapshot),
        Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error).into_response(),
    }
}

async fn get_clock(State(state): State<DebugState>) -> axum::Json<ClockControlResponse> {
    axum::Json(state.world.clock_control().into())
}

async fn update_clock(
    State(state): State<DebugState>,
    axum::Json(request): axum::Json<UpdateClockRequest>,
) -> Response {
    let tick_interval = match request.tick_seconds {
        Some(0) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "tick_seconds must be between 1 and 3600".to_string(),
            );
        }
        Some(seconds @ 1..=3600) => Some(Duration::from_secs(seconds)),
        Some(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "tick_seconds must be between 1 and 3600".to_string(),
            );
        }
        None => None,
    };
    axum::Json(ClockControlResponse::from(
        state.world.set_clock_control(request.paused, tick_interval),
    ))
    .into_response()
}

async fn step_clock(State(state): State<DebugState>) -> Response {
    if let Err(error) = state.world.tick_now().await {
        return api_error(StatusCode::SERVICE_UNAVAILABLE, error);
    }
    match state.world.debug_snapshot().await {
        Ok(snapshot) => axum::Json(LiveSnapshot {
            snapshot,
            clock: state.world.clock_control().into(),
        })
        .into_response(),
        Err(error) => api_error(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

async fn snapshot_stream(
    State(state): State<DebugState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let changes = state.world.subscribe_changes();
    let stream = futures::stream::unfold(
        (state.world, changes, true),
        |(world, mut changes, initial)| async move {
            if !initial {
                match changes.recv().await {
                    Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }

            let event = match world.debug_snapshot().await {
                Ok(snapshot) => match serde_json::to_string(&LiveSnapshot {
                    snapshot,
                    clock: world.clock_control().into(),
                }) {
                    Ok(snapshot) => Event::default().event("snapshot").data(snapshot),
                    Err(error) => Event::default()
                        .event("snapshot-error")
                        .data(error.to_string()),
                },
                Err(error) => Event::default().event("snapshot-error").data(error),
            };
            Some((Ok(event), (world, changes, false)))
        },
    );
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn get_content(State(state): State<DebugState>) -> Response {
    let copy = state.copy.read().await;
    axum::Json(CopyDocument {
        path: state.content_path.display().to_string(),
        restart_required: copy.restart_required,
        content: &copy.content,
    })
    .into_response()
}

async fn save_content(
    State(state): State<DebugState>,
    axum::Json(request): axum::Json<SaveCopyRequest>,
) -> Response {
    let source = match serde_json::to_string_pretty(&request.content) {
        Ok(source) => source,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, error.to_string()),
    };
    let content = match GameContent::parse(&source) {
        Ok(content) => content,
        Err(error) => return api_error(StatusCode::BAD_REQUEST, format!("{error:#}")),
    };
    if let Err(error) = write_content(&state.content_path, &source) {
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not save content: {error:#}"),
        );
    }
    let mut copy = state.copy.write().await;
    copy.content = content;
    copy.restart_required = true;
    axum::Json(SaveCopyResponse {
        saved: true,
        path: state.content_path.display().to_string(),
        restart_required: true,
    })
    .into_response()
}

fn write_content(path: &Path, source: &str) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, format!("{source}\n"))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

fn api_error(status: StatusCode, error: String) -> Response {
    (status, axum::Json(ApiError { error })).into_response()
}

fn json_no_store(snapshot: DebugSnapshot) -> Response {
    let mut response = axum::Json(snapshot).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn snapshot_exposes_world_state_without_ssh_fingerprints() {
        let directory = tempdir().unwrap();
        let world = WorldHandle::start(directory.path().join("world"), Duration::from_secs(3_600));
        let actor = world
            .ensure_human("garden-debugger", Some("SHA256:private".to_string()))
            .await
            .unwrap();

        let snapshot = world.debug_snapshot().await.unwrap();
        let debug_actor = snapshot
            .actors
            .iter()
            .find(|candidate| candidate.id == actor.id)
            .unwrap();
        assert_eq!(debug_actor.auth_fingerprint, None);
        assert!(!snapshot.rooms.is_empty());
        assert!(!snapshot.world_cells.is_empty());

        world.shutdown().await;
    }

    #[test]
    fn saved_copy_is_validated_and_persisted() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("content.json");
        let mut value = serde_json::to_value(GameContent::bundled().as_ref()).unwrap();
        value["game"]["tagline"] = serde_json::Value::String("Edited in the console.".to_string());
        let source = serde_json::to_string_pretty(&value).unwrap();
        let content = GameContent::parse(&source).unwrap();

        write_content(&path, &source).unwrap();

        assert_eq!(content.game.tagline, "Edited in the console.");
        assert_eq!(
            GameContent::load(&path).unwrap().game.tagline,
            "Edited in the console."
        );
    }
}
