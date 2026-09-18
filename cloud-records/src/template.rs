use std::path::Path;

use axum::body::{Body, to_bytes};
use axum::extract::DefaultBodyLimit;
use axum::http::{Request, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use bog_serve::{Durability, KeyedApp, ServedApp};
use fold::pipeline::terminal;
use serde_json::{Value, json};

use crate::JsonDocument;

pub const DEFAULT_LOGICAL_BYTES: u64 = 16 * 1024 * 1024;

pub const TEMPLATE_ID: &str = "records-v1";
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_BATCH_OPS: usize = 100;
pub const MAX_LIST_LIMIT: usize = 1000;
pub const MAX_LIST_OFFSET: usize = 10_000;
pub const MAX_KEY_BYTES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("failed to open records store: {0}")]
    OpenStore(#[source] fold::fjall::Error),
    #[error("failed to initialize records template: {0}")]
    Initialize(String),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RecordPage {
    limit: Option<usize>,
    offset: Option<usize>,
}

pub fn records_router(path: &Path) -> Result<Router, TemplateError> {
    Ok(records_service(path)?.router())
}

/// Durable records template with explicit ownership of shutdown/checkpoint.
pub fn records_service(path: &Path) -> Result<ServedApp, TemplateError> {
    records_service_with_limit(path, DEFAULT_LOGICAL_BYTES)
}

/// Logical bytes sum compact JSON encodings of each primary key and value.
/// Key quotes/escapes and UTF-8 bytes count; separators, indexes and disk overhead do not.
/// Object keys use serde_json's stable sorted map encoding. State is measured from
/// the durable table on every transaction, including the first after reopening.
pub fn records_service_with_limit(path: &Path, limit: u64) -> Result<ServedApp, TemplateError> {
    let service = KeyedApp::<String, JsonDocument, _>::try_stream(
        path,
        (terminal::Table::new("docs"), terminal::Count::new("total")),
    )
    .map_err(TemplateError::OpenStore)?
    .raw_string_keys()
    .logical_quota(limit, |tx| {
        tx.rtx(|(docs, _)| {
            docs.iter()
                .map(|(key, value)| {
                    serde_json::to_vec(&key).unwrap().len() as u64
                        + serde_json::to_vec(&value).unwrap().len() as u64
                })
                .sum()
        })
    })
    .get("/_cloud/usage", move |(docs, _), _: RecordPage| {
        let used: u64 = docs
            .iter()
            .map(|(key, value)| {
                serde_json::to_vec(&key).unwrap().len() as u64
                    + serde_json::to_vec(&value).unwrap().len() as u64
            })
            .sum();
        Ok(json!({"logical_bytes":used,"limit_bytes":limit,"over_limit":used > limit}))
    })
    .get("/_cloud/sequence", |(_, _), _: RecordPage| { Ok(json!({})) })
    .get("/_cloud/export", |(docs, _), page: RecordPage| {
        use sha2::{Digest,Sha256};
        let all: std::collections::BTreeMap<_,_> = docs.iter().collect();
        let mut hash=Sha256::new();
        for (k,v) in &all { hash.update(serde_json::to_vec(&(k,v)).unwrap()); hash.update(b"\n"); }
        let offset=page.offset.unwrap_or(0);
        let mut records=Vec::new();let mut bytes=0;
        for (key,value) in all.iter().skip(offset) {
            let item=json!({"key":key,"value":value});let n=item.to_string().len();
            if !records.is_empty() && bytes+n>768*1024 {break;}
            bytes+=n;records.push(item);if records.len()==100 {break;}
        }
        let next=offset+records.len();
        Ok(json!({"records":records,"record_count":all.len(),"source_digest":format!("{:x}",hash.finalize()),"next_offset":if next<all.len(){Some(next)}else{None}}))
    })
    .get("/views/docs", |(docs, _total), page: RecordPage| {
        let mut bytes = 0usize;
        let mut items = Vec::new();
        for (key, value) in docs
            .iter()
            .skip(page.offset.unwrap_or(0))
            .take(page.limit.unwrap_or(100))
        {
            let item = json!({"key":key,"value":value});
            bytes += serde_json::to_vec(&item)
                .map_err(|_| (503, "cannot encode view".into()))?
                .len();
            if bytes > 4 * 1024 * 1024 - 4096 {
                return Err((413, "reduce the requested page size".into()));
            }
            items.push(item);
        }
        Ok(Value::Array(items))
    })
    .durability(Durability::CheckpointBeforeAck)
    .try_into_service()
    .map_err(|e| TemplateError::Initialize(e.to_string()))?;
    Ok(service.map_router(|router| {
        router
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .layer(from_fn(validate_request))
    }))
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
            if let Err(message) = validate_batch(&value) {
                return bad_request(message);
            }
        }
        request = Request::from_parts(parts, Body::from(bytes));
    }
    next.run(request).await
}

/// Validate the complete records-v1 batch before routing or dispatching it.
pub fn validate_batch(value: &Value) -> Result<(), String> {
    let ops = value.as_array().ok_or("batch must be an array")?;
    if ops.len() > MAX_BATCH_OPS {
        return Err(format!(
            "batch exceeds maximum of {MAX_BATCH_OPS} operations"
        ));
    }
    if value.to_string().len() > MAX_REQUEST_BYTES {
        return Err("request body exceeds 1 MiB".into());
    }
    for op in ops {
        let key = op
            .get("key")
            .and_then(Value::as_str)
            .ok_or("each batch operation requires a string key")?;
        validate_key(key)?;
        match op.get("op").and_then(Value::as_str) {
            Some("upsert") => {
                let data = op.get("data").ok_or("upsert requires data")?;
                JsonDocument::try_from_value(data.clone()).map_err(|e| e.to_string())?;
            }
            Some("remove") => {}
            _ => return Err("batch operation must be upsert or remove".into()),
        }
    }
    Ok(())
}

/// Validate a records-v1 primary key without interpreting it as JSON.
pub fn validate_key(key: &str) -> Result<(), String> {
    if key == "." || key == ".." {
        return Err("key must not be a dot-only path segment".into());
    }
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
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            decoded.push(hi * 16 + lo);
            i += 3;
            continue;
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
