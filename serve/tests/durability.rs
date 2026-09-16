use bog_serve::{App, Durability};
use fold::pipeline::terminal;
use tower::ServiceExt;
#[tokio::test]
async fn durable_mutations_and_failed_custom_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let service = App::stream(dir.path(), terminal::Count::new("total"))
        .durability(Durability::CheckpointBeforeAck)
        .post("/reject", |tx, value: String| {
            tx.insert(&value);
            Err::<String, _>((422, "rejected".into()))
        })
        .try_into_service()
        .unwrap();
    for (path, body, status) in [
        ("/insert", "\"x\"", 200),
        ("/remove", "\"x\"", 200),
        ("/batch", "[{\"op\":\"insert\",\"data\":\"x\"}]", 200),
        ("/reject", "\"x\"", 422),
    ] {
        let response = service
            .router()
            .oneshot(
                axum::http::Request::post(path)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    service.shutdown().unwrap();
}

/// Local diagnostic only; no capacity or latency guarantee is inferred.
#[tokio::test]
#[ignore]
async fn checkpoint_latency_sample() {
    let dir = tempfile::tempdir().unwrap();
    let service = App::<String, _>::stream(dir.path(), terminal::Count::new("total"))
        .durability(Durability::CheckpointBeforeAck)
        .try_into_service()
        .unwrap();
    let mut times = Vec::new();
    for n in 0..30 {
        let start = std::time::Instant::now();
        let response = service
            .router()
            .oneshot(
                axum::http::Request::post("/insert")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(format!("\"{n}\"")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        times.push(start.elapsed());
    }
    times.sort();
    eprintln!(
        "Local checkpoint-before-ack request latency (30 samples): median {:?}, max {:?}",
        times[15], times[29]
    );
    service.shutdown().unwrap();
}
