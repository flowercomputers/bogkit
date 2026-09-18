//! One shared todo fixture exercises real HTTP routing and worker persistence.
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use bog_cloud::{CloudService, Scope, build_rest_router, config::Config};
use serde_json::{Value, json};
use tower::ServiceExt;
const OWNER: &str = "composable-acceptance-owner-secret-over-32-bytes";
fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/examples/composable")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn config(root: std::path::PathBuf) -> Config {
    let worker = std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker")
        });
    assert!(
        worker.exists(),
        "build bog-records-worker before process acceptance tests"
    );
    let mut config = Config::new(root, worker);
    config.composable_enabled = true;
    config
}
async fn request(
    router: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (u16, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", "todo-acceptance")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}
async fn ok(router: &axum::Router, method: &str, path: &str, token: &str, body: Value) -> Value {
    let (status, result) = request(router, method, path, token, body).await;
    assert!(
        (200..300).contains(&status),
        "{method} {path}: {status}: {result}"
    );
    result
}
async fn query(router: &axum::Router, base: &str, resource: &str, token: &str) -> Value {
    ok(
        router,
        "POST",
        &format!("{base}/resources/{resource}/query"),
        token,
        json!({}),
    )
    .await
}
#[test]
fn documented_definitions_are_valid_and_additive() {
    let base: bog_definition::Definition = serde_json::from_value(fixture("todo.json")).unwrap();
    let text: bog_definition::Definition =
        serde_json::from_value(fixture("todo-search.json")).unwrap();
    let semantic: bog_definition::Definition =
        serde_json::from_value(fixture("todo-semantic.json")).unwrap();
    for definition in [&base, &text, &semantic] {
        definition.validate().unwrap();
    }
    assert_eq!(
        base.validate_additive(&text).unwrap().added_resources,
        vec!["text_search"]
    );
    assert_eq!(
        text.validate_additive(&semantic).unwrap().added_resources,
        vec!["semantic_search"]
    );
    assert!(
        base.expose
            .values()
            .all(|operation| operation.target != "private_stats")
    );
}
#[tokio::test]
async fn todo_http_mutation_additive_search_restart_and_backup() {
    let temp = tempfile::Builder::new()
        .prefix("bca-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temp.path().join("r");
    let service = CloudService::open(config(root.clone()), OWNER).unwrap();
    let router = build_rest_router(service.clone());
    let catalog = ok(&router, "GET", "/v1/components", OWNER, Value::Null).await;
    assert_eq!(catalog["enabled"], true);
    let validated = ok(
        &router,
        "POST",
        "/v1/definitions/validate",
        OWNER,
        json!({"definition":fixture("todo.json")}),
    )
    .await;
    assert_eq!(validated["valid"], true);
    let mut malformed = fixture("todo.json");
    malformed["resources"]["priority"]["terminal"]["field"] = json!("priority");
    assert_eq!(
        request(
            &router,
            "POST",
            "/v1/definitions/validate",
            OWNER,
            malformed
        )
        .await
        .0,
        400
    );
    let created = ok(
        &router,
        "POST",
        "/v1/bogs",
        OWNER,
        json!({"name":"todo","definition":fixture("todo.json")}),
    )
    .await;
    let id = created["id"].as_str().expect("created Bog id");
    let base = format!("/v1/bogs/{id}");
    let bog_id = serde_json::from_value(json!(id)).unwrap();
    let owner = service.auth.authenticate(OWNER).unwrap();
    let reader = service.auth.issue(&owner, bog_id, Scope::Read).unwrap();
    ok(
        &router,
        "POST",
        &format!("{base}/batch"),
        OWNER,
        fixture("todo-records.json"),
    )
    .await;
    let count = query(&router, &base, "open_count", &reader.secret).await;
    assert_eq!(count["data"], json!(2), "{count}");
    let open = query(&router, &base, "open", &reader.secret).await;
    assert!(open.to_string().contains("Ship release"));
    assert!(!open.to_string().contains("Verify backup"));
    assert!(
        !open.to_string().contains("Publish release notes"),
        "projection must omit notes"
    );
    let ranked = query(&router, &base, "priority", &reader.secret).await;
    assert_eq!(ranked["data"][0]["key"], "release");
    assert_eq!(ranked["data"][0]["score"], json!(9.0));
    let mut local = bog_runtime::Runtime::open(
        temp.path().join("local"),
        serde_json::from_value(fixture("todo.json")).unwrap(),
    )
    .unwrap();
    local
        .mutate(
            &serde_json::from_value::<Vec<bog_runtime::Mutation>>(fixture("todo-records.json"))
                .unwrap(),
        )
        .unwrap();
    for (resource, http) in [
        ("open", &open),
        ("open_count", &count),
        ("priority", &ranked),
    ] {
        assert_eq!(
            local.execute(resource, json!({})).unwrap(),
            http["data"],
            "local/HTTP mismatch for {resource}"
        );
    }
    assert!(local.execute("private_stats", json!({})).is_err());
    let listed = ok(
        &router,
        "GET",
        &format!("{base}/resources"),
        &reader.secret,
        Value::Null,
    )
    .await;
    assert!(
        !listed.to_string().contains("private_stats"),
        "private resource leaked: {listed}"
    );
    let (status, _) = request(
        &router,
        "POST",
        &format!("{base}/resources/private_stats/query"),
        OWNER,
        json!({}),
    )
    .await;
    assert_eq!(
        status, 404,
        "unexposed resource is inaccessible even to owner"
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("{base}/docs/forbidden"),
            &reader.secret,
            json!({})
        )
        .await
        .0,
        403
    );
    // A type error must not partially replace the source or any derived output.
    let (invalid_status, _) = request(
        &router,
        "PUT",
        &format!("{base}/docs/release"),
        OWNER,
        json!({"title":"Broken replacement","completed":false,"priority":"high"}),
    )
    .await;
    assert!(invalid_status >= 400);
    assert_eq!(
        query(&router, &base, "priority", OWNER).await["data"],
        ranked["data"]
    );
    let source = ok(
        &router,
        "GET",
        &format!("{base}/docs/release"),
        OWNER,
        Value::Null,
    )
    .await;
    assert_eq!(source["data"]["title"], "Ship release");
    let before = ok(
        &router,
        "GET",
        &format!("{base}/definition"),
        OWNER,
        Value::Null,
    )
    .await;
    let update =
        json!({"definition":fixture("todo-semantic.json"),"expected_revision":before["revision"]});
    assert_eq!(
        request(
            &router,
            "POST",
            &format!("{base}/definition/apply"),
            &reader.secret,
            update.clone()
        )
        .await
        .0,
        403
    );
    ok(
        &router,
        "POST",
        &format!("{base}/definition/plan"),
        OWNER,
        update.clone(),
    )
    .await;
    let (stale_status, _) = request(
        &router,
        "POST",
        &format!("{base}/definition/apply"),
        OWNER,
        json!({"definition":fixture("todo-semantic.json"),"expected_revision":0}),
    )
    .await;
    assert_eq!(stale_status, 409);
    let job = ok(
        &router,
        "POST",
        &format!("{base}/definition/apply"),
        OWNER,
        update,
    )
    .await;
    let job_id = job["job_id"].as_str().expect("persistent job id");
    tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            let job = ok(
                &router,
                "GET",
                &format!("{base}/definition/jobs/{job_id}"),
                OWNER,
                Value::Null,
            )
            .await;
            if job["status"] == "succeeded"
                || job["status"] == "completed"
                || job["status"] == "active"
            {
                break;
            }
            assert_ne!(job["status"], "failed", "{job}");
            // Previously active reads remain available throughout a build.
            assert_eq!(
                query(&router, &base, "open_count", &reader.secret).await["data"],
                json!(2)
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("definition build finishes");
    let search = ok(
        &router,
        "POST",
        &format!("{base}/resources/text_search/search"),
        &reader.secret,
        json!({"query":"changelog","limit":10}),
    )
    .await;
    assert_eq!(
        search["data"][0]["key"], "release",
        "populated source indexed: {search}"
    );
    let semantic = ok(
        &router,
        "POST",
        &format!("{base}/resources/semantic_search/search"),
        &reader.secret,
        json!({"query":"publish software changelog","limit":3}),
    )
    .await;
    assert_eq!(
        request(
            &router,
            "POST",
            &format!("{base}/resources/semantic_search/search"),
            OWNER,
            json!({"vector":[1.0,0.0],"limit":3})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        semantic["data"][0]["key"], "release",
        "semantic meaning must rank release first: {semantic}"
    );
    assert_eq!(
        query(&router, &base, "priority", &reader.secret).await["data"],
        ranked["data"]
    );
    ok(&router,"PUT",&format!("{base}/docs/milk"),OWNER,json!({"title":"Buy oat milk","notes":"Groceries for breakfast","completed":true,"priority":3})).await;
    assert_eq!(
        query(&router, &base, "open_count", OWNER).await["data"],
        json!(1)
    );
    ok(&router,"PUT",&format!("{base}/docs/milk"),OWNER,json!({"title":"Buy oat milk","notes":"Groceries for breakfast","completed":false,"priority":3})).await;
    assert_eq!(
        query(&router, &base, "open_count", OWNER).await["data"],
        json!(2)
    );
    // Replacement must retain a still-present token and replace the vector even
    // when the searchable key itself does not change.
    for title in ["bread bread", "bread"] {
        ok(
            &router,
            "PUT",
            &format!("{base}/docs/rewrite"),
            OWNER,
            json!({"title":title,"notes":"","completed":true,"priority":0}),
        )
        .await;
    }
    let bread = ok(
        &router,
        "POST",
        &format!("{base}/resources/text_search/search"),
        OWNER,
        json!({"query":"bread","limit":10}),
    )
    .await;
    assert!(
        bread["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["key"] == "rewrite"),
        "decreased token frequency must retain posting: {bread}"
    );
    ok(
        &router,
        "PUT",
        &format!("{base}/docs/rewrite"),
        OWNER,
        json!({"title":"publish software release","notes":"","completed":true,"priority":0}),
    )
    .await;
    let replaced = ok(
        &router,
        "POST",
        &format!("{base}/resources/semantic_search/search"),
        OWNER,
        json!({"query":"publish software release","limit":10}),
    )
    .await;
    let replacement_hit = replaced["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["key"] == "rewrite")
        .expect("replacement vector is searchable");
    assert!(
        replacement_hit["distance"].as_f64().unwrap().abs() < 1e-5,
        "semantic index retained stale replacement: {replaced}"
    );
    let bread = ok(
        &router,
        "POST",
        &format!("{base}/resources/text_search/search"),
        OWNER,
        json!({"query":"bread","limit":10}),
    )
    .await;
    assert!(
        !bread["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["key"] == "rewrite"),
        "old text posting survived replacement: {bread}"
    );
    ok(
        &router,
        "DELETE",
        &format!("{base}/docs/rewrite"),
        OWNER,
        Value::Null,
    )
    .await;
    for (resource, query_text) in [
        ("text_search", "publish software release"),
        ("semantic_search", "publish software release"),
    ] {
        let deleted = ok(
            &router,
            "POST",
            &format!("{base}/resources/{resource}/search"),
            OWNER,
            json!({"query":query_text,"limit":10}),
        )
        .await;
        assert!(
            !deleted["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|hit| hit["key"] == "rewrite"),
            "deleted record remained in {resource}: {deleted}"
        );
    }
    let archive = service.supervisor.backup(bog_id).await.unwrap();
    let restored = service
        .supervisor
        .restore(&archive, "restored-todo")
        .await
        .unwrap();
    let restored_base = format!("/v1/bogs/{}", restored.id);
    assert_eq!(
        query(&router, &restored_base, "open_count", OWNER).await["data"],
        json!(2)
    );
    assert_eq!(
        query(&router, &restored_base, "priority", OWNER).await["data"],
        ranked["data"]
    );
    let restored_search = ok(
        &router,
        "POST",
        &format!("{restored_base}/resources/semantic_search/search"),
        OWNER,
        json!({"query":"publish software changelog","limit":3}),
    )
    .await;
    assert_eq!(restored_search["data"], semantic["data"]);
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("{restored_base}/definition"),
            &reader.secret,
            Value::Null
        )
        .await
        .0,
        404,
        "original credential must not access restored Bog"
    );
    service.supervisor.shutdown().await.unwrap();
    drop(router);
    drop(service);
    let service = CloudService::open(config(root), OWNER).unwrap();
    assert!(service.supervisor.reconcile().await.unwrap().is_empty());
    let router = build_rest_router(service.clone());
    assert_eq!(
        query(&router, &base, "open_count", &reader.secret).await["data"],
        json!(2)
    );
    let after = ok(
        &router,
        "POST",
        &format!("{base}/resources/text_search/search"),
        &reader.secret,
        json!({"query":"changelog","limit":10}),
    )
    .await;
    assert_eq!(after["data"], search["data"]);
    let semantic_after = ok(
        &router,
        "POST",
        &format!("{base}/resources/semantic_search/search"),
        &reader.secret,
        json!({"query":"publish software changelog","limit":3}),
    )
    .await;
    assert_eq!(semantic_after["data"], semantic["data"]);
    service.supervisor.shutdown().await.unwrap();
}

#[test]
fn local_todo_search_matches_expected_records_and_reopens() {
    let temp = tempfile::tempdir().unwrap();
    let definition: bog_definition::Definition =
        serde_json::from_value(fixture("todo-semantic.json")).unwrap();
    let mut runtime =
        bog_runtime::Runtime::open(temp.path().join("data"), definition.clone()).unwrap();
    let operations: Vec<bog_runtime::Mutation> =
        serde_json::from_value(fixture("todo-records.json")).unwrap();
    runtime.mutate(&operations).unwrap();
    assert_eq!(runtime.execute("open_count", json!({})).unwrap(), json!(2));
    assert_eq!(
        runtime.execute("priority", json!({})).unwrap()[0]["key"],
        "release"
    );
    let text = runtime
        .execute("search", json!({"query":"changelog"}))
        .unwrap();
    assert_eq!(text[0]["key"], "release");
    let semantic = runtime
        .execute(
            "semantic_search",
            json!({"query":"publish software changelog"}),
        )
        .unwrap();
    assert_eq!(semantic[0]["key"], "release");
    drop(runtime);
    let mut reopened = bog_runtime::Runtime::open(temp.path().join("data"), definition).unwrap();
    assert_eq!(
        reopened
            .execute("search", json!({"query":"changelog"}))
            .unwrap(),
        text
    );
    assert_eq!(
        reopened
            .execute(
                "semantic_search",
                json!({"query":"publish software changelog"})
            )
            .unwrap(),
        semantic
    );
}
