use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use bog_cloud::{CloudService, Scope, build_rest_router, config::Config};
use serde_json::{Value, json};
use tower::ServiceExt;
const OWNER: &str = "test-owner-secret-at-least-thirty-two-bytes";
async fn request(
    router: &axum::Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        r = r.header("authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(
            r.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}
#[tokio::test]
async fn auth_scope_errors_and_management_are_consistent() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(tmp.path().join("service"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let a = svc.registry.create("a", "records-v1", "a").unwrap();
    let b = svc.registry.create("b", "records-v1", "b").unwrap();
    let reader = svc.auth.issue(&owner, a.id, Scope::Read).unwrap();
    let router = build_rest_router(svc.clone());
    for (path, content_type) in [
        ("/", "text/html; charset=utf-8"),
        ("/guide.css", "text/css; charset=utf-8"),
        ("/guide.js", "text/javascript; charset=utf-8"),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], content_type);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains(OWNER));
    }
    let (status, body) = request(&router, "GET", "/v1/bogs", None, None).await;
    assert_eq!(status, 401);
    assert!(body["request_id"].is_string());
    assert_eq!(
        request(&router, "GET", "/v1/bogs", Some(OWNER), None)
            .await
            .1["bogs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        request(&router, "GET", "/v1/bogs", Some(&reader.secret), None)
            .await
            .0,
        403
    );
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/v1/bogs/{}", b.id),
            Some(&reader.secret),
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        request(
            &router,
            "PUT",
            &format!("/v1/bogs/{}/docs/k", a.id),
            Some(&reader.secret),
            Some(json!({"x":1}))
        )
        .await
        .0,
        403
    );
    assert_eq!(
        request(&router, "GET", "/v1/bogs/not-uuid", Some(OWNER), None)
            .await
            .0,
        400
    );
    let (status, issued) = request(
        &router,
        "POST",
        &format!("/v1/bogs/{}/tokens", a.id),
        Some(OWNER),
        Some(json!({"scope":"write"})),
    )
    .await;
    assert_eq!(status, 200);
    assert!(issued["token"].is_string());
    let id = issued["id"].as_str().unwrap();
    assert_eq!(
        request(
            &router,
            "DELETE",
            &format!("/v1/bogs/{}/tokens/{id}", a.id),
            Some(OWNER),
            None
        )
        .await
        .0,
        204
    );
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/v1/bogs/{}", a.id),
            Some(issued["token"].as_str().unwrap()),
            None
        )
        .await
        .0,
        401
    );
}

#[tokio::test]
async fn legacy_owner_management_repair() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(tmp.path().join("service"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let bog = svc
        .registry
        .create("repair", "records-v1", "repair")
        .unwrap();
    let token = svc.auth.issue(&owner, bog.id, Scope::Write).unwrap();
    let router = build_rest_router(svc.clone());
    let me = request(&router, "GET", "/v1/me", Some(OWNER), None).await;
    assert_eq!(me.1["kind"], "operator");
    assert_eq!(me.1["workspace_id"], "00000000-0000-0000-0000-000000000001");
    assert_eq!(
        request(&router, "GET", "/v1/workspaces", Some(OWNER), None)
            .await
            .0,
        200
    );
    let path = format!("/v1/bogs/{}", bog.id);
    assert_eq!(
        request(&router, "GET", &format!("{path}/tokens"), Some(OWNER), None)
            .await
            .0,
        200
    );
    assert_eq!(
        request(
            &router,
            "DELETE",
            &path,
            Some(&token.secret),
            Some(json!({"confirm":"wrong"}))
        )
        .await
        .0,
        403
    );
    assert_eq!(
        request(
            &router,
            "DELETE",
            &path,
            Some(OWNER),
            Some(json!({"confirm":bog.id}))
        )
        .await
        .0,
        202
    );
    assert!(svc.auth.authenticate(&token.secret).is_err());
}

#[tokio::test]
async fn repair_creation_diagnostics_and_discovery() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(tmp.path().join("svc"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let router = build_rest_router(svc);
    let result = request(
        &router,
        "POST",
        "/v1/bogs",
        Some(OWNER),
        Some(json!({"name":"x","template":"nope","colour":"red"})),
    )
    .await;
    let message = result.1["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("colour")
            && message.contains("records-v1")
            && message.contains("Idempotency-Key"),
        "{message}"
    );
    let templates = request(&router, "GET", "/v1/templates", None, None).await.1;
    assert!(templates["templates"].is_array());
    assert!(templates.get("create_example").is_none());
}

#[tokio::test]
async fn legacy_guidance_is_truthful_without_javascript() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(tmp.path().join("svc"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let router = build_rest_router(svc);
    for path in ["/", "/auth.md", "/llms.txt"] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = String::from_utf8(
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            if path == "/llms.txt" {
                body.contains("Operator-only")
            } else {
                body.contains("Public signup, workspace sharing, and invitations are unavailable")
            },
            "{path}"
        );
        assert!(body.contains("privately"), "{path}");
        assert!(!body.contains("Your GitHub, your workspace."), "{path}");
    }
}

#[tokio::test]
async fn legacy_workspace_reports_service_configured_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::new(tmp.path().join("service"), std::env::current_exe().unwrap());
    config.max_active = 17;
    let svc = CloudService::open(config, OWNER).unwrap();
    let router = build_rest_router(svc);
    let (status, body) = request(&router, "GET", "/v1/workspaces", Some(OWNER), None).await;
    assert_eq!(status, 200);
    assert_eq!(body["workspaces"][0]["bog_limit"], 17);
    assert_eq!(body["workspaces"][0]["uncapped_bogs"], false);
}

#[tokio::test]
async fn app_management_forbidden_before_body_validation_and_metrics_are_scoped() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(tmp.path().join("service"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let bog = svc.registry.create("a", "records-v1", "a").unwrap();
    let other = svc.registry.create("b", "records-v1", "b").unwrap();
    let app = svc.auth.issue(&owner, bog.id, Scope::Write).unwrap();
    let router = build_rest_router(svc.clone());
    for path in [
        "/v1/bogs".into(),
        format!("/v1/bogs/{}/tokens", bog.id),
        format!("/v1/bogs/{}/app-access", bog.id),
        format!("/v1/bogs/{}/definition/plan", bog.id),
        format!("/v1/bogs/{}/definition/apply", bog.id),
    ] {
        assert_eq!(
            request(&router, "POST", &path, Some(&app.secret), None)
                .await
                .0,
            403
        );
    }
    let (status, metrics) = request(
        &router,
        "GET",
        &format!("/v1/bogs/{}/metrics", bog.id),
        Some(&app.secret),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(metrics["worker"].is_object());
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/v1/bogs/{}/metrics", other.id),
            Some(&app.secret),
            None
        )
        .await
        .0,
        404
    );
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("/v1/bogs/{}/events", bog.id),
            Some(&app.secret),
            None
        )
        .await
        .0,
        403
    );
    svc.auth.revoke(&owner, bog.id, &app.id).unwrap();
    svc.auth.revoke(&owner, bog.id, &app.id).unwrap();
    let (_, events) = request(
        &router,
        "GET",
        &format!("/v1/bogs/{}/events", bog.id),
        Some(OWNER),
        None,
    )
    .await;
    assert_eq!(events.to_string().matches("credential_revoked").count(), 1);
}

#[tokio::test]
async fn http_resource_errors_include_shared_recovery_hint() {
    let response = bog_cloud::http::error_response(
        bog_cloud::CloudError::new("not_found", "resource operation is not exposed"),
        "fixture-request",
    );
    assert_eq!(response.status(), 404);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 10000).await.unwrap()).unwrap();
    assert_eq!(body["request_id"], "fixture-request");
    assert!(
        body["error"]["next_action"]
            .as_str()
            .unwrap()
            .contains("list_resources")
    );
}
