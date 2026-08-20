//! Golden OpenAPI snapshots: the generated doc for each reference pipeline
//! shape is pinned to a file. Drift fails CI; intended changes are
//! re-recorded with `UPDATE_GOLDEN=1 cargo test -p bog-serve --test golden`.

use std::path::Path;

use anny::metric::Cosine;
use axum::body::Body;
use axum::http::Request;
use bog_serve::{App, KeyedApp, NoParams, TextQuery};
use fold::pipeline::{Keyed, Map, terminal};
use http_body_util::BodyExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct Entry {
    text: String,
}

/// The `bogkit new --kind server` template pipeline, verbatim.
fn template_router() -> axum::Router {
    let dir = tempfile::tempdir().unwrap().keep();
    App::stream(
        dir,
        (
            terminal::Count::new("total"),
            terminal::Bag::<Entry>::new("entries"),
        ),
    )
    .into_router()
}

/// The search-server shape (toy 4-dim embedder standing in for ese).
fn search_router() -> axum::Router {
    fn embed(s: &str) -> [f32; 4] {
        let mut v = [0.0f32; 4];
        for (i, b) in s.bytes().enumerate() {
            v[i % 4] += b as f32;
        }
        v
    }
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
    .get("/search/hybrid", |_readers, _: NoParams| Ok(Value::Null))
    .into_router()
}

async fn openapi(router: axum::Router) -> Value {
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn check_golden(name: &str, doc: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let pretty = serde_json::to_string_pretty(doc).unwrap() + "\n";

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &pretty).unwrap();
        return;
    }

    let stored = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing golden {name} — record it with UPDATE_GOLDEN=1 cargo test -p bog-serve --test golden")
    });
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(
        &stored, doc,
        "\nthe generated OpenAPI doc for {name} drifted from its golden file.\n\
         If this change is intended, re-record with:\n\
         UPDATE_GOLDEN=1 cargo test -p bog-serve --test golden\n"
    );
}

#[tokio::test]
async fn template_openapi_matches_golden() {
    check_golden("template.json", &openapi(template_router()).await);
}

#[tokio::test]
async fn search_openapi_matches_golden() {
    check_golden("search.json", &openapi(search_router()).await);
}
