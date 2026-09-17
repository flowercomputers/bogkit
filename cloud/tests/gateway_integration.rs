use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::Request,
    routing::get,
};
use bog_cloud::{
    CloudService,
    browser_auth::BrowserAuth,
    config::Config,
    gateway::PublicAuth,
    oauth::{WorkOsConfig, WorkOsVerifier},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
fn jwt(issuer: &str, sub: &str) -> String {
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some("one".into());
    encode(&h,&json!({"iss":issuer,"aud":"resource","client_id":"browser","sub":sub,"exp":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()+600}),&EncodingKey::from_rsa_pem(include_bytes!("fixtures/oauth-test-key.pem")).unwrap()).unwrap()
}
async fn call(app: &Router, method: &str, path: &str, token: &str, body: Value) -> (u16, Value) {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", "create")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status().as_u16();
    let b = to_bytes(r.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&b).unwrap_or(Value::Null))
}
#[tokio::test]
async fn authenticated_workspace_isolation_and_app_limits() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/oauth2/jwks",
                get(|| async {
                    Json(
                        serde_json::from_str::<Value>(include_str!(
                            "fixtures/oauth-test-jwks.json"
                        ))
                        .unwrap(),
                    )
                }),
            ),
        )
        .await
        .unwrap()
    });
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let root = temp.path().join("cloud");
    let mut svc = CloudService::open(
        Config::new(
            root.clone(),
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker"),
        ),
        "owner-credential-is-at-least-32-bytes",
    )
    .unwrap();
    let verifier = WorkOsVerifier::new(
        WorkOsConfig::for_loopback_testing(
            &issuer,
            "http://127.0.0.1/mcp",
            "resource",
            "browser",
            "secret",
            "http://127.0.0.1/auth/callback",
        )
        .unwrap(),
    )
    .unwrap();
    let browser = BrowserAuth::new(verifier.clone(), &root.join("sessions")).unwrap();
    Arc::get_mut(&mut svc).unwrap().public_auth = Some(PublicAuth { verifier, browser });
    let app = bog_cloud::build_rest_router(svc.clone());
    let a = jwt(&issuer, "a");
    let b = jwt(&issuer, "b");
    assert_eq!(
        call(
            &app,
            "GET",
            "/v1/bogs",
            "owner-credential-is-at-least-32-bytes",
            json!({})
        )
        .await
        .0,
        401
    );
    assert_eq!(
        call(&app, "GET", "/v1/bogs", "not.a.jwt", json!({}))
            .await
            .0,
        401
    );
    let (status, created) = call(&app, "POST", "/v1/bogs", &a, json!({"name":"a"})).await;
    assert_eq!(status, 202, "{created}");
    let id = created["id"].as_str().unwrap();
    assert_eq!(
        call(&app, "GET", &format!("/v1/bogs/{id}"), &b, json!({}))
            .await
            .0,
        404
    );
    let (_, listed) = call(&app, "GET", "/v1/bogs", &b, json!({})).await;
    assert_eq!(listed["bogs"].as_array().unwrap().len(), 0);
    let (_, workspaces) = call(&app, "GET", "/v1/workspaces", &a, json!({})).await;
    let workspace = workspaces["workspaces"][0]["id"].as_str().unwrap();
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/v1/bogs?workspace_id={workspace}"),
            &b,
            json!({})
        )
        .await
        .0,
        403
    );
    let (_, issued) = call(
        &app,
        "POST",
        &format!("/v1/bogs/{id}/tokens"),
        &a,
        json!({"scope":"read"}),
    )
    .await;
    let token = issued["token"].as_str().unwrap();
    assert_eq!(
        call(&app, "GET", &format!("/v1/bogs/{id}"), token, json!({}))
            .await
            .0,
        200
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("/v1/bogs/{id}/tokens"),
            token,
            json!({"scope":"write"})
        )
        .await
        .0,
        403
    );
    assert_eq!(call(&app, "GET", "/v1/bogs", token, json!({})).await.0, 403);
    let (_, tokens) = call(&app, "GET", &format!("/v1/bogs/{id}/tokens"), &a, json!({})).await;
    assert!(!tokens.to_string().contains(token));
    // Join A's workspace as an owner using a verified browser identity. B's
    // personal default remains unchanged; every shared operation is explicit.
    let verifier = &svc.public_auth.as_ref().unwrap().verifier;
    let identity_a = verifier.verify_access_token(&a).await.unwrap();
    let identity_b = verifier.verify_access_token(&b).await.unwrap();
    let workspace_id = bog_cloud::WorkspaceId(uuid::Uuid::parse_str(workspace).unwrap());
    let human_a = svc
        .auth
        .principal_from_verified(&identity_a, workspace_id)
        .unwrap();
    let invitation = svc.auth.invite(&human_a, "owner").unwrap();
    svc.auth
        .accept_invitation(&identity_b, &invitation.secret)
        .unwrap();
    let (_, personal) = call(&app, "GET", "/v1/bogs", &b, json!({})).await;
    assert_eq!(personal["bogs"].as_array().unwrap().len(), 0);
    let (_, shared) = call(
        &app,
        "GET",
        &format!("/v1/bogs?workspace_id={workspace}"),
        &b,
        json!({}),
    )
    .await;
    assert_eq!(shared["bogs"][0]["id"], id);
    for suffix in ["schema", "usage"] {
        let (status, body) = call(
            &app,
            "GET",
            &format!("/v1/bogs/{id}/{suffix}?workspace_id={workspace}"),
            &b,
            json!({}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/v1/bogs/{id}"),
            &b,
            json!({"confirm":id})
        )
        .await
        .0,
        404
    );
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/v1/bogs/{id}?workspace_id={workspace}"),
            &b,
            json!({"confirm":id})
        )
        .await
        .0,
        202
    );
    assert_eq!(
        call(&app, "GET", &format!("/v1/bogs/{id}"), token, json!({}))
            .await
            .0,
        401
    );
    svc.supervisor.shutdown().await.unwrap();
    task.abort();
}
#[tokio::test]
async fn browser_session_rest_requires_origin_and_csrf() {
    use axum::routing::post;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let nonce = Arc::new(std::sync::Mutex::new(String::new()));
    let n = nonce.clone();
    let iss = issuer.clone();
    let provider=Router::new().route("/oauth2/jwks",get(||async{Json(serde_json::from_str::<Value>(include_str!("fixtures/oauth-test-jwks.json")).unwrap())})).route("/oauth2/token",post(move ||{let n=n.clone();let iss=iss.clone();async move{let mut h=Header::new(Algorithm::RS256);h.kid=Some("one".into());let id=encode(&h,&json!({"iss":iss,"aud":"browser","sub":"human","nonce":n.lock().unwrap().clone(),"exp":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()+600}),&EncodingKey::from_rsa_pem(include_bytes!("fixtures/oauth-test-key.pem")).unwrap()).unwrap();Json(json!({"access_token":jwt(&iss,"human"),"id_token":id,"token_type":"bearer"}))}}));
    let task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("cloud");
    let mut svc = CloudService::open(
        Config::new(root.clone(), "/bin/false".into()),
        "owner-credential-is-at-least-32-bytes",
    )
    .unwrap();
    let verifier = WorkOsVerifier::new(
        WorkOsConfig::for_loopback_testing(
            &issuer,
            "http://127.0.0.1/mcp",
            "resource",
            "browser",
            "secret",
            "http://127.0.0.1/auth/callback",
        )
        .unwrap(),
    )
    .unwrap();
    let browser = BrowserAuth::new(verifier.clone(), &root.join("sessions")).unwrap();
    Arc::get_mut(&mut svc).unwrap().public_auth = Some(PublicAuth { verifier, browser });
    let app = bog_cloud::build_rest_router(svc.clone());
    let login = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/auth/login")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), 303);
    let url = reqwest::Url::parse(login.headers()["location"].to_str().unwrap()).unwrap();
    let params = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    *nonce.lock().unwrap() = params["nonce"].to_string();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let callback = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/auth/callback?state={}&code=fixture",
                    params["state"]
                ))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(callback.status(), 303);
    assert_eq!(callback.headers().get_all("set-cookie").iter().count(), 2);
    let session = callback.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let info = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console-session")
                .header("cookie", session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(info.status(), 200);
    let info: Value =
        serde_json::from_slice(&to_bytes(info.into_body(), 65536).await.unwrap()).unwrap();
    assert!(info.get("access_token").is_none());
    let csrf = info["csrf_token"].as_str().unwrap();
    let w = info["workspaces"][0]["id"].as_str().unwrap();
    for (origin, token, expected) in [
        ("", "", 401),
        ("http://evil.test", csrf, 401),
        ("http://127.0.0.1", "bad", 401),
        ("http://127.0.0.1", csrf, 200),
    ] {
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/workspaces/{w}/invitations"))
                    .header("cookie", session)
                    .header("origin", origin)
                    .header("x-csrf-token", token)
                    .header("content-type", "application/json")
                    .body(Body::from("{\"role\":\"member\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), expected);
    }
    let logout = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/logout")
                .header("cookie", session)
                .header("origin", "http://127.0.0.1")
                .header("x-csrf-token", csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), 200);
    let info = app
        .oneshot(
            Request::builder()
                .uri("/console-session")
                .header("cookie", session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(info.status(), 401);
    task.abort();
}
