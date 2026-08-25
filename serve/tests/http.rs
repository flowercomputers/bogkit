//! Drive every generated route through the router in-process: tower's
//! `oneshot` sends one request through the service without a listener.

use axum::http::StatusCode;
use bog_serve::App;
use fold::pipeline::{KeyBy, terminal};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod common;
use common::send;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct Entry {
    id: u64,
    text: String,
}

/// Every phase-1 sink kind, plus an operator (KeyBy) to prove readers pass
/// through operators untouched.
fn test_router() -> axum::Router {
    let dir = tempfile::tempdir().unwrap().keep();
    App::stream(
        dir,
        (
            terminal::Count::new("total"),
            terminal::Bag::<Entry>::new("entries"),
            KeyBy::new(|e: &Entry| e.id, terminal::Table::new("by_id")),
        ),
    )
    .into_router()
}

fn entry(id: u64, text: &str) -> Value {
    json!({ "id": id, "text": text })
}

#[tokio::test]
async fn healthz() {
    let router = test_router();
    let (status, body) = send(&router, "GET", "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, Value::String("ok".into()));
}

#[tokio::test]
async fn writes_flow_to_every_view() {
    let router = test_router();

    let (status, body) = send(&router, "POST", "/insert", Some(entry(1, "peat"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["seq"], 1);
    send(&router, "POST", "/insert", Some(entry(2, "moss"))).await;
    let (_, body) = send(&router, "POST", "/remove", Some(entry(1, "peat"))).await;
    assert_eq!(body["seq"], 3);

    // count: retraction rolled entry 1 back out
    let (status, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["seq"], 3);
    assert_eq!(body["data"]["value"], 1);

    // bag: only entry 2 remains
    let (_, body) = send(&router, "GET", "/views/entries", None).await;
    let items = body["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["value"]["text"], "moss");
    assert_eq!(items[0]["count"], 1);

    // table through the KeyBy operator: point lookup by key
    let (status, body) = send(&router, "GET", "/views/by_id/2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["value"]["text"], "moss");
}

#[tokio::test]
async fn batch_commits_atomically_or_rejects_wholly() {
    let router = test_router();

    let ops = json!([
        { "op": "insert", "data": entry(1, "a") },
        { "op": "insert", "data": entry(2, "b") },
        { "op": "remove", "data": entry(1, "a") },
    ]);
    let (status, body) = send(&router, "POST", "/batch", Some(ops)).await;
    assert_eq!(status, StatusCode::OK);
    // three ops, ONE transaction, one seq
    assert_eq!(body["seq"], 1);
    assert_eq!(body["applied"], 3);
    let (_, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(body["data"]["value"], 1);

    // a malformed op anywhere rejects the whole batch before any write
    let bad = json!([
        { "op": "insert", "data": entry(3, "c") },
        { "op": "bogus" },
    ]);
    let (status, _) = send(&router, "POST", "/batch", Some(bad)).await;
    assert!(status.is_client_error());
    let (_, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(
        body["data"]["value"], 1,
        "rejected batch must write nothing"
    );
    assert_eq!(body["seq"], 1, "rejected batch must not commit");
}

#[tokio::test]
async fn read_errors() {
    let router = test_router();
    send(&router, "POST", "/insert", Some(entry(1, "a"))).await;

    let (status, _) = send(&router, "GET", "/views/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "unknown view");

    let (status, _) = send(&router, "GET", "/views/by_id/999", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "known view, absent key");

    let (status, _) = send(&router, "GET", "/views/by_id/abc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unparseable key for u64");

    let (status, _) = send(&router, "GET", "/views/entries/1", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "bags have no key lookup");
}

#[tokio::test]
async fn pagination() {
    let router = test_router();
    for id in 0..5 {
        send(&router, "POST", "/insert", Some(entry(id, "x"))).await;
    }

    let (_, body) = send(&router, "GET", "/views/entries?limit=2", None).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 2);

    let (_, body) = send(&router, "GET", "/views/entries?limit=10&offset=4", None).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn openapi_reflects_the_pipeline() {
    let router = test_router();
    let (status, doc) = send(&router, "GET", "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["openapi"], "3.1.0");

    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/insert",
        "/remove",
        "/batch",
        "/views/total",
        "/views/entries",
        "/views/by_id",
        "/views/by_id/{key}",
        "/healthz",
    ] {
        assert!(paths.contains_key(p), "missing path {p}");
    }

    // the write body documents the actual input type
    let input =
        &doc["paths"]["/insert"]["post"]["requestBody"]["content"]["application/json"]["schema"];
    assert!(input["properties"]["id"].is_object());
    assert!(input["properties"]["text"].is_object());
}

#[tokio::test]
async fn schema_fingerprint_is_stable_across_builds() {
    let (_, a) = send(&test_router(), "GET", "/schema", None).await;
    let (_, b) = send(&test_router(), "GET", "/schema", None).await;
    assert!(a["fingerprint"].as_str().unwrap().len() == 16);
    assert_eq!(a["fingerprint"], b["fingerprint"]);
    assert_eq!(a["views"].as_array().unwrap().len(), 3);
}
