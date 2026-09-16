use bog_serve::{App, Durability};
use fold::pipeline::terminal;
use tower::ServiceExt;
#[tokio::test]
async fn shutdown_closes_watchers_rejects_requests_and_releases_store() {
    let dir = tempfile::tempdir().unwrap();
    let service = App::<String, _>::stream(dir.path(), terminal::Count::new("total"))
        .durability(Durability::CheckpointBeforeAck)
        .try_into_service()
        .unwrap();
    let router = service.router();
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::get("/watch")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    service.begin_shutdown();
    let status = router
        .clone()
        .oneshot(
            axum::http::Request::get("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status();
    assert_eq!(status, 503);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        axum::body::to_bytes(response.into_body(), 4096),
    )
    .await
    .unwrap()
    .unwrap();
    drop(router);
    service.shutdown().unwrap();
    let next = App::<String, _>::stream(dir.path(), terminal::Count::new("total"))
        .try_into_service()
        .unwrap();
    next.shutdown().unwrap();
}
