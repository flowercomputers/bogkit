//! Phase-3 coverage: every remaining sink kind served, the JSON error
//! model, ordered listings, per-view watch streams, and the schema
//! fingerprint guard.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bog_serve::App;
use fold::pipeline::{Keyed, KeyBy, Map, ScoreBy, terminal};
use http_body_util::BodyExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tower::ServiceExt;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct Reading {
    station: String,
    temp: i64,
}

/// One pipeline exercising every non-search sink kind at once.
fn all_sinks_router() -> axum::Router {
    let dir = tempfile::tempdir().unwrap().keep();
    App::stream(
        dir,
        (
            terminal::Count::new("total"),
            terminal::Stats::new("temp_stats", |r: &Reading| r.temp as f64),
            ScoreBy::new(
                |r: &Reading| r.temp,
                (
                    terminal::Ranked::new("by_temp"),
                    terminal::Histogram::new("temp_hist", |t: &i64| (t / 10) * 10),
                ),
            ),
            KeyBy::new(
                |r: &Reading| r.station.clone(),
                terminal::Multimap::new("by_station"),
            ),
            Map::new(
                |r: &Reading| Keyed::new(r.station.clone(), (r.temp / 10) * 10),
                terminal::InvertedIndex::<String, i64>::new("stations_by_bucket"),
            ),
        ),
    )
    // custom POST for the rollback test: inserts, then rejects hot
    // readings — the Err must un-insert
    .post("/insert_checked", |tx, r: Reading| {
        tx.insert(&r);
        if r.temp > 100 {
            return Err((422, format!("{} is implausibly hot", r.temp)));
        }
        Ok(json!({ "accepted": r.station }))
    })
    .into_router()
}

async fn send(
    router: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = match body {
        Some(v) => Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    };
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, value)
}

fn reading(station: &str, temp: i64) -> Value {
    json!({ "station": station, "temp": temp })
}

async fn seed(router: &axum::Router) {
    let ops: Vec<Value> = [("alpha", 12), ("alpha", 18), ("beta", 25), ("beta", 31)]
        .iter()
        .map(|(s, t)| json!({ "op": "insert", "data": reading(s, *t) }))
        .collect();
    let (status, _) = send(router, "POST", "/batch", Some(json!(ops))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn every_sink_kind_serves() {
    let router = all_sinks_router();
    seed(&router).await;

    let (_, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(body["data"]["value"], 4);

    // stats: one object with the moments
    let (_, body) = send(&router, "GET", "/views/temp_stats", None).await;
    assert_eq!(body["data"]["count"], 4);
    assert_eq!(body["data"]["sum"], 86.0);
    assert_eq!(body["data"]["mean"], 21.5);

    // ranked ascending, and ?desc=true for top-first
    let (_, body) = send(&router, "GET", "/views/by_temp", None).await;
    assert_eq!(body["data"][0]["score"], 12);
    let (_, body) = send(&router, "GET", "/views/by_temp?desc=true&limit=1", None).await;
    assert_eq!(body["data"][0]["score"], 31);
    assert_eq!(body["data"][0]["value"]["station"], "beta");

    // histogram: decade buckets plus the total
    let (_, body) = send(&router, "GET", "/views/temp_hist", None).await;
    assert_eq!(body["data"]["total"], 4);
    assert_eq!(
        body["data"]["buckets"],
        json!([
            { "bucket": 10, "count": 2 },
            { "bucket": 20, "count": 1 },
            { "bucket": 30, "count": 1 },
        ])
    );

    // multimap: all readings posted under a station
    let (_, body) = send(&router, "GET", "/views/by_station/alpha", None).await;
    assert_eq!(body["data"]["values"].as_array().unwrap().len(), 2);

    // inverted index: stations posted under a temp bucket
    let (_, body) = send(&router, "GET", "/views/stations_by_bucket/10", None).await;
    assert_eq!(body["data"]["keys"], json!(["alpha"]));

    // point-lookup views explain themselves on a bare GET
    let (status, body) = send(&router, "GET", "/views/by_station", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("point lookups"));
}

#[tokio::test]
async fn retraction_rolls_back_every_sink() {
    let router = all_sinks_router();
    seed(&router).await;

    send(&router, "POST", "/remove", Some(reading("beta", 31))).await;

    let (_, body) = send(&router, "GET", "/views/temp_stats", None).await;
    assert_eq!(body["data"]["count"], 3);
    assert_eq!(body["data"]["sum"], 55.0);

    let (_, body) = send(&router, "GET", "/views/by_temp?desc=true&limit=1", None).await;
    assert_eq!(body["data"][0]["score"], 25, "31 retracted from ranked");

    let (_, body) = send(&router, "GET", "/views/temp_hist", None).await;
    assert_eq!(body["data"]["total"], 3);

    let (_, body) = send(&router, "GET", "/views/stations_by_bucket/30", None).await;
    assert_eq!(body["data"]["keys"], json!([]), "posting deleted");
}

#[tokio::test]
async fn errors_are_json_with_field_detail() {
    let router = all_sinks_router();

    // missing field: serde's message names it
    let (status, body) = send(&router, "POST", "/insert", Some(json!({ "station": "x" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let msg = body["error"].as_str().unwrap();
    assert!(msg.contains("temp"), "field named in: {msg}");

    // syntactically broken body
    let req = Request::builder()
        .method("POST")
        .uri("/insert")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).expect("error body is JSON");
    assert!(body["error"].is_string());

    // bad query parameter
    let (status, body) = send(&router, "GET", "/views/by_temp?limit=abc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("invalid query"));
}

#[tokio::test]
async fn custom_post_rolls_back_on_err() {
    let router = all_sinks_router();
    seed(&router).await; // 4 readings, seq 1

    // rejected after the insert was already pushed: everything rolls back
    let (status, body) =
        send(&router, "POST", "/insert_checked", Some(reading("volcano", 999))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("implausibly hot"));
    let (_, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(body["data"]["value"], 4, "rolled-back insert must not count");
    assert_eq!(body["seq"], 1, "no commit, no seq");

    // accepted: commits like any generated write
    let (status, body) =
        send(&router, "POST", "/insert_checked", Some(reading("delta", 21))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["seq"], 2);
    let (_, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(body["data"]["value"], 5);
}

#[tokio::test]
async fn view_watch_streams_fresh_payloads() {
    let router = all_sinks_router();
    seed(&router).await; // seq 1

    let req = Request::builder()
        .uri("/views/total/watch")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut events = resp.into_body().into_data_stream();

    let first = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
        .await
        .expect("initial event")
        .unwrap()
        .unwrap();
    let first = String::from_utf8_lossy(&first);
    assert!(first.contains("\"seq\":1") && first.contains("\"value\":4"), "got: {first}");

    // a commit through a different clone of the router pushes a fresh payload
    send(&router, "POST", "/insert", Some(reading("gamma", 7))).await;
    let second = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
        .await
        .expect("post-commit event")
        .unwrap()
        .unwrap();
    let second = String::from_utf8_lossy(&second);
    assert!(second.contains("\"seq\":2") && second.contains("\"value\":5"), "got: {second}");

    // unknown views 404 immediately instead of hanging a stream
    let (status, _) = send(&router, "GET", "/views/nope/watch", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[should_panic(expected = "pipeline changed")]
async fn changed_pipeline_refuses_stale_data_dir() {
    let dir = tempfile::tempdir().unwrap().keep();

    // first open records the fingerprint; drop closes the store.
    // (Count accepts any input type, so D needs the turbofish here.)
    let first = App::<Reading, _>::stream(&dir, terminal::Count::new("total")).into_router();
    drop(first);

    // same dir, same input type, different sink structure: must refuse
    let _ = App::stream(
        &dir,
        (
            terminal::Count::new("total"),
            terminal::Bag::<Reading>::new("extras"),
        ),
    )
    .into_router();
}
