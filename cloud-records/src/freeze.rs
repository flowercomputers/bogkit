//! Worker-owned mutation barrier. The supervisor's request lease can disappear
//! when its caller disconnects; this barrier lives with the actual data handler.
use axum::{
    Json, Router,
    extract::Request,
    http::{Method, StatusCode},
    middleware::{Next, from_fn},
    response::IntoResponse,
    routing::post,
};
use bog_definition::{Action, Definition};
use serde_json::json;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::RwLock;

/// Attach operator-only pause/resume controls to a Unix worker router. A pause
/// acknowledgment proves earlier mutations have returned (after checkpoint).
/// Reads never acquire this barrier and remain available throughout a build.
pub fn with_write_freeze(router: Router, definition: &Definition) -> Router {
    let paused = Arc::new(RwLock::new(false));
    let reads: Arc<HashSet<String>> = Arc::new(
        definition
            .expose
            .iter()
            .filter(|(_, op)| !matches!(op.action, Action::Put | Action::Remove | Action::Batch))
            .map(|(name, _)| format!("/operations/{name}"))
            .collect(),
    );
    let guard = paused.clone();
    let router=router.layer(from_fn(move |request:Request,next:Next| {
  let paused=guard.clone();let reads=reads.clone();
  async move {
   let path=request.uri().path();
   let mutation=(matches!(*request.method(),Method::PUT|Method::DELETE)&&path.starts_with("/docs/"))
    ||(*request.method()==Method::POST&&(path=="/batch"||path=="/_cloud/import"||(path.starts_with("/operations/")&&!reads.contains(path))));
   if !mutation {return next.run(request).await;}
   // Held through the entire real handler; manager cancellation cannot shorten
   // the in-worker critical section. Tokio's fair lock orders queued pauses.
   let permit=paused.read_owned().await;
   if *permit {return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":{"code":"writes_paused","message":"writes paused for definition build"}}))).into_response();}
   let response=next.run(request).await;
   drop(permit);
   response
  }
 }));
    let pause = paused.clone();
    router
        .route(
            "/_cloud/pause_writes",
            post(move || {
                let paused = pause.clone();
                async move {
                    *paused.write().await = true;
                    Json(json!({"paused":true}))
                }
            }),
        )
        .route(
            "/_cloud/resume_writes",
            post(move || {
                let paused = paused.clone();
                async move {
                    *paused.write().await = false;
                    Json(json!({"paused":false}))
                }
            }),
        )
}
