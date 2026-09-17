use crate::tools::Handler;
use axum::{
    Router,
    extract::{Request, State},
    middleware::{self, Next},
    response::Response,
};
use bog_cloud::{CloudError, CloudService};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use std::sync::Arc;

/// An empty origins list denies all requests carrying Origin, while allowing
/// non-browser clients without Origin. Host defaults remain loopback-only.
#[derive(Clone, Default)]
pub struct McpOptions {
    pub allowed_origins: Vec<String>,
    pub allowed_hosts: Option<Vec<String>>,
}
#[derive(Clone)]
struct Gateway {
    service: Arc<CloudService>,
    options: McpOptions,
}
pub fn build_mcp_router(service: Arc<CloudService>) -> Router {
    build_mcp_router_with_options(service, McpOptions::default())
}
pub fn build_mcp_router_with_options(service: Arc<CloudService>, options: McpOptions) -> Router {
    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(1024 * 1024);
    if let Some(hosts) = &options.allowed_hosts
        && !hosts.is_empty()
    {
        config = config.with_allowed_hosts(hosts.clone());
    }
    let handler_service = service.clone();
    let transport = StreamableHttpService::new(
        move || Ok(Handler(handler_service.clone())),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route_service("/mcp", transport)
        .layer(middleware::from_fn_with_state(
            Gateway { service, options },
            authorize,
        ))
}
async fn authorize(State(gateway): State<Gateway>, mut request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let reject = |code, message| {
        let mut response =
            bog_cloud::http::error_response(CloudError::new(code, message), &request_id);
        if response.status() == 401
            && let Ok(v) =
                axum::http::HeaderValue::from_str(&gateway.service.authentication_challenge())
        {
            response
                .headers_mut()
                .insert(axum::http::header::WWW_AUTHENTICATE, v);
        }
        response
    };
    let headers = request.headers();
    let auth = headers
        .get_all(axum::http::header::AUTHORIZATION)
        .iter()
        .collect::<Vec<_>>();
    if auth.len() != 1 {
        return reject("unauthorized", "bearer credential required");
    }
    let Some(token) = auth[0]
        .to_str()
        .ok()
        .and_then(|s| s.strip_prefix("Bearer "))
    else {
        return reject("unauthorized", "bearer credential required");
    };
    let principal = match gateway.service.authenticate_bearer(token, None).await {
        Ok(p) => p,
        Err(e) => return reject(&e.code, &e.message),
    };
    let origins = headers
        .get_all(axum::http::header::ORIGIN)
        .iter()
        .collect::<Vec<_>>();
    if origins.len() > 1
        || origins.first().is_some_and(|origin| {
            origin.to_str().ok().is_none_or(|value| {
                !gateway
                    .options
                    .allowed_origins
                    .iter()
                    .any(|allowed| allowed == value)
            })
        })
    {
        return reject("forbidden", "origin not allowed");
    }
    // This endpoint never creates sessions. Reject any claimed session identity,
    // including legacy IDs, instead of allowing a different bearer to reuse one.
    if headers.contains_key("mcp-session-id") {
        return reject(
            "invalid_request",
            "sessions are not supported; authenticate every request",
        );
    }
    request.extensions_mut().insert(principal);
    match tokio::time::timeout(std::time::Duration::from_secs(30), next.run(request)).await {
        Ok(response) => response,
        Err(_) => reject("unavailable", "request timed out"),
    }
}
