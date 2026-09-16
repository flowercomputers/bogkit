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
            "/",
            get(|| async {
                guide_asset(
                    "text/html; charset=utf-8",
                    include_str!("../static/index.html"),
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
                    Ok(_) => (StatusCode::OK, Json(json!({"status":"ok"}))),
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

fn guide_asset(content_type: &'static str, body: &'static str) -> Response {
    ([
        (header::CONTENT_TYPE, content_type),
        (header::CONTENT_SECURITY_POLICY, "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CACHE_CONTROL, "no-cache"),
    ], body).into_response()
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
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or_else(|| CloudError::new("unauthorized", "bearer credentials required"))?;
    let principal = service.auth.authenticate(token)?;
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
    if path.len() < 2 || path[0] != "v1" || path[1] != "bogs" {
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
    let id = if path.len() > 2 {
        Some(BogId(
            Uuid::parse_str(&path[2]).map_err(|_| bad("invalid database ID"))?,
        ))
    } else {
        None
    };
    let op = match (method.as_str(), path.len()) {
        ("GET", 2) => Operation::ListBogs,
        ("POST", 2) => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Create {
                name: String,
                template: String,
            }
            let c: Create =
                serde_json::from_value(parse()?).map_err(|_| bad("expected name and template"))?;
            Operation::CreateBog {
                name: c.name,
                template: c.template,
                idempotency_key: key.ok_or_else(|| bad("Idempotency-Key required"))?,
            }
        }
        ("GET", 3) => Operation::DescribeBog {
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
