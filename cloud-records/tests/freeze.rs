use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::{get, put},
};
use bog_cloud_records::{
    ConfiguredService, DEFAULT_LOGICAL_BYTES, records_service, with_write_freeze,
};
use bog_definition::Definition;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
async fn call(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn check_freeze(router: Router) {
    assert_eq!(
        call(&router, "PUT", "/docs/a", json!({"title":"before"}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, paused) = call(&router, "POST", "/_cloud/pause_writes", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(paused["paused"], true);
    for (method, path, body) in [
        ("PUT", "/docs/b", json!({"title":"after"})),
        ("DELETE", "/docs/a", json!({})),
        ("POST", "/batch", json!([{"op":"remove","key":"a"}])),
    ] {
        let (status, value) = call(&router, method, path, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(value["error"]["code"], "writes_paused");
    }
    assert_eq!(
        call(&router, "GET", "/docs/a", json!({})).await.0,
        StatusCode::OK
    );
    let total = call(&router, "GET", "/views/total", json!({})).await.1["data"].clone();
    assert_eq!(total.get("value").unwrap_or(&total), &json!(1));
    assert_eq!(
        call(&router, "GET", "/_cloud/export?offset=0", json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, "POST", "/_cloud/resume_writes", json!({}))
            .await
            .1["paused"],
        false
    );
    assert_eq!(
        call(&router, "PUT", "/docs/b", json!({"title":"after"}))
            .await
            .0,
        StatusCode::OK
    );
}
#[tokio::test]
async fn legacy_worker_freezes_writes_and_keeps_reads() {
    let dir = tempfile::tempdir().unwrap();
    let service = records_service(dir.path()).unwrap();
    check_freeze(with_write_freeze(
        service.router(),
        &Definition::records_v1(),
    ))
    .await;
}
#[tokio::test]
async fn configured_worker_freezes_aliases_and_generic_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let d = Definition::records_v1();
    let service = ConfiguredService::open(dir.path(), d.clone(), 1, DEFAULT_LOGICAL_BYTES).unwrap();
    let router = with_write_freeze(service.router(), &d);
    check_freeze(router.clone()).await;
    call(&router, "POST", "/_cloud/pause_writes", json!({})).await;
    assert_eq!(
        call(
            &router,
            "POST",
            "/operations/put",
            json!({"key":"x","data":{}})
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        call(
            &router,
            "POST",
            "/operations/%70ut",
            json!({"key":"encoded","data":{}})
        )
        .await
        .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        call(&router, "POST", "/operations/total", json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, "POST", "/_cloud/import", json!({"records":[]}))
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
#[tokio::test]
async fn pause_ack_waits_for_inflight_worker_mutation() {
    let started = Arc::new(tokio::sync::Notify::new());
    let finish = Arc::new(tokio::sync::Notify::new());
    let started_handler = started.clone();
    let finish_handler = finish.clone();
    let router = Router::new()
        .route(
            "/docs/{key}",
            put(move || {
                let started = started_handler.clone();
                let finish = finish_handler.clone();
                async move {
                    started.notify_one();
                    finish.notified().await;
                    axum::Json(json!({"committed":true}))
                }
            }),
        )
        .route("/read", get(|| async { axum::Json(json!({"read":true})) }));
    let router = with_write_freeze(router, &Definition::records_v1());
    let write = tokio::spawn({
        let router = router.clone();
        async move { call(&router, "PUT", "/docs/a", json!({})).await }
    });
    started.notified().await;
    // Keep the worker mutation in flight while verifying that pause waits for it.
    let pause = tokio::spawn({
        let router = router.clone();
        async move { call(&router, "POST", "/_cloud/pause_writes", json!({})).await }
    });
    tokio::task::yield_now().await;
    assert!(!pause.is_finished());
    assert_eq!(
        call(&router, "GET", "/read", json!({})).await.0,
        StatusCode::OK
    );
    finish.notify_one();
    assert_eq!(write.await.unwrap().0, StatusCode::OK);
    assert_eq!(pause.await.unwrap().1["paused"], true);
    assert_eq!(
        call(&router, "PUT", "/docs/a", json!({})).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
