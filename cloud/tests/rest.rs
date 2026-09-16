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
