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
#[derive(Clone)]
pub(crate) struct RequestId(pub String);
async fn authorize(State(gateway): State<Gateway>, mut request: Request, next: Next) -> Response {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
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
            .headers_mut()
            .insert("x-request-id", request_id.parse().expect("UUID header"));
        eprintln!(
            "{}",
            serde_json::json!({"event":"mcp_request","request_id":request_id,"status":response.status().as_u16()})
        );
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
    let native_oauth = token.starts_with("bog_oauth_");
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
    // Only native OAuth scope failures become HTTP challenges. Parse using the
    // same SDK envelope and tool validator as the handler; malformed requests
    // and workspace/record denials keep their existing protocol errors.
    if native_oauth
        && request.method() == axum::http::Method::POST
        && let Some(challenge) = gateway.service.oauth_write_challenge(&principal)
    {
        let (parts, body) = request.into_parts();
        let bytes = match tokio::time::timeout_at(deadline, axum::body::to_bytes(body, 1024 * 1024))
            .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(_)) => return reject("payload_too_large", "request exceeds 1 MiB"),
            Err(_) => return reject("unavailable", "request timed out"),
        };
        let upgrade = scope_upgrade_required(&gateway.service, &principal, &bytes);
        request = Request::from_parts(parts, axum::body::Body::from(bytes));
        if upgrade {
            let mut response = reject("forbidden", "bog:write scope required");
            if let Ok(value) = axum::http::HeaderValue::from_str(&challenge) {
                response
                    .headers_mut()
                    .insert(axum::http::header::WWW_AUTHENTICATE, value);
            }
            return response;
        }
    }
    request.extensions_mut().insert(principal);
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    match tokio::time::timeout_at(deadline, next.run(request)).await {
        Ok(mut response) => {
            response
                .headers_mut()
                .insert("x-request-id", request_id.parse().expect("UUID header"));
            eprintln!(
                "{}",
                serde_json::json!({"event":"mcp_request","request_id":request_id,"status":response.status().as_u16()})
            );
            response
        }
        Err(_) => reject("unavailable", "request timed out"),
    }
}

fn scope_upgrade_required(
    service: &CloudService,
    principal: &bog_cloud::Principal,
    bytes: &[u8],
) -> bool {
    use bog_cloud::Operation;
    use rmcp::model::{ClientJsonRpcMessage, ClientRequest};
    let Ok(ClientJsonRpcMessage::Request(message)) =
        serde_json::from_slice::<ClientJsonRpcMessage>(bytes)
    else {
        return false;
    };
    let ClientRequest::CallToolRequest(call) = message.request else {
        return false;
    };
    let mut args = call.params.arguments.unwrap_or_default();
    let principal = if let Some(workspace) = args.remove("workspace_id") {
        let Some(workspace) = workspace
            .as_str()
            .and_then(|w| uuid::Uuid::parse_str(w).ok())
        else {
            return false;
        };
        let Ok(principal) = service
            .auth
            .select_workspace(principal, bog_cloud::WorkspaceId(workspace))
        else {
            return false;
        };
        principal
    } else {
        principal.clone()
    };
    let Ok(operation) = crate::tools::operation(&call.params.name, serde_json::Value::Object(args))
    else {
        return false;
    };
    let target = match operation {
        Operation::CreateBog { .. } => None,
        Operation::PrepareAppAccess { bog_id, .. }
        | Operation::IssueToken { bog_id, .. }
        | Operation::RevokeToken { bog_id, .. }
        | Operation::UpsertRecord { bog_id, .. }
        | Operation::DeleteRecord { bog_id, .. }
        | Operation::Batch { bog_id, .. } => Some(bog_id),
        _ => return false,
    };
    service.auth.authorize(&principal, target, false).is_ok()
}
