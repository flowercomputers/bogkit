use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use bog_cloud_records::{DocumentError, JsonDocument, TEMPLATE_ID, records_router};
use fold::pipeline::{Keyed, Push, terminal};
use fold::stream::KeyedStream;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn pipeline() -> impl Push<Keyed<String, JsonDocument>> {
    (terminal::Table::new("docs"), terminal::Count::new("total"))
}

fn doc(value: Value) -> JsonDocument {
    JsonDocument::try_from_value(value).unwrap()
}

async fn send(
    router: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = match body {
        Some(body) => Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap(),
    };
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

#[test]
fn json_codec_preserves_objects_across_postcard_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let value = json!({
        "title": "moss 🌿", "done": false, "count": 42,
        "nothing": null, "nested": {"tags": ["a", "b"], "ratio": 1.25}
    });
    let replacement = json!({
        "empty": {}, "items": [], "escaped": "quote: \" slash: \\ newline: \n",
        "min": i64::MIN, "max": u64::MAX
    });
    {
        let mut store = KeyedStream::try_new(dir.path(), pipeline()).unwrap();
        store.wtx(|tx| tx.upsert(&"moss".to_owned(), &doc(value.clone())));
        assert_eq!(store.get(&"moss".to_owned()).unwrap().as_value(), &value);
        store.wtx(|tx| tx.upsert(&"moss".to_owned(), &doc(replacement.clone())));
        assert_eq!(
            store.get(&"moss".to_owned()).unwrap().as_value(),
            &replacement
        );
        store.wtx(|tx| {
            tx.upsert(&"one".to_owned(), &doc(json!({"n": 1})));
            tx.upsert(&"two".to_owned(), &doc(json!({"n": 2})));
        });
        assert_eq!(
            store.get(&"one".to_owned()).unwrap().as_value(),
            &json!({"n": 1})
        );
        store.checkpoint();
    }
    let store = KeyedStream::try_new(dir.path(), pipeline()).unwrap();
    assert_eq!(
        store.get(&"moss".to_owned()).unwrap().as_value(),
        &replacement
    );
    assert_eq!(
        store.get(&"two".to_owned()).unwrap().as_value(),
        &json!({"n": 2})
    );
    let encoded = postcard::to_stdvec(&doc(value.clone())).unwrap();
    let decoded: JsonDocument = postcard::from_bytes(&encoded).unwrap();
    assert_eq!(decoded.as_value(), &value);
}

#[test]
fn document_validation_rejects_wrong_roots_depth_and_size() {
    for scalar in [
        json!(null),
        json!(true),
        json!(42),
        json!("object?"),
        json!([]),
    ] {
        assert_eq!(
            JsonDocument::try_from_value(scalar).unwrap_err(),
            DocumentError::ObjectRequired
        );
    }
    let mut depth_32 = json!({});
    for _ in 1..32 {
        depth_32 = json!({"next": depth_32});
    }
    assert!(JsonDocument::try_from_value(depth_32.clone()).is_ok());
    assert_eq!(
        JsonDocument::try_from_value(json!({"next": depth_32})).unwrap_err(),
        DocumentError::TooDeep { max_depth: 32 }
    );
    assert_eq!(
        JsonDocument::try_from_value(json!({"data": "x".repeat(256 * 1024)})).unwrap_err(),
        DocumentError::TooLarge {
            max_bytes: 256 * 1024
        }
    );
    assert!(
        JsonDocument::try_from_value(json!({
            "x": "x".repeat(256 * 1024 - 8)
        }))
        .is_ok()
    );
    assert_eq!(
        JsonDocument::try_from_value(json!({"x": "x".repeat(256 * 1024 - 7)})).unwrap_err(),
        DocumentError::TooLarge {
            max_bytes: 256 * 1024
        }
    );
}

#[tokio::test]
async fn records_http_crud_batch_views_schema_and_restart() {
    assert_eq!(TEMPLATE_ID, "records-v1");
    let dir = tempfile::tempdir().unwrap();
    let first = json!({"title": "moss 🌿", "nested": {"tags": ["a", "b"]}});
    let second = json!({"title": "lichen", "empty": {}});
    {
        let router = records_router(dir.path()).unwrap();
        let (status, body) = send(&router, "PUT", "/docs/123", Some(first.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["replaced"], false);
        let (_, body) = send(&router, "GET", "/docs/123", None).await;
        assert_eq!(body["data"], first);
        for key in ["%22abc%22", "true", "null"] {
            let value = json!({"literal_key": key});
            assert_eq!(
                send(&router, "PUT", &format!("/docs/{key}"), Some(value.clone()))
                    .await
                    .0,
                StatusCode::OK
            );
            assert_eq!(
                send(&router, "GET", &format!("/docs/{key}"), None).await.1["data"],
                value
            );
            assert_eq!(
                send(&router, "GET", &format!("/views/docs/{key}"), None)
                    .await
                    .1["data"]["key"],
                percent_decode_for_assertion(key)
            );
        }
        let (_, body) = send(&router, "PUT", "/docs/123", Some(second.clone())).await;
        assert_eq!(body["replaced"], true);
        let (_, body) = send(&router, "GET", "/views/total", None).await;
        assert_eq!(body["data"]["value"], 4);
        let batch = json!([
            {"op": "upsert", "key": "a", "data": {"n": 1}},
            {"op": "upsert", "key": "b", "data": {"n": 2}},
            {"op": "remove", "key": "a"}
        ]);
        let (status, body) = send(&router, "POST", "/batch", Some(batch)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["applied"], 3);
        let (_, body) = send(&router, "GET", "/views/total", None).await;
        assert_eq!(body["data"]["value"], 5);
        let (_, schema) = send(&router, "GET", "/schema", None).await;
        assert_eq!(schema["input"]["type"], "object");
        let names: Vec<_> = schema["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|view| view["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["docs", "total"]);
    }
    let router = records_router(dir.path()).unwrap();
    let (status, body) = send(&router, "GET", "/docs/123", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], second);
    let (_, body) = send(&router, "DELETE", "/docs/123", None).await;
    assert_eq!(body["removed"], true);
    let (_, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(body["data"]["value"], 4);
}

#[tokio::test]
async fn rejected_batches_are_atomic_and_limits_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let router = records_router(dir.path()).unwrap();
    send(&router, "PUT", "/docs/kept", Some(json!({"v": 1}))).await;
    let invalid = json!([
        {"op": "upsert", "key": "new", "data": {"v": 2}},
        {"op": "upsert", "key": "bad/key", "data": {"v": 3}}
    ]);
    let (status, _) = send(&router, "POST", "/batch", Some(invalid)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        send(&router, "GET", "/docs/new", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&router, "GET", "/views/total", None).await.1["data"]["value"],
        1
    );
    let invalid_document = json!([
        {"op": "upsert", "key": "also-new", "data": {"v": 4}},
        {"op": "upsert", "key": "scalar", "data": 5}
    ]);
    assert_eq!(
        send(&router, "POST", "/batch", Some(invalid_document))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&router, "GET", "/docs/also-new", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&router, "GET", "/views/total", None).await.1["data"]["value"],
        1
    );
    for key in ["slash%2Fkey", "line%0Abreak", &"x".repeat(257)] {
        let (status, _) = send(&router, "PUT", &format!("/docs/{key}"), Some(json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "key {key:?}");
    }
    let too_many: Vec<_> = (0..101)
        .map(|n| json!({"op": "upsert", "key": format!("k{n}"), "data": {}}))
        .collect();
    assert_eq!(
        send(&router, "POST", "/batch", Some(Value::Array(too_many)))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    for path in [
        "/views/docs?limit=1001",
        "/views/docs?offset=10001",
        "/views/docs?%6cimit=1001",
    ] {
        assert_eq!(
            send(&router, "GET", path, None).await.0,
            StatusCode::BAD_REQUEST,
            "path {path}"
        );
    }
    let huge = json!({"data": "x".repeat(1024 * 1024)});
    assert_eq!(
        send(&router, "PUT", "/docs/huge", Some(huge)).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[test]
fn a_locked_store_returns_a_typed_startup_error() {
    let dir = tempfile::tempdir().unwrap();
    let router = records_router(dir.path()).unwrap();
    let error = records_router(dir.path()).unwrap_err();
    assert!(error.to_string().contains("open records store"));
    drop(router);
}

#[test]
fn schema_mismatch_returns_a_typed_startup_error() {
    let dir = tempfile::tempdir().unwrap();
    drop(records_router(dir.path()).unwrap());
    let mut marker = dir.path().as_os_str().to_owned();
    marker.push(".schema");
    std::fs::write(std::path::PathBuf::from(marker), "wrong-fingerprint").unwrap();
    assert!(records_router(dir.path()).is_err());
}

fn percent_decode_for_assertion(key: &str) -> String {
    key.replace("%22", "\"")
}

#[tokio::test]
async fn large_view_is_rejected_before_materializing_an_unbounded_page() {
    let dir = tempfile::tempdir().unwrap();
    let router = records_router(dir.path()).unwrap();
    for n in 0..20 {
        assert_eq!(
            send(
                &router,
                "PUT",
                &format!("/docs/{n}"),
                Some(json!({"x":"x".repeat(240*1024)}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    assert_eq!(
        send(&router, "GET", "/views/docs?limit=1000", None).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        send(&router, "GET", "/views/docs?limit=1", None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn logical_quota_boundary_batch_rollback_and_restart() {
    use bog_cloud_records::records_service_with_limit;
    let dir = tempfile::tempdir().unwrap();
    {
        let service = records_service_with_limit(dir.path(), 10).unwrap();
        let app = service.router();
        // JSON key "a" = 3 bytes, value {"n":1} = 7 bytes.
        assert_eq!(
            send(&app, "PUT", "/docs/a", Some(json!({"n":1}))).await.0,
            200
        );
        assert_eq!(
            send(&app, "PUT", "/docs/a", Some(json!({"n":12}))).await.0,
            413
        );
        assert_eq!(
            send(&app, "GET", "/docs/a", None).await.1["data"],
            json!({"n":1})
        );
        assert_eq!(
            send(
                &app,
                "POST",
                "/batch",
                Some(json!([
                    {"op":"remove","key":"a"}, {"op":"upsert","key":"b","data":{"n":12}}
                ]))
            )
            .await
            .0,
            413
        );
        assert_eq!(send(&app, "GET", "/docs/a", None).await.0, 200);
        // Only final state counts, including repeated keys and temporary excess.
        assert_eq!(
            send(
                &app,
                "POST",
                "/batch",
                Some(json!([
                    {"op":"upsert","key":"a","data":{"n":123456}},
                    {"op":"upsert","key":"a","data":{}},
                    {"op":"upsert","key":"b","data":{}}
                ]))
            )
            .await
            .0,
            200
        );
        assert_eq!(
            send(&app, "GET", "/_cloud/usage", None).await.1["data"]["logical_bytes"],
            10
        );
        service.shutdown().unwrap();
    }
    {
        let service = records_service_with_limit(dir.path(), 4).unwrap();
        let app = service.router();
        assert_eq!(
            send(&app, "GET", "/_cloud/usage", None).await.1["data"]["logical_bytes"],
            10
        );
        assert_eq!(send(&app, "GET", "/docs/a", None).await.0, 200);
        assert_eq!(send(&app, "PUT", "/docs/c", Some(json!({}))).await.0, 413);
        assert_eq!(send(&app, "DELETE", "/docs/a", None).await.0, 200);
        assert_eq!(
            send(&app, "GET", "/_cloud/usage", None).await.1["data"]["logical_bytes"],
            5
        );
        assert_eq!(send(&app, "PUT", "/docs/b", Some(json!({}))).await.0, 200);
        assert_eq!(
            send(&app, "PUT", "/docs/b", Some(json!({"n":1}))).await.0,
            413
        );
        assert_eq!(send(&app, "DELETE", "/docs/b", None).await.0, 200);
        assert_eq!(
            send(&app, "GET", "/_cloud/usage", None).await.1["data"]["logical_bytes"],
            0
        );
        service.shutdown().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quota_concurrent_writers_cannot_oversubscribe() {
    let dir = tempfile::tempdir().unwrap();
    let service = bog_cloud_records::records_service_with_limit(dir.path(), 5).unwrap();
    let app = service.router();
    let mut tasks = Vec::new();
    for key in ['a', 'b', 'c', 'd'] {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            send(&app, "PUT", &format!("/docs/{key}"), Some(json!({})))
                .await
                .0
        }));
    }
    let mut accepted = 0;
    for task in tasks {
        match task.await.unwrap().as_u16() {
            200 => accepted += 1,
            413 => (),
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!(accepted, 1);
    assert_eq!(
        send(&app, "GET", "/_cloud/usage", None).await.1["data"]["logical_bytes"],
        5
    );
    service.shutdown().unwrap();
}

#[tokio::test]
async fn custom_transactions_obey_quota_and_failed_handler_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        bog_serve::KeyedApp::<String, String, _>::stream(dir.path(), terminal::Table::new("docs"))
            .post("/custom", |tx, value: String| {
                tx.upsert(&"a".to_string(), &value);
                if value == "fail" {
                    return Err((400, "rejected".into()));
                }
                Ok(true)
            })
            .logical_quota(3, |tx| {
                tx.rtx(|docs| docs.iter().map(|(_, value)| value.len() as u64).sum())
            })
            .durability(bog_serve::Durability::CheckpointBeforeAck)
            .try_into_service()
            .unwrap();
    let app = service.router();
    assert_eq!(
        send(&app, "POST", "/custom", Some(json!("abc"))).await.0,
        200
    );
    assert_eq!(
        send(&app, "POST", "/custom", Some(json!("abcd"))).await.0,
        413
    );
    assert_eq!(
        send(&app, "POST", "/custom", Some(json!("fail"))).await.0,
        400
    );
    assert_eq!(send(&app, "GET", "/docs/a", None).await.1["data"], "abc");
    service.shutdown().unwrap();
}

#[tokio::test]
async fn legacy_bounded_reads_are_ordered_and_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let router = records_router(dir.path()).unwrap();
    for key in ["z", "aa", "b"] {
        assert_eq!(
            send(
                &router,
                "PUT",
                &format!("/docs/{key}"),
                Some(json!({"title":key}))
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let args = serde_urlencoded::to_string([(
        "args",
        json!({"keys":["b","missing","aa","b"]}).to_string(),
    )])
    .unwrap();
    let (status, body) = send(&router, "GET", &format!("/_cloud/batch-get?{args}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"],
        json!([{"key":"b","value":{"title":"b"}},{"key":"missing","value":null},{"key":"aa","value":{"title":"aa"}},{"key":"b","value":{"title":"b"}}])
    );
    let (_, body) = send(&router, "GET", "/views/docs?after=a&before=z&limit=1", None).await;
    assert_eq!(body["data"][0]["key"], "aa");
    assert_eq!(
        send(&router, "GET", "/views/docs?after=a&offset=0", None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let args =
        serde_urlencoded::to_string([("args", json!({"keys":vec!["a";101]}).to_string())]).unwrap();
    assert_eq!(
        send(&router, "GET", &format!("/_cloud/batch-get?{args}"), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}
