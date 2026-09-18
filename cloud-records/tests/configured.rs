use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use bog_cloud_records::{ConfiguredService, DEFAULT_LOGICAL_BYTES};
use serde_json::{Value, json};
use tower::ServiceExt;
fn definition() -> bog_definition::Definition {
    serde_json::from_value(json!({"resources":{"docs":{"terminal":{"kind":"table"}},"private":{"stages":[{"kind":"filter","expression":{"op":"equals","field":"/secret","value":"private-literal"}}],"terminal":{"kind":"table"}},"total":{"terminal":{"kind":"count"}}},"expose":{"put":{"target":"docs","action":"put"},"list":{"target":"docs","action":"list"},"total":{"target":"total","action":"read"}}})).unwrap()
}
async fn request(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, value)
}
#[tokio::test]
async fn private_resources_are_not_routable_or_discoverable() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ConfiguredService::open(dir.path(), definition(), 1, DEFAULT_LOGICAL_BYTES).unwrap();
    let router = service.router();
    assert_eq!(
        request(
            &router,
            "PUT",
            "/docs/a",
            json!({"secret":"private-literal"})
        )
        .await
        .0,
        StatusCode::OK
    );
    for path in ["/views/private", "/views/private/a", "/operations/private"] {
        assert_eq!(
            request(
                &router,
                if path.starts_with("/operations") {
                    "POST"
                } else {
                    "GET"
                },
                path,
                json!({})
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    for path in ["/schema", "/openapi.json"] {
        let (status, value) = request(&router, "GET", path, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!value.to_string().contains("private"));
    }
    assert_eq!(
        request(&router, "GET", "/docs/a", json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&router, "DELETE", "/docs/a", json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&router, "POST", "/batch", json!([])).await.0,
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn worker_sequence_export_and_usage_agree() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ConfiguredService::open(dir.path(), definition(), 2, DEFAULT_LOGICAL_BYTES).unwrap();
    let router = service.router();
    let (_, write) = request(&router, "PUT", "/docs/a", json!({"title":"bread"})).await;
    let (_, sequence) = request(&router, "GET", "/_cloud/sequence", json!({})).await;
    assert_eq!(write["seq"], sequence["seq"]);
    let (_, export) = request(&router, "GET", "/_cloud/export?offset=0", json!({})).await;
    assert_eq!(export["record_count"], 1);
    let (_, usage) = request(&router, "GET", "/_cloud/usage", json!({})).await;
    assert_eq!(usage["data"]["source_records"], 1);
    assert!(usage["data"]["physical_store_bytes"].as_u64().unwrap() > 0);
    let (_, total) = request(&router, "POST", "/operations/total", json!({})).await;
    assert_eq!(total["data"], 1);
    drop(router);
    service.begin_shutdown();
    service.shutdown().unwrap();
    let service =
        ConfiguredService::open(dir.path(), definition(), 2, DEFAULT_LOGICAL_BYTES).unwrap();
    let (_, total) = request(&service.router(), "POST", "/operations/total", json!({})).await;
    assert_eq!(total["data"], 1);
}
#[tokio::test]
async fn unknown_operation_never_exposes_private_definition() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ConfiguredService::open(dir.path(), definition(), 1, DEFAULT_LOGICAL_BYTES).unwrap();
    assert_eq!(
        request(&service.router(), "POST", "/operations/unknown", json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

fn waiting_definition() -> bog_definition::Definition {
    let mut d = definition();
    d.expose.insert(
        "wait".into(),
        bog_definition::Operation {
            target: "docs".into(),
            action: bog_definition::Action::Wait,
        },
    );
    d
}
#[tokio::test]
async fn exposed_local_wait_initializes_wakes_times_out_and_resets_on_restart() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ConfiguredService::open(dir.path(), waiting_definition(), 1, DEFAULT_LOGICAL_BYTES)
            .unwrap();
    let router = service.router();
    let (status, first) = request(&router, "POST", "/operations/wait", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["data"]["changed"], false);
    assert_eq!(first["data"]["reset"], false);
    let cursor = first["data"]["cursor"].as_str().unwrap().to_owned();
    let waiter = tokio::spawn({
        let router = router.clone();
        let cursor = cursor.clone();
        async move {
            request(
                &router,
                "POST",
                "/operations/wait",
                json!({"cursor":cursor,"timeout_ms":1000}),
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        request(&router, "PUT", "/docs/a", json!({"title":"bread"}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, changed) = waiter.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["data"]["changed"], true);
    assert_eq!(changed["data"]["reset"], false);
    let cursor = changed["data"]["cursor"].as_str().unwrap().to_owned();
    let (status, unchanged) = request(
        &router,
        "POST",
        "/operations/wait",
        json!({"cursor":cursor,"timeout_ms":1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unchanged["data"]["changed"], false);
    assert_eq!(
        request(
            &router,
            "POST",
            "/operations/wait",
            json!({"timeout_ms":30001})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/operations/wait",
            json!({"cursor":"malformed"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    drop(router);
    service.begin_shutdown();
    service.shutdown().unwrap();
    let service =
        ConfiguredService::open(dir.path(), waiting_definition(), 1, DEFAULT_LOGICAL_BYTES)
            .unwrap();
    let (status, reset) = request(
        &service.router(),
        "POST",
        "/operations/wait",
        json!({"cursor":cursor,"timeout_ms":1000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reset["data"]["changed"], true);
    assert_eq!(reset["data"]["reset"], true);
}
#[tokio::test]
async fn local_wait_wakes_on_shutdown_without_holding_store_lock() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ConfiguredService::open(dir.path(), waiting_definition(), 1, DEFAULT_LOGICAL_BYTES)
            .unwrap();
    let router = service.router();
    let (_, first) = request(&router, "POST", "/operations/wait", json!({})).await;
    let cursor = first["data"]["cursor"].clone();
    let waiter = tokio::spawn({
        let router = router.clone();
        async move {
            request(
                &router,
                "POST",
                "/operations/wait",
                json!({"cursor":cursor,"timeout_ms":30000}),
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    service.begin_shutdown();
    let (status, _) = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
#[tokio::test]
async fn metadata_and_usage_advertise_effective_lower_limits() {
    let dir = tempfile::tempdir().unwrap();
    let limits = bog_definition::Limits {
        query_bytes: 8,
        hits: 2,
        text_bytes: 16,
        ..Default::default()
    };
    let service = ConfiguredService::open_with_limits(
        dir.path(),
        definition(),
        1,
        DEFAULT_LOGICAL_BYTES,
        limits,
    )
    .unwrap();
    let router = service.router();
    let (_, schema) = request(&router, "GET", "/schema", json!({})).await;
    assert_eq!(schema["limits"]["query_bytes"], 8);
    let (_, usage) = request(&router, "GET", "/_cloud/usage", json!({})).await;
    assert_eq!(usage["data"]["limits"]["hits"], 2);
}
#[tokio::test]
async fn local_router_hides_operator_definition_and_controls() {
    let dir = tempfile::tempdir().unwrap();
    let service =
        ConfiguredService::open(dir.path(), definition(), 1, DEFAULT_LOGICAL_BYTES).unwrap();
    let router = service.local_router();
    for path in [
        "/definition",
        "/_cloud/export",
        "/_cloud/import",
        "/_cloud/usage",
        "/_cloud/sequence",
    ] {
        assert_eq!(
            request(
                &router,
                if path == "/_cloud/import" {
                    "POST"
                } else {
                    "GET"
                },
                path,
                json!({})
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    let (status, schema) = request(&router, "GET", "/schema", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!schema.to_string().contains("private-literal"));
}

#[tokio::test]
async fn worker_published_contracts_validate_actual_data_and_wait_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let mut definition = waiting_definition();
    definition.resources.insert(
        "text".into(),
        serde_json::from_value(json!({"terminal":{"kind":"bm25","fields":["/title"]}})).unwrap(),
    );
    definition.expose.insert(
        "text".into(),
        bog_definition::Operation {
            target: "text".into(),
            action: bog_definition::Action::Search,
        },
    );
    let service =
        ConfiguredService::open(dir.path(), definition, 1, DEFAULT_LOGICAL_BYTES).unwrap();
    let router = service.router();
    let (status, schema) = request(&router, "GET", "/schema", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    for (name, body) in [
        (
            "put",
            json!({"key":"a","data":{"title":"bread","secret":"private"}}),
        ),
        ("list", json!({})),
        ("total", json!({})),
        ("text", json!({"query":"bread"})),
        ("text", json!({"query":"bread","include_fields":["/title"]})),
        ("wait", json!({"timeout":0})),
    ] {
        let metadata = schema["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|op| op["name"] == name)
            .unwrap();
        assert!(
            jsonschema::validator_for(&metadata["request_schema"])
                .unwrap()
                .is_valid(&body)
        );
        let (status, actual) = request(
            &router,
            "POST",
            &format!("/operations/{name}"),
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name}: {actual}");
        assert!(
            jsonschema::validator_for(&metadata["response_schema"])
                .unwrap()
                .is_valid(&actual["data"]),
            "{name}: {actual}"
        );
        if name == "text" {
            if body.get("include_fields").is_some() {
                assert_eq!(actual["data"][0]["value"], json!({"/title":"bread"}));
            } else {
                assert!(actual["data"][0].get("value").is_none());
            }
        }
    }
    let wait = schema["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["name"] == "wait")
        .unwrap();
    assert!(
        wait["request_schema"]["properties"]
            .get("timeout_ms")
            .is_none()
    );
    assert_eq!(
        request(&router, "POST", "/operations/wait", json!({"timeout_ms":0}))
            .await
            .0,
        StatusCode::OK
    );
    for body in [
        json!({"timeout":null}),
        json!({"timeout":26}),
        json!({"timeout":0,"timeout_ms":0}),
        json!({"timeout":-1}),
        json!({"timeout":0.5}),
        json!({"timeout":0,"extra":true}),
    ] {
        assert_eq!(
            request(&router, "POST", "/operations/wait", body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(
            &router,
            "POST",
            "/operations/put",
            json!({"key":"b","data":{},"extra":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn bounded_reads_use_existing_get_authority_and_cursor_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = definition();
    d.expose.insert(
        "get".into(),
        bog_definition::Operation {
            target: "docs".into(),
            action: bog_definition::Action::Get,
        },
    );
    let service = ConfiguredService::open(dir.path(), d, 1, DEFAULT_LOGICAL_BYTES).unwrap();
    let router = service.router();
    for key in ["a", "b", "c"] {
        assert_eq!(
            request(
                &router,
                "PUT",
                &format!("/docs/{key}"),
                json!({"title":key})
            )
            .await
            .0,
            StatusCode::OK
        );
    }
    let (status, body) = request(
        &router,
        "POST",
        "/operations/get",
        json!({"keys":["b","missing","a"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"],
        json!([{"key":"b","value":{"title":"b"}},{"key":"missing","value":null},{"key":"a","value":{"title":"a"}}])
    );
    let (status, body) = request(&router, "GET", "/views/docs?after=a&before=c", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!([{"key":"b","value":{"title":"b"}}]));
    assert_eq!(
        request(&router, "GET", "/views/docs?after=a&offset=0", json!({}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/operations/get",
            json!({"keys":vec!["a";101]})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            &router,
            "POST",
            "/operations/private",
            json!({"keys":["a"]})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
