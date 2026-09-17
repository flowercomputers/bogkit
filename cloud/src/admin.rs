//! Operator API available only on the manager's private Unix socket.
use crate::{BogId, CloudError, CloudService, http::error_response};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};
fn authorize(service: &CloudService, headers: &HeaderMap) -> Result<(), CloudError> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| CloudError::new("unauthorized", "owner credentials required"))?;
    let principal = service.auth.authenticate(token)?;
    if principal.kind() != crate::auth::PrincipalKind::Operator {
        return Err(CloudError::new(
            "forbidden",
            "operator credentials required",
        ));
    }
    service.auth.authorize(&principal, None, true)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimLegacy {
    access_token: String,
}
async fn claim_legacy(
    State(service): State<Arc<CloudService>>,
    headers: HeaderMap,
    Json(body): Json<ClaimLegacy>,
) -> Response {
    let result = async {
        authorize(&service, &headers)?;
        let public = service
            .public_auth
            .as_ref()
            .ok_or_else(|| CloudError::new("unavailable", "WorkOS is not configured"))?;
        let issuer = std::env::var("BOG_LEGACY_OWNER_ISSUER").map_err(|_| {
            CloudError::new(
                "invalid_config",
                "legacy owner identity must be explicitly configured",
            )
        })?;
        let subject = std::env::var("BOG_LEGACY_OWNER_SUBJECT").map_err(|_| {
            CloudError::new(
                "invalid_config",
                "legacy owner identity must be explicitly configured",
            )
        })?;
        let identity = public
            .verifier
            .verify_access_token(&body.access_token)
            .await?;
        let operator = service.auth.authenticate(
            headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or_default(),
        )?;
        service
            .auth
            .claim_legacy(&operator, &identity, &issuer, &subject)
    }
    .await;
    match result {
        Ok(workspace) => Json(json!({"workspace_id":workspace})).into_response(),
        Err(e) => error_response(e, &uuid::Uuid::new_v4().to_string()),
    }
}
async fn backup(
    State(service): State<Arc<CloudService>>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        authorize(&service, &headers)?;
        service.supervisor.backup(BogId(id)).await
    }
    .await;
    match result {
        Ok(id) => Json(json!({"archive_id":id})).into_response(),
        Err(e) => error_response(e, &uuid::Uuid::new_v4().to_string()),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Restore {
    name: String,
}
async fn restore(
    State(service): State<Arc<CloudService>>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(body): Json<Restore>,
) -> Response {
    let result = async {
        authorize(&service, &headers)?;
        service
            .supervisor
            .restore(&id.to_string(), &body.name)
            .await
    }
    .await;
    match result {
        Ok(bog) => Json(bog).into_response(),
        Err(e) => error_response(e, &uuid::Uuid::new_v4().to_string()),
    }
}
pub struct AdminServer {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    path: PathBuf,
    marker: PathBuf,
    dev: u64,
    ino: u64,
}
impl AdminServer {
    pub async fn shutdown(mut self) -> Result<(), CloudError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.task)
            .await
            .map_err(|_| CloudError::new("unavailable", "admin listener failed"))?
            .map_err(|_| CloudError::new("unavailable", "admin listener failed"))?;
        Ok(())
    }
}
impl Drop for AdminServer {
    fn drop(&mut self) {
        self.task.abort();
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(&self.marker);
        }
    }
}
pub async fn bind(service: Arc<CloudService>, path: PathBuf) -> Result<AdminServer, CloudError> {
    let fail = || CloudError::new("unavailable", "cannot bind private admin socket");
    let marker = path.with_extension("owner");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        let expected = format!("{}:{}", meta.dev(), meta.ino());
        if !meta.file_type().is_socket()
            || std::fs::read_to_string(&marker).ok().as_deref() != Some(&expected)
        {
            return Err(fail());
        }
        match tokio::net::UnixStream::connect(&path).await {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            _ => return Err(fail()),
        }
        std::fs::remove_file(&path).map_err(|_| fail())?;
    }
    if std::fs::symlink_metadata(&marker).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(fail());
    }
    let listener = tokio::net::UnixListener::bind(&path).map_err(|_| fail())?;
    let meta = std::fs::symlink_metadata(&path).map_err(|_| fail())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|_| fail())?;
    std::fs::write(&marker, format!("{}:{}", meta.dev(), meta.ino())).map_err(|_| fail())?;
    let router = Router::new()
        .route("/claim-legacy", post(claim_legacy))
        .route("/backup/{id}", post(backup))
        .route("/restore/{id}", post(restore))
        .layer(DefaultBodyLimit::max(16384))
        .with_state(service);
    let (stop, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
    });
    Ok(AdminServer {
        stop: Some(stop),
        task,
        path,
        marker,
        dev: meta.dev(),
        ino: meta.ino(),
    })
}
