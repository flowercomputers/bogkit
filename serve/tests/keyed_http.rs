//! Keyed apps end to end: CRUD by key, search three ways, custom routes,
//! and the phase-2 exit criterion — forgetting a document over HTTP
//! removes it from every index.

use anny::metric::Cosine;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bog_serve::{KeyedApp, TextQuery};
use fold::pipeline::{Keyed, Map, terminal};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_stream::StreamExt;
use tower::ServiceExt;

mod common;
use common::send;

#[derive(Deserialize, JsonSchema)]
struct TopDocParams {
    q: String,
}

#[derive(Serialize, JsonSchema)]
struct TopDoc {
    id: u64,
    text: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct Claim {
    key: u64,
    text: String,
}

/// Deterministic toy embedder standing in for ese: 4 dims, char-bucket
/// counts, L2-normalized so cosine distances behave.
fn embed(s: &str) -> [f32; 4] {
    let mut v = [0.0f32; 4];
    for (i, b) in s.bytes().enumerate() {
        v[i % 4] += b as f32;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
    v.map(|x| x / norm)
}

/// The dogfood shape: BM25 + HNSW (text-queryable) + doc table, all fed by
/// one keyed stream of `id -> text`.
fn test_router() -> axum::Router {
    let dir = tempfile::tempdir().unwrap().keep();
    KeyedApp::stream(
        dir,
        (
            terminal::search::Bm25::new("bm25"),
            Map::new(
                |d: &Keyed<u64, String>| Keyed::new(d.key, embed(&d.val)),
                TextQuery::new(
                    terminal::search::Hnsw::<u64, f32, Cosine, 4>::new("vecs", Cosine, 42),
                    embed,
                ),
            ),
            terminal::Table::new("docs"),
        ),
    )
    // typed custom GET: query struct in, response struct out — both
    // schemas land in /openapi.json
    .get("/top_doc", |(bm25, _vecs, docs), p: TopDocParams| {
        Ok(bm25.search(&p.q, 1).into_iter().next().map(|hit| TopDoc {
            id: hit.val,
            text: docs.get(&hit.val),
        }))
    })
    // typed custom POST: atomic check-and-set — Err rolls the whole
    // transaction back, so a taken key is never overwritten
    .post("/claim", |tx, c: Claim| {
        if tx.contains(&c.key) {
            return Err((409, format!("key {} taken", c.key)));
        }
        tx.upsert(&c.key, &c.text);
        Ok(json!({ "claimed": c.key }))
    })
    .into_router()
}

async fn seed(router: &axum::Router) {
    for (id, text) in [
        (1, "the postgres database was slow"),
        (2, "deployed the api to kubernetes"),
        (3, "the user prefers rust for backends"),
    ] {
        let (status, _) = send(router, "PUT", &format!("/docs/{id}"), Some(json!(text))).await;
        assert_eq!(status, StatusCode::OK);
    }
}

#[tokio::test]
async fn crud_by_key() {
    let router = test_router();

    let (status, body) = send(&router, "PUT", "/docs/1", Some(json!("hello bog"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["replaced"], false);

    let (_, body) = send(&router, "GET", "/docs/1", None).await;
    assert_eq!(body["data"], "hello bog");

    // upsert replaces and reports it
    let (_, body) = send(&router, "PUT", "/docs/1", Some(json!("hello again"))).await;
    assert_eq!(body["replaced"], true);
    let (_, body) = send(&router, "GET", "/views/docs/1", None).await;
    assert_eq!(body["data"]["value"], "hello again");

    let (_, body) = send(&router, "DELETE", "/docs/1", None).await;
    assert_eq!(body["removed"], true);
    let (status, _) = send(&router, "GET", "/docs/1", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // deleting an absent key commits but reports removed: false
    let (_, body) = send(&router, "DELETE", "/docs/99", None).await;
    assert_eq!(body["removed"], false);
}

#[tokio::test]
async fn search_three_ways() {
    let router = test_router();
    seed(&router).await;

    // bm25: GET ?q=
    let (status, body) = send(&router, "GET", "/views/bm25/search?q=kubernetes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["key"], 2);

    // hnsw by text (via the TextQuery encoder)
    let (status, body) = send(
        &router,
        "GET",
        "/views/vecs/search?q=deployed%20the%20api%20to%20kubernetes&k=1",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["key"], 2, "self-similarity must win");

    // hnsw by raw vector (POST)
    let vector: Vec<f32> = embed("deployed the api to kubernetes").to_vec();
    let (status, body) = send(
        &router,
        "POST",
        "/views/vecs/search",
        Some(json!({ "vector": vector, "k": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"][0]["key"], 2);

    // wrong dimensionality is a 400, not a panic
    let (status, body) = send(
        &router,
        "POST",
        "/views/vecs/search",
        Some(json!({ "vector": [1.0, 2.0] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("4 dims"));

    // non-searchable views say so
    let (status, _) = send(&router, "GET", "/views/docs/search?q=x", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn forgetting_removes_from_every_index() {
    let router = test_router();
    seed(&router).await;

    // present everywhere before
    let (_, body) = send(&router, "GET", "/views/bm25/search?q=kubernetes", None).await;
    assert_eq!(body["data"][0]["key"], 2);

    let (_, body) = send(&router, "DELETE", "/docs/2", None).await;
    assert_eq!(body["removed"], true);

    // bm25: no hit for its terms
    let (_, body) = send(&router, "GET", "/views/bm25/search?q=kubernetes", None).await;
    assert!(
        body["data"].as_array().unwrap().is_empty(),
        "bm25 must forget: {body}"
    );

    // hnsw: doc 2 gone from the graph (its own text no longer finds it)
    let (_, body) = send(
        &router,
        "GET",
        "/views/vecs/search?q=deployed%20the%20api%20to%20kubernetes",
        None,
    )
    .await;
    let keys: Vec<u64> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["key"].as_u64().unwrap())
        .collect();
    assert!(!keys.contains(&2), "hnsw must forget: {keys:?}");

    // table and primary store: gone
    let (status, _) = send(&router, "GET", "/views/docs/2", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&router, "GET", "/docs/2", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn keyed_batch_is_one_transaction() {
    let router = test_router();
    seed(&router).await;

    let ops = json!([
        { "op": "upsert", "key": 4, "data": "a brand new memory" },
        { "op": "remove", "key": 1 },
    ]);
    let (status, body) = send(&router, "POST", "/batch", Some(ops)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["applied"], 2);
    let seq = body["seq"].as_u64().unwrap();

    let (_, body) = send(&router, "GET", "/docs/4", None).await;
    assert_eq!(body["data"], "a brand new memory");
    assert_eq!(body["seq"].as_u64().unwrap(), seq, "one tx, one seq");
    let (status, _) = send(&router, "GET", "/docs/1", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn custom_route_reads_the_same_snapshot() {
    let router = test_router();
    seed(&router).await;

    let (status, body) = send(&router, "GET", "/top_doc?q=kubernetes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["id"], 2);
    assert_eq!(body["data"]["text"], "deployed the api to kubernetes");

    // missing required param: rejected by the typed extractor, field named
    let (status, body) = send(&router, "GET", "/top_doc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("q"));
}

#[tokio::test]
async fn custom_post_is_atomic_check_and_set() {
    let router = test_router();
    seed(&router).await; // seq 3

    let claim = json!({ "key": 9, "text": "the deploy runs at midnight" });
    let (status, body) = send(&router, "POST", "/claim", Some(claim)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["claimed"], 9);
    assert_eq!(body["seq"], 4, "successful claim commits");

    // second claim: refused, and the whole transaction rolled back
    let steal = json!({ "key": 9, "text": "overwritten!" });
    let (status, body) = send(&router, "POST", "/claim", Some(steal)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("taken"));

    let (_, body) = send(&router, "GET", "/docs/9", None).await;
    assert_eq!(
        body["data"], "the deploy runs at midnight",
        "original survives"
    );
    assert_eq!(body["seq"], 4, "rejected claim must not commit a seq");

    // the rolled-back text was never indexed anywhere
    let (_, body) = send(&router, "GET", "/views/bm25/search?q=overwritten", None).await;
    assert!(body["data"].as_array().unwrap().is_empty());

    // malformed body: rejected before any transaction, field named
    let (status, body) = send(&router, "POST", "/claim", Some(json!({ "key": 10 }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("text"));
}

#[tokio::test]
async fn watch_emits_commit_events() {
    let router = test_router();
    seed(&router).await; // seq is now 3

    let req = Request::builder()
        .uri("/watch")
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );

    // the current seq arrives immediately on connect
    let mut body = resp.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
        .await
        .expect("an event within 5s")
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&first);
    assert!(text.contains("{\"seq\":3}"), "got: {text}");
}

#[tokio::test]
async fn keyed_openapi_and_schema() {
    let router = test_router();

    let (_, doc) = send(&router, "GET", "/openapi.json", None).await;
    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/docs/{key}",
        "/batch",
        "/views/bm25/search",
        "/views/vecs/search",
        "/views/docs/{key}",
        "/top_doc",
        "/claim",
        "/watch",
    ] {
        assert!(paths.contains_key(p), "missing path {p}");
    }
    // vecs is text+vector: both operations documented
    assert!(paths["/views/vecs/search"].get("get").is_some());
    assert!(paths["/views/vecs/search"].get("post").is_some());
    // bm25 is text-only
    assert!(paths["/views/bm25/search"].get("post").is_none());

    // custom routes are fully typed in the doc: the GET documents its
    // query params, the POST its body and response schemas
    let top_doc = &paths["/top_doc"]["get"];
    assert_eq!(top_doc["parameters"][0]["name"], "q");
    assert_eq!(top_doc["parameters"][0]["required"], true);
    let claim_body =
        &paths["/claim"]["post"]["requestBody"]["content"]["application/json"]["schema"];
    assert!(claim_body["properties"]["key"].is_object());
    assert!(claim_body["properties"]["text"].is_object());
    let claim_resp =
        &paths["/claim"]["post"]["responses"]["200"]["content"]["application/json"]["schema"];
    assert_eq!(claim_resp["properties"]["seq"]["type"], "integer");

    let (_, schema) = send(&router, "GET", "/schema", None).await;
    assert_eq!(
        schema["write"]["keyed"]["type"], "integer",
        "u64 key schema"
    );
    let kinds: Vec<&str> = schema["views"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["bm25", "hnsw", "table"]);
}
