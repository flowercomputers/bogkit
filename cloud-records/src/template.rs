use std::path::Path;

use axum::body::{Body, to_bytes};
use axum::extract::DefaultBodyLimit;
use axum::http::{Request, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use bog_serve::KeyedApp;
use fold::pipeline::terminal;
use serde_json::{Value, json};

use crate::JsonDocument;

pub const TEMPLATE_ID: &str = "records-v1";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_BATCH_OPS: usize = 100;
const MAX_LIST_LIMIT: usize = 1000;
const MAX_LIST_OFFSET: usize = 10_000;
const MAX_KEY_BYTES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("failed to open records store: {0}")]
    OpenStore(#[source] fold::fjall::Error),
    #[error("failed to initialize records template: {0}")]
    Initialize(String),
}

pub fn records_router(path: &Path) -> Result<Router, TemplateError> {
    let app = KeyedApp::<String, JsonDocument, _>::try_stream(
        path,
        (terminal::Table::new("docs"), terminal::Count::new("total")),
    )
    .map_err(TemplateError::OpenStore)?;
    let app = app.raw_string_keys();
    let router = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.into_router()))
        .map_err(|panic| TemplateError::Initialize(panic_message(panic)))?;
    Ok(router
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(from_fn(validate_request)))
}

async fn validate_request(mut request: Request<Body>, next: Next) -> Response {
    if let Err(message) = validate_path_key(&request) {
        return bad_request(message);
    }
    if let Err(message) = validate_page(request.uri()) {
        return bad_request(message);
    }
    if matches!(
        *request.method(),
        axum::http::Method::PUT | axum::http::Method::POST
    ) {
        let (parts, body) = request.into_parts();
        let bytes = match to_bytes(body, MAX_REQUEST_BYTES).await {
            Ok(bytes) => bytes,
            Err(_) => return payload_too_large(),
        };
        if parts.uri.path() == "/batch" {
            let value: Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(error) => return bad_request(format!("invalid batch: {error}")),
            };
            let Some(ops) = value.as_array() else {
                return bad_request("batch must be an array");
            };
            if ops.len() > MAX_BATCH_OPS {
                return bad_request(format!(
                    "batch exceeds maximum of {MAX_BATCH_OPS} operations"
                ));
            }
            for op in ops {
                let Some(key) = op.get("key").and_then(Value::as_str) else {
                    return bad_request("each batch operation requires a string key");
                };
                if let Err(message) = validate_key(key) {
                    return bad_request(message);
                }
            }
        }
        request = Request::from_parts(parts, Body::from(bytes));
    }
    next.run(request).await
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("key must not be empty".into());
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(format!("key exceeds maximum of {MAX_KEY_BYTES} bytes"));
    }
    if key.contains('/') {
        return Err("key must not contain slash".into());
    }
    if key.chars().any(char::is_control) {
        return Err("key must not contain control characters".into());
    }
    Ok(())
}

fn validate_page(uri: &axum::http::Uri) -> Result<(), String> {
    if !uri.path().starts_with("/views/") {
        return Ok(());
    }
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(uri.query().unwrap_or_default())
        .map_err(|error| format!("invalid query: {error}"))?;
    for (name, value) in pairs {
        let parsed = value.parse::<usize>().ok();
        match (name.as_str(), parsed) {
            ("limit", Some(limit)) if limit > MAX_LIST_LIMIT => {
                return Err(format!("limit exceeds maximum of {MAX_LIST_LIMIT}"));
            }
            ("offset", Some(offset)) if offset > MAX_LIST_OFFSET => {
                return Err(format!("offset exceeds maximum of {MAX_LIST_OFFSET}"));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_path_key(request: &Request<Body>) -> Result<(), String> {
    let path = request.uri().path();
    let raw_key = if let Some(key) = path.strip_prefix("/docs/") {
        key
    } else if let Some(key) = path.strip_prefix("/views/docs/") {
        key
    } else {
        return Ok(());
    };
    let key = percent_decode(raw_key)?;
    validate_key(&key)
}

fn percent_decode(raw: &str) -> Result<String, String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                decoded.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(decoded).map_err(|_| "key must be valid UTF-8".into())
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn payload_too_large() -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({"error": "request body exceeds 1 MiB"})),
    )
        .into_response()
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_string()
    } else {
        "records template initialization panicked".into()
    }
}
