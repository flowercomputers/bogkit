use crate::{BogId, CloudError, CloudService, Operation, Scope};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub fn build_rest_router(service: Arc<CloudService>) -> Router {
    Router::new()
        .route(
            "/console",
            get(|| async {
                guide_asset(
                    "text/html; charset=utf-8",
                    include_str!("../static/console.html"),
                )
            }),
        )
        .route(
            "/console.js",
            get(|| async {
                guide_asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../static/console.js"),
                )
            }),
        )
        .route(
            "/console.css",
            get(|| async {
                guide_asset(
                    "text/css; charset=utf-8",
                    include_str!("../static/console.css"),
                )
            }),
        )
        .route("/auth/login", get(browser_endpoint))
        .route("/auth/callback", get(browser_endpoint))
        .route("/auth/logout", axum::routing::post(browser_endpoint))
        .route("/auth/refresh", axum::routing::post(browser_endpoint))
        .route("/console-session", get(browser_endpoint))
        .route("/v1", get(discovery))
        .route("/v1/templates", get(discovery))
        .route("/auth.md", get(discovery))
        .route("/llms.txt", get(discovery))
        .route("/openapi.json", get(discovery))
        .route("/.well-known/oauth-protected-resource", get(discovery))
        .route(
            "/",
            get(|State(service): State<Arc<CloudService>>| async move {
                guide_asset(
                    "text/html; charset=utf-8",
                    crate::contract::guide(service.public_auth.is_some(), service.supervisor.max_active()),
                )
            }),
        )
        .route(
            "/guide.css",
            get(|| async {
                guide_asset(
                    "text/css; charset=utf-8",
                    include_str!("../static/guide.css"),
                )
            }),
        )
        .route(
            "/guide.js",
            get(|| async {
                guide_asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../static/guide.js"),
                )
            }),
        )
        .route(
            "/healthz",
            get(|State(service): State<Arc<CloudService>>| async move {
                match service.registry.list() {
                    Ok(_) => (StatusCode::OK, Json(json!({"status":"ok","authentication_configured":service.public_auth.is_some()}))),
                    Err(_) => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"status":"unavailable"})),
                    ),
                }
            }),
        )
        .fallback(dispatch)
        .with_state(service)
}

fn guide_asset(content_type: &'static str, body: impl Into<String>) -> Response {
    ([
        (header::CONTENT_TYPE, content_type),
        (header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CACHE_CONTROL, "no-cache"),
    ], body.into()).into_response()
}
pub fn error_response(error: CloudError, request_id: &str) -> Response {
    let status = match error.code.as_str() {
        "unauthorized" => 401,
        "forbidden" => 403,
        "not_found" => 404,
        "conflict" => 409,
        "payload_too_large" => 413,
        "capacity" => 429,
        "method_not_allowed" => 405,
        "unavailable" => 503,
        "response_too_large" => 413,
        _ => 400,
    };
    let mut response = (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({"error":error,"request_id":request_id})),
    )
        .into_response();
    if status == 503 {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("1"));
    }
    if status == 401 {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
    }
    response
}
async fn dispatch(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        dispatch_inner(&service, request),
    )
    .await
    .unwrap_or_else(|_| Err(CloudError::new("unavailable", "request timed out")));
    let mut response = match result {
        Ok(mut result) => {
            if result.status == 204 {
                StatusCode::NO_CONTENT.into_response()
            } else {
                if let Some(object) = result.body.as_object_mut() {
                    object.insert("request_id".into(), json!(request_id));
                }
                (
                    StatusCode::from_u16(result.status).unwrap_or(StatusCode::OK),
                    Json(result.body),
                )
                    .into_response()
            }
        }
        Err(error) => error_response(error, &request_id),
    };
    if response.status() == StatusCode::UNAUTHORIZED
        && let Ok(v) = header::HeaderValue::from_str(&service.authentication_challenge())
    {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "x-request-id",
        header::HeaderValue::from_str(&request_id).unwrap(),
    );
    eprintln!(
        "{}",
        json!({"event":"request","request_id":request_id,"status":response.status().as_u16(),"elapsed_ms":started.elapsed().as_millis()})
    );
    response
}
fn bad(message: &str) -> CloudError {
    CloudError::new("invalid_request", message)
}
async fn dispatch_inner(
    service: &CloudService,
    request: Request,
) -> Result<crate::OperationResult, CloudError> {
    if request
        .headers()
        .get_all(header::AUTHORIZATION)
        .iter()
        .count()
        > 1
    {
        return Err(CloudError::new(
            "unauthorized",
            "exactly one bearer credential allowed",
        ));
    }
    let workspace = request
        .uri()
        .query()
        .and_then(|q| url_query(q, "workspace_id"))
        .map(|w| {
            Uuid::parse_str(&w)
                .map(crate::WorkspaceId)
                .map_err(|_| bad("invalid workspace_id"))
        })
        .transpose()?;
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if request.headers().contains_key(header::AUTHORIZATION) && token.is_none() {
        return Err(CloudError::new("unauthorized", "invalid bearer credential"));
    }
    let principal = if let Some(token) = token {
        service.authenticate_bearer(token, workspace).await?
    } else {
        let browser = &service
            .public_auth
            .as_ref()
            .ok_or_else(|| CloudError::new("unauthorized", "sign in required"))?
            .browser;
        let session = cookie_value(request.headers(), crate::browser_auth::SESSION_COOKIE)
            .ok_or_else(|| CloudError::new("unauthorized", "sign in required"))?;
        let identity = browser.authenticate(&session).await?;
        if !matches!(request.method().as_str(), "GET" | "HEAD") {
            browser.check_csrf(
                &session,
                header_text(request.headers(), "origin"),
                header_text(request.headers(), "x-csrf-token"),
            )?;
        }
        let (_, personal) = service.auth.provision_identity(&identity)?;
        service
            .auth
            .principal_from_verified(&identity, workspace.unwrap_or(personal.id))?
    };
    let method = request.method().as_str().to_owned();
    let path: Vec<String> = request
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| bad("invalid path encoding"))
        })
        .collect::<Result<_, _>>()?;
    if path.len() < 2 || path[0] != "v1" {
        return Err(CloudError::new("not_found", "route not found"));
    }
    let query = request.uri().query().unwrap_or("").to_owned();
    let key = request
        .headers()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if matches!(method.as_str(), "POST" | "PUT") && content_type != "application/json" {
        return Err(bad("content-type must be application/json"));
    }
    let bytes = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| CloudError::new("payload_too_large", "request exceeds 1 MiB"))?;
    let parse = || serde_json::from_slice::<Value>(&bytes).map_err(|_| bad("invalid JSON body"));
    if path[1] != "bogs" {
        service.rate_limit(&principal)?;
        let ok = |body| Ok(crate::OperationResult { status: 200, body });
        match (method.as_str(), path[1].as_str(), path.len()) {
            ("GET", "me", 2) => {
                return ok(
                    json!({"kind":principal.kind(),"account":principal.account_id().map(|id|json!({"id":id})),"workspace_id":principal.workspace_id()}),
                );
            }
            ("GET", "workspaces", 2) => {
                return ok(
                    json!({"workspaces":service.auth.workspaces_for_principal(&principal)?}),
                );
            }
            ("POST", "invitations", 3) if path[2] == "accept" => {
                let v = parse()?;
                let w = service.auth.accept_invitation_for_principal(
                    &principal,
                    v["secret"].as_str().ok_or_else(|| bad("secret required"))?,
                )?;
                return ok(json!({"workspace_id":w}));
            }
            ("POST", "invitations", 3) if path[2] == "preview" => {
                let v = parse()?;
                return ok(serde_json::to_value(service.auth.preview_invitation(
                    &principal,
                    v["secret"].as_str().ok_or_else(|| bad("secret required"))?,
                )?)
                .map_err(|_| bad("preview unavailable"))?);
            }
            _ => {}
        }
        if path[1] == "workspaces" && path.len() >= 4 {
            let w = crate::WorkspaceId(
                Uuid::parse_str(&path[2]).map_err(|_| bad("invalid workspace_id"))?,
            );
            let p = service.auth.select_workspace(&principal, w)?;
            let body = match (method.as_str(), path[3].as_str(), path.len()) {
                ("GET", "members", 4) => json!({"members":service.auth.list_members(&p)?}),
                ("DELETE", "members", 5) => {
                    service.auth.remove_member(&p, &path[4])?;
                    json!({"removed":true})
                }
                ("GET", "invitations", 4) => {
                    json!({"invitations":service.auth.list_invitations(&p)?})
                }
                ("POST", "invitations", 4) => {
                    let v = parse()?;
                    let i = service
                        .auth
                        .invite(&p, v["role"].as_str().unwrap_or("member"))?;
                    json!({"id":i.id,"secret":i.secret,"expires_at":i.expires_at})
                }
                ("DELETE", "invitations", 5) => {
                    service.auth.revoke_invitation(&p, &path[4])?;
                    json!({"revoked":true})
                }
                _ => return Err(CloudError::new("not_found", "route not found")),
            };
            return ok(body);
        }
        return Err(CloudError::new("not_found", "route not found"));
    }
    let id = if path.len() > 2 {
        Some(BogId(
            Uuid::parse_str(&path[2]).map_err(|_| bad("invalid database ID"))?,
        ))
    } else {
        None
    };
    let op = match (method.as_str(), path.len()) {
        ("GET", 4) if path[3] == "changes" => Operation::WaitForChange {
            bog_id: id.unwrap(),
            cursor: url_query(&query, "cursor"),
            timeout_seconds: url_query(&query, "timeout")
                .map(|v| {
                    v.parse::<u64>()
                        .map_err(|_| bad("timeout must be seconds, maximum 25"))
                })
                .transpose()?
                .unwrap_or(25),
        },
        ("GET", 2) => Operation::ListBogs,
        ("POST", 2) => {
            let (name, template, idempotency_key) =
                crate::contract::validate_creation(&parse()?, key.as_deref(), "Idempotency-Key")?;
            Operation::CreateBog {
                name,
                template,
                idempotency_key,
            }
        }
        ("DELETE", 3) => {
            service.auth.authorize_delete_bog(&principal, id.unwrap())?;
            let v = parse()?;
            let confirmation = BogId(
                Uuid::parse_str(
                    v["confirm"]
                        .as_str()
                        .ok_or_else(|| bad("confirm must equal database ID"))?,
                )
                .map_err(|_| bad("confirm must equal database ID"))?,
            );
            service
                .auth
                .delete_bog(&principal, id.unwrap(), confirmation)?;
            let supervisor = service.supervisor.clone();
            let bog = id.unwrap();
            tokio::spawn(async move {
                let _ = supervisor.cleanup_deleted(bog).await;
            });
            return Ok(crate::OperationResult {
                status: 202,
                body: json!({"deleted":true}),
            });
        }
        ("GET", 4) if path[3] == "tokens" => Operation::ListTokens {
            bog_id: id.unwrap(),
        },
        ("GET", 3) => Operation::DescribeBog {
            bog_id: id.unwrap(),
        },
        ("GET", 4) if path[3] == "usage" => Operation::Usage {
            bog_id: id.unwrap(),
        },
        ("GET", 4) if path[3] == "schema" => Operation::Schema {
            bog_id: id.unwrap(),
        },
        ("POST", 4) if path[3] == "batch" => Operation::Batch {
            bog_id: id.unwrap(),
            operations: parse()?,
        },
        ("POST", 4) if path[3] == "tokens" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Issue {
                scope: Scope,
            }
            let issue: Issue = serde_json::from_value(parse()?)
                .map_err(|_| bad("expected read or write scope"))?;
            Operation::IssueToken {
                bog_id: id.unwrap(),
                scope: issue.scope,
            }
        }
        ("DELETE", 5) if path[3] == "tokens" => Operation::RevokeToken {
            bog_id: id.unwrap(),
            token_id: path[4].clone(),
        },
        ("GET", 5) if path[3] == "docs" => Operation::GetRecord {
            bog_id: id.unwrap(),
            key: path[4].clone(),
        },
        ("PUT", 5) if path[3] == "docs" => Operation::UpsertRecord {
            bog_id: id.unwrap(),
            key: path[4].clone(),
            data: parse()?,
        },
        ("DELETE", 5) if path[3] == "docs" => Operation::DeleteRecord {
            bog_id: id.unwrap(),
            key: path[4].clone(),
        },
        ("GET", 5) if path[3] == "views" => {
            let mut limit = None;
            let mut offset = None;
            let url = reqwest::Url::parse(&format!("http://worker/?{query}"))
                .map_err(|_| bad("invalid query"))?;
            for (name, value) in url.query_pairs() {
                let target = match name.as_ref() {
                    "limit" => &mut limit,
                    "offset" => &mut offset,
                    "workspace_id" => continue,
                    _ => return Err(bad("unsupported query parameter")),
                };
                if target.is_some() {
                    return Err(bad("duplicate query parameter"));
                }
                *target = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| bad("invalid pagination"))?,
                );
            }
            Operation::ReadView {
                bog_id: id.unwrap(),
                view: path[4].clone(),
                limit,
                offset,
            }
        }
        _ => {
            let known = path.len() == 2
                || path.len() == 3
                || path.len() == 4 && ["schema", "batch", "tokens"].contains(&path[3].as_str())
                || path.len() == 5 && ["docs", "views", "tokens"].contains(&path[3].as_str());
            return Err(if known {
                CloudError::new("method_not_allowed", "method not allowed")
            } else {
                CloudError::new("not_found", "route not found")
            });
        }
    };
    service.execute(&principal, op).await
}

fn header_text<'a>(h: &'a axum::http::HeaderMap, n: &str) -> &'a str {
    h.get(n).and_then(|v| v.to_str().ok()).unwrap_or("")
}
fn cookie_value(h: &axum::http::HeaderMap, name: &str) -> Option<String> {
    header_text(h, "cookie").split(';').find_map(|v| {
        v.trim()
            .split_once('=')
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v.to_owned())
    })
}
fn url_query(query: &str, name: &str) -> Option<String> {
    reqwest::Url::parse(&format!("http://localhost/?{query}"))
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}
async fn browser_endpoint(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    let result = browser_inner(&service, request).await;
    let mut response = result.unwrap_or_else(|e| error_response(e, &Uuid::new_v4().to_string()));
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
async fn browser_inner(service: &CloudService, request: Request) -> Result<Response, CloudError> {
    let public = service
        .public_auth
        .as_ref()
        .ok_or_else(|| CloudError::new("unavailable", "browser sign in is not configured"))?;
    let browser = &public.browser;
    let h = request.headers();
    let path = request.uri().path();
    let session = cookie_value(h, crate::browser_auth::SESSION_COOKIE).unwrap_or_default();
    let response = match path {
        "/auth/login" => {
            let login = browser.begin_login()?;
            (
                [
                    (header::LOCATION, login.authorization_url),
                    (header::SET_COOKIE, login.set_cookie),
                ],
                StatusCode::SEE_OTHER,
            )
                .into_response()
        }
        "/auth/callback" => {
            let q = request.uri().query().unwrap_or("");
            let state = url_query(q, "state").unwrap_or_default();
            let code = url_query(q, "code").unwrap_or_default();
            let binding = cookie_value(h, crate::browser_auth::LOGIN_COOKIE).unwrap_or_default();
            let login = browser.complete_login(&state, &code, &binding).await?;
            service.auth.provision_identity(&login.identity)?;
            let mut r = (
                [
                    (header::LOCATION, "/console"),
                    (header::SET_COOKIE, &login.set_cookie),
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
        "/auth/logout" => {
            let out = browser
                .logout(
                    &session,
                    header_text(h, "origin"),
                    header_text(h, "x-csrf-token"),
                )
                .await?;
            (
                [(header::SET_COOKIE, out.set_cookie)],
                Json(json!({"logged_out":true,"provider_revoked":out.provider_revoked})),
            )
                .into_response()
        }
        "/auth/refresh" => {
            let identity = browser
                .refresh(
                    &session,
                    header_text(h, "origin"),
                    header_text(h, "x-csrf-token"),
                )
                .await?;
            service.auth.provision_identity(&identity)?;
            Json(json!({"refreshed":true})).into_response()
        }
        "/console-session" => {
            let identity = browser.authenticate(&session).await?;
            let (account, _) = service.auth.provision_identity(&identity)?;
            Json(json!({"account":account,"workspaces":service.auth.list_workspaces(&identity)?,"csrf_token":browser.csrf_token(&session)?})).into_response()
        }
        _ => return Err(CloudError::new("not_found", "route not found")),
    };
    Ok(response)
}
async fn discovery(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    match request.uri().path() {
        "/auth.md" => (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            service
                .public_auth
                .as_ref()
                .map(|a| a.verifier.config.auth_markdown())
                .unwrap_or_else(|| {
                    format!("# Bog Cloud: token-access preview\n\n{}\n\nBearer authentication is supported over HTTP and MCP. Use /v1 for creation requirements and /v1/templates for templates. Management credentials default to the legacy workspace. App credentials cannot provision or manage credentials. Keep credentials in a local secret store, never in chats or URLs.\n",crate::contract::LEGACY_GUIDANCE)
                }),
        )
            .into_response(),
        "/.well-known/oauth-protected-resource" => service
            .public_auth
            .as_ref()
            .map(|a| Json(a.verifier.config.resource_metadata()).into_response())
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response()),
        "/openapi.json" => Json(crate::contract::openapi()).into_response(),
        "/llms.txt" => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            crate::contract::llms_for_mode(service.public_auth.is_some()),
        )
            .into_response(),
        "/v1/templates" => Json(json!({"templates":crate::contract::overview()["templates"]})).into_response(),
        _ => {
            let mut overview = crate::contract::overview();
            overview["authentication_configured"] = json!(service.public_auth.is_some());
            if service.public_auth.is_none() {
                overview["limits"]["legacy_operator_bogs"] = json!(service.supervisor.max_active());
                overview["workspace_selection"] = json!("Legacy management credentials default to the legacy workspace; account workspaces and signup are not activated.");
                overview["next_step"] = json!("Ask the operator privately for a management credential to provision, or a single-Bog app credential to use an existing Bog. Never paste credentials into chat.");
            }
            Json(overview).into_response()
        }
    }
}
