//! Native GitHub identity and human approval routes.
use crate::{
    CloudError, CloudService,
    browser_auth::{LOGIN_COOKIE, SESSION_COOKIE},
    http::{cookie_value, guide_asset, header_text, url_query},
};
use axum::{
    Json,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
fn denied() -> CloudError {
    CloudError::new("unauthorized", "sign in required")
}
pub async fn endpoint(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    let mut r = inner(&service, request)
        .await
        .unwrap_or_else(|e| crate::http::error_response(e, &uuid::Uuid::new_v4().to_string()));
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    r
}
async fn inner(service: &CloudService, request: Request) -> Result<Response, CloudError> {
    let native = service
        .native_auth
        .as_ref()
        .ok_or_else(|| CloudError::new("unavailable", "native authentication is not configured"))?;
    let path = request.uri().path().to_owned();
    if path == "/auth/device/approve" && request.method() == "GET" {
        return Ok(guide_asset(
            "text/html; charset=utf-8",
            include_str!("../static/device.html"),
        ));
    }
    if header_text(request.headers(), "content-type")
        .split(';')
        .next()
        != Some("application/json")
    {
        return Err(CloudError::new(
            "invalid_request",
            "application/json required",
        ));
    }
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|p| p.0.ip());
    let session = cookie_value(request.headers(), SESSION_COOKIE).unwrap_or_default();
    let origin = header_text(request.headers(), "origin").to_owned();
    let csrf = header_text(request.headers(), "x-csrf-token").to_owned();
    let bytes = to_bytes(request.into_body(), 4096)
        .await
        .map_err(|_| CloudError::new("payload_too_large", "request too large"))?;
    let body: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| CloudError::new("invalid_request", "invalid JSON"))?;
    let value = match path.as_str() {
        "/auth/device" => native.start_device(
            peer.ok_or_else(|| CloudError::new("unavailable", "peer address required"))?,
            body["name"].as_str().unwrap_or(""),
        )?,
        "/auth/device/token" => {
            let token =
                native.poll_device(body["device_code"].as_str().unwrap_or(""), &service.auth)?;
            json!({"access_token":token.secret,"token_type":"Bearer","expires_in":2592000})
        }
        "/auth/device/approve" => {
            native.check_csrf(&session, &origin, &csrf)?;
            let identity = native.authenticate(&session)?;
            let (_, personal) = service.auth.provision_identity(&identity)?;
            let principal = service
                .auth
                .principal_from_verified(&identity, personal.id)?;
            let code = body["user_code"].as_str().unwrap_or("");
            if !crate::native_auth::valid_public(code) {
                return Err(CloudError::new(
                    "invalid_request",
                    "8-character public code required",
                ));
            }
            if body.get("approve").is_none() {
                json!({"name":native.device_details(&session,code)?,"access":native.oauth_access_description(code)?.unwrap_or_else(||"Create and use Bogs and issue app credentials in your current workspaces for 30 days. Cannot manage members, delete Bogs, or create account credentials.".into())})
            } else {
                let approve = body["approve"].as_bool().ok_or_else(|| {
                    CloudError::new("invalid_request", "approval must be true or false")
                })?;
                native.approve_device(&session, code, principal, approve, &service.auth)?;
                json!({"approved":approve,"redirect_uri":native.oauth_approved(code,approve)?})
            }
        }
        _ => return Err(CloudError::new("not_found", "route not found")),
    };
    Ok(Json(value).into_response())
}
pub async fn browser_inner(
    service: &CloudService,
    request: Request,
) -> Result<Response, CloudError> {
    let native = service.native_auth.as_ref().ok_or_else(denied)?;
    let h = request.headers();
    let session = cookie_value(h, SESSION_COOKIE).unwrap_or_default();
    Ok(match request.uri().path() {
        "/auth/login" => {
            let code = url_query(request.uri().query().unwrap_or(""), "user_code");
            let start = native.begin_login(code.as_deref())?;
            (
                [
                    (header::LOCATION, start.authorization_url),
                    (header::SET_COOKIE, start.set_cookie),
                ],
                StatusCode::SEE_OTHER,
            )
                .into_response()
        }
        "/auth/callback" => {
            let q = request.uri().query().unwrap_or("");
            if url_query(q, "error").is_some() {
                let _ = native.cancel_login(
                    &url_query(q, "state").unwrap_or_default(),
                    &cookie_value(h, LOGIN_COOKIE).unwrap_or_default(),
                );
                return Err(denied());
            }
            let login = native
                .complete_login(
                    &url_query(q, "state").unwrap_or_default(),
                    &url_query(q, "code").unwrap_or_default(),
                    &cookie_value(h, LOGIN_COOKIE).unwrap_or_default(),
                )
                .await?;
            service.auth.provision_identity(&login.identity)?;
            let target = login
                .public_code
                .map(|c| format!("/auth/device/approve?user_code={c}"))
                .unwrap_or_else(|| "/console".into());
            let mut r = (
                [
                    (header::LOCATION, target),
                    (header::SET_COOKIE, login.set_cookie),
                ],
                StatusCode::SEE_OTHER,
            )
                .into_response();
            r.headers_mut().append(
                header::SET_COOKIE,
                header::HeaderValue::from_static(
                    "__Host-bog_login=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0",
                ),
            );
            r
        }
        "/console-session" => {
            let identity = native.authenticate(&session)?;
            let (account, _) = service.auth.provision_identity(&identity)?;
            Json(json!({"account":account,"workspaces":service.auth.list_workspaces(&identity)?,"csrf_token":native.csrf_token(&session)?,"authentication_mode":"github_native"})).into_response()
        }
        "/auth/logout" => {
            let cookie = native.logout(
                &session,
                header_text(h, "origin"),
                header_text(h, "x-csrf-token"),
            )?;
            (
                [(header::SET_COOKIE, cookie)],
                Json(json!({"logged_out":true,"provider_revoked":false})),
            )
                .into_response()
        }
        "/auth/refresh" => {
            let identity = native.authenticate(&session)?;
            service.auth.provision_identity(&identity)?;
            let renewed = native.refresh(
                &session,
                header_text(h, "origin"),
                header_text(h, "x-csrf-token"),
            )?;
            (
                [(header::SET_COOKIE, renewed.set_cookie)],
                Json(json!({"refreshed":true,"csrf_token":renewed.csrf_token})),
            )
                .into_response()
        }
        _ => return Err(CloudError::new("not_found", "route not found")),
    })
}
pub const AUTH_MARKDOWN: &str = r#"# auth.md — Bog Cloud authentication

This is a prototype. During this preview, the existing chat owner credential may remain enabled for the legacy workspace only; it cannot access new personal workspaces or manage memberships.

GitHub sign-in at /auth/login proves identity. Bog owns sessions, memberships and credentials. No repository scopes are requested. Your personal workspace is created once. Logout ends only the local Bog session.

For agents, POST /auth/device with JSON {"name":"My agent"}. Keep device_code private in memory. Show the user only verification_uri and user_code. The human signs in, reviews the name/access, and explicitly approves or denies. Poll POST /auth/device/token with JSON {"device_code":"<private code>"} at intervals of at least 5 seconds. Grants expire after 10 minutes and are lost on server restart. Errors: authorization_pending (keep waiting), slow_down (for polling, wait at least 5 seconds; request and approval limits require a 10-minute wait, then a new request if the grant expired), expired_token (start again), access_denied (stop). Successful polling returns access_token exactly once, token_type Bearer, expires_in 2592000. Never put credentials in chat, URLs, logs or browser storage.

Alternatively, a signed-in human creates/revokes named agent credentials in /console or uses GET/POST /v1/agent-tokens and DELETE /v1/agent-tokens/{id}. Writes require the session cookie, exact Origin and x-csrf-token from /console-session. Only humans can manage account credentials. Tokens expire in 30 days; revocation and account suspension are immediate.

MCP tool results, including structuredContent, may enter model context, chat transcripts or client logs. Structured output is not a private credential channel. For credential issuance, prefer HTTP and write the response directly to a private file with owner-only permissions, without printing it. Configure the client from that file privately; never paste its contents into a conversation. This applies to device-token responses, account credentials and scoped app credentials.

Use Authorization: Bearer <Bog credential> for REST and bearer-capable MCP clients at /mcp. Select workspace_id explicitly; omission selects personal. Agents can create Bogs and issue scoped app credentials, but cannot manage members, delete Bogs or mint account credentials. App credentials remain restricted to one Bog. Membership is checked on every request. GitHub access tokens are never Bog API credentials. OAuth-capable MCP clients can discover the self-hosted authorization server and request bog:read or bog:write. GitHub remains the identity provider; there is no paid authentication intermediary. See /v1 for creation requirements and /v1/templates for templates.

## OAuth 2.0 for MCP clients

Discover /.well-known/oauth-protected-resource (resource is this origin plus /mcp), then /.well-known/oauth-authorization-server. Register a public client at POST /oauth/register with application/json, client_name, redirect_uris, and token_endpoint_auth_method "none". Registration lasts 30 days. Only exact HTTPS or HTTP loopback-IP redirect URLs are accepted. Client metadata document URLs, refresh tokens, OIDC ID tokens, and the separate Auth.md identity-assertion registration protocol are not implemented.

Open /oauth/authorize with response_type=code, client_id, redirect_uri, resource, scope, state, code_challenge and code_challenge_method=S256. The human signs in through GitHub and explicitly approves or denies the named client and callback destination. Check state and the returned iss before exchanging the code. Exchange within 60 seconds at POST /oauth/token using application/x-www-form-urlencoded: grant_type=authorization_code, code, code_verifier, client_id, the exact redirect_uri and resource. Codes are one-use; pending requests expire after ten minutes and are lost on server restart. Access tokens persist across restart, expire after 30 days, and remain subject to current membership and account suspension.

Scopes: bog:read reads accessible Bogs, records, workspace and credential metadata; it cannot create, write, or issue credentials. bog:write includes reads, Bog creation, record writes, and single-Bog app credential issuance/revocation. Neither scope permits Bog deletion, organization/membership administration, platform controls or account credential issuance. Request the least access needed; the default is bog:read. Both HTTP and MCP share this permission boundary. For a deployed app, use a single-Bog read or write credential instead of an account-wide OAuth credential.

Revoke an OAuth connection through Agents in /console, or POST /oauth/revoke using application/x-www-form-urlencoded with token and client_id. Revocation is immediate, including for pending change waits. Invalid or already-revoked tokens also receive HTTP 200. There are no refresh tokens: reconnect with a new human approval after expiry. Keep access tokens out of model context, URLs, logs, and chat; the client handles the short-lived authorization code and PKCE verifier privately.

## Free self-serve access

GitHub signup is open. No payment card, sales contact, organization setup or manual key approval is required. The free prototype allowance is three Bogs per ordinary workspace, 16 MiB each, subject to shared host capacity. Approved accounts or organizations can have an uncapped Bog allowance; per-Bog storage and host capacity still apply. Device approval or OAuth gets an agent started; /console provides self-serve credential creation and revocation. Test in a disposable Bog in your workspace; there is no separate sandbox environment.
"#;
fn native_llms() -> String {
    format!(
        "{}\n## When to use Bog Cloud\n\nUse Bog Cloud when an app or agent needs a hosted typed datastore with schema validation and durable storage. Use HTTP for direct API integration and private credential issuance; use MCP for a bearer-capable agent client.\n\nAuthentication: GitHub browser sign-in at /auth/login, Bog device approval at /auth/device, and Bog bearer credentials. Read /auth.md before requesting credentials. Free tier: three Bogs per ordinary workspace, 16 MiB each, no payment card. Self-serve API keys are available after GitHub approval in /console. Use a disposable Bog for testing; no separate sandbox is provided. OAuth-capable MCP clients use /.well-known/oauth-protected-resource and /.well-known/oauth-authorization-server.\n",
        crate::contract::llms()
    )
}
pub fn discovery(service: &CloudService, path: &str) -> Option<Response> {
    Some(match path {
        "/auth.md" => (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            AUTH_MARKDOWN,
        )
            .into_response(),
        "/.well-known/oauth-protected-resource" => (
            [(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")],
            Json(service.native_auth.as_ref()?.resource_metadata()),
        )
            .into_response(),
        "/openapi.json" => {
            let native = service.native_auth.as_ref()?;
            let base = native.config.origin();
            let mut spec = crate::contract::openapi();
            spec["components"]["securitySchemes"]["oauth2"] = json!({"type":"oauth2","description":"Self-hosted OAuth with human GitHub approval, PKCE S256 and current workspace checks. App credentials are separately restricted to one Bog.","flows":{"authorizationCode":{"authorizationUrl":format!("{base}/oauth/authorize"),"tokenUrl":format!("{base}/oauth/token"),"scopes":{"bog:read":"Read accessible Bogs, records and workspace metadata","bog:write":"Read/write records, create Bogs and manage single-Bog app credentials; no owner administration"}}}});
            for (path, methods) in spec["paths"].as_object_mut()? {
                for (method, operation) in methods.as_object_mut()? {
                    let allowed = path == "/v1/me"
                        || (path == "/v1/workspaces" && method == "get")
                        || (path.starts_with("/v1/bogs")
                            && !(method == "delete" && path == "/v1/bogs/{bog_id}"));
                    if allowed && let Some(security) = operation["security"].as_array_mut() {
                        let scope = if method == "get" {
                            "bog:read"
                        } else {
                            "bog:write"
                        };
                        security.push(json!({"oauth2":[scope]}));
                    }
                }
            }
            Json(spec).into_response()
        }
        "/llms.txt" => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            native_llms(),
        )
            .into_response(),
        "/v1" => {
            let mut v = crate::contract::overview();
            v["pricing"] = json!({"plan":"free prototype","price":0,"currency":"USD","payment_card_required":false,"self_serve_signup":"/auth/login","self_serve_credentials":"/console","sandbox":"Use a disposable Bog in your workspace; no separate sandbox"});
            v["interfaces"] = json!({"http":"/v1","mcp":"/mcp","graphql":false});
            v["authentication_configured"] = json!(service.authentication_configured());
            v["authentication_mode"] = json!("github_native");
            v["authentication"] = json!({"browser_login":"/auth/login","device_start":"/auth/device","device_token":"/auth/device/token","device_approval":"/auth/device/approve","agent_tokens":"/v1/agent-tokens","account":"/v1/me","session":"/console-session","mcp":"bearer","oauth_authorization_server":true,"oauth_metadata":"/.well-known/oauth-authorization-server","scopes":["bog:read","bog:write"]});
            v["next_step"] = json!(
                "Sign in with GitHub at /auth/login or request human approval via /auth/device. Read /auth.md."
            );
            Json(v).into_response()
        }
        _ => return None,
    })
}
pub fn guide(service: &CloudService) -> String {
    if service.native_auth.is_none() {
        return crate::contract::guide(
            service.public_auth.is_some(),
            service.supervisor.max_active(),
        );
    }
    include_str!("../static/home.html").to_owned()
}
