use axum::{
    Json, Router,
    routing::{get, post},
};
use bog_cloud::{
    CloudService,
    config::Config,
    native_auth::{GithubConfig, NativeAuth},
};
use serde_json::json;
#[tokio::test]
async fn github_login_uses_pkce_and_immutable_identity_then_local_session() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server=tokio::spawn(axum::serve(listener,Router::new().route("/token",post(||async{Json(json!({"access_token":"provider-secret","token_type":"bearer","scope":""}))})).route("/user",get(||async{Json(json!({"id":42,"login":"rename-does-not-matter"}))}))).into_future());
    let dir = tempfile::tempdir().unwrap();
    let native = NativeAuth::new(
        GithubConfig::for_loopback_testing(&origin).unwrap(),
        &dir.path().join("sessions"),
    )
    .unwrap();
    let login = native.begin_login(None).unwrap();
    let url = reqwest::Url::parse(&login.authorization_url).unwrap();
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["scope"], "");
    assert_eq!(params["code_challenge_method"], "S256");
    let binding = login
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    assert!(
        native
            .complete_login(&params["state"], "code", "wrong")
            .await
            .is_err()
    );
    let session = native
        .complete_login(&params["state"], "code", binding)
        .await
        .unwrap();
    assert_eq!(session.identity.subject(), "42");
    assert_eq!(session.identity.issuer(), "https://github.com");
    assert!(
        native
            .complete_login(&params["state"], "code", binding)
            .await
            .is_err()
    );
    let id = session
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    assert_eq!(native.authenticate(id).unwrap().subject(), "42");
    assert!(
        native
            .check_csrf(id, "https://evil.test", &session.csrf_token)
            .is_err()
    );
    native.logout(id, &origin, &session.csrf_token).unwrap();
    assert!(native.authenticate(id).is_err());
    server.abort();
}
#[test]
fn config_rejects_non_exact_callback_and_partial_or_mixed_provider() {
    assert!(GithubConfig::new("id", "secret", "https://evil.test/auth/callback").is_err());
    assert!(GithubConfig::new("id", "secret", "https://cloud.bog.new/auth/callback").is_ok());
    for bad in [
        "http://cloud.bog.new/auth/callback",
        "https://cloud.bog.new.evil.test/auth/callback",
        "https://cloud.bog.new/auth/callback?x=1",
        "https://mcp.bog.new/auth/callback",
    ] {
        assert!(GithubConfig::new("id", "secret", bad).is_err());
    }
    assert!(
        GithubConfig::new(
            "id",
            "secret",
            "https://flower-bog-cloud.fly.dev/auth/callback"
        )
        .is_ok()
    );
}
use std::future::IntoFuture;
#[tokio::test]
async fn account_tokens_are_human_only_revocable_and_stored_hashed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/token",
                    post(|| async { Json(json!({"access_token":"secret","token_type":"bearer"})) }),
                )
                .route("/user", get(|| async { Json(json!({"id":42})) })),
        )
        .into_future(),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut svc = CloudService::open(
        Config::new(dir.path().join("cloud"), "/tmp/unused-worker".into()),
        "owner-token-long-enough-for-validation",
    )
    .unwrap();
    let native = NativeAuth::new(
        GithubConfig::for_loopback_testing(&origin).unwrap(),
        &dir.path().join("sessions"),
    )
    .unwrap();
    let login = native.begin_login(None).unwrap();
    let url = reqwest::Url::parse(&login.authorization_url).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let binding = login
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    let session = native
        .complete_login(&state, "code", binding)
        .await
        .unwrap();
    let (_, w) = svc.auth.provision_identity(&session.identity).unwrap();
    let human = svc
        .auth
        .principal_from_verified(&session.identity, w.id)
        .unwrap();
    // Native mode remains closed unless the temporary compatibility switch is enabled.
    std::sync::Arc::get_mut(&mut svc).unwrap().native_auth = Some(native);
    assert!(!svc.preview_legacy_operator);
    const OWNER: &str = "owner-token-long-enough-for-validation";
    assert!(svc.authenticate_bearer(OWNER, None).await.is_err());
    std::sync::Arc::get_mut(&mut svc)
        .unwrap()
        .preview_legacy_operator = true;
    let operator = svc.authenticate_bearer(OWNER, None).await.unwrap();
    assert_eq!(
        operator.workspace_id(),
        Some(bog_cloud::WorkspaceId::legacy())
    );
    assert!(svc.authenticate_bearer(OWNER, Some(w.id)).await.is_err());
    assert!(svc.auth.issue_agent_token(&operator, "escalate").is_err());
    let legacy = svc
        .registry
        .create("existing-chat", "records-v1", "legacy")
        .unwrap();
    let personal = svc
        .registry
        .create_scoped(w.id, "personal", "records-v1", "personal")
        .unwrap();
    assert!(svc.auth.authorize(&operator, Some(legacy.id), true).is_ok());
    assert!(
        svc.auth
            .authorize(&operator, Some(personal.id), false)
            .is_err()
    );
    let app = svc
        .auth
        .issue(&operator, legacy.id, bog_cloud::Scope::Read)
        .unwrap();
    let app_principal = svc.authenticate_bearer(&app.secret, None).await.unwrap();
    assert!(
        svc.auth
            .authorize(&app_principal, Some(legacy.id), false)
            .is_ok()
    );
    assert!(
        svc.auth
            .authorize(&app_principal, Some(legacy.id), true)
            .is_err()
    );
    assert!(
        svc.auth
            .authorize(&app_principal, Some(personal.id), false)
            .is_err()
    );
    let error = svc
        .auth
        .issue_agent_token(&human, "agent\nname")
        .err()
        .unwrap();
    assert_eq!(error.code, "invalid_request");
    assert!(error.message.contains("without control characters"));
    let token = svc.auth.issue_agent_token(&human, "test agent").unwrap();
    assert!(token.secret.starts_with("bog_agent_"));
    let agent = svc
        .auth
        .authenticate_agent_token(&token.secret, None)
        .unwrap();
    assert_eq!(agent.kind(), bog_cloud::PrincipalKind::Agent);
    assert!(svc.auth.issue_agent_token(&agent, "escalate").is_err());
    svc.auth.revoke_agent_token(&human, &token.id).unwrap();
    assert!(
        svc.auth
            .authenticate_agent_token(&token.secret, None)
            .is_err()
    );
    assert!(svc.auth.authorize(&agent, None, false).is_err());
    server.abort();
}
#[tokio::test]
async fn native_http_device_and_discovery() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let mut svc = CloudService::open(
        Config::new(dir.path().join("cloud"), "/tmp/unused-worker".into()),
        "owner-token-long-enough-for-validation",
    )
    .unwrap();
    let native = NativeAuth::new(
        GithubConfig::for_loopback_testing("http://127.0.0.1").unwrap(),
        &dir.path().join("sessions"),
    )
    .unwrap();
    std::sync::Arc::get_mut(&mut svc).unwrap().native_auth = Some(native);
    let app = bog_cloud::build_rest_router(svc);
    for (path, required) in [
        (
            "/llms.txt",
            vec![
                "# Bog Cloud",
                "GitHub approval is enabled",
                "/agent.md",
                "/auth.md",
            ],
        ),
        (
            "/auth.md",
            vec![
                "including structuredContent",
                "may enter model context, chat transcripts or client logs",
                "private file with owner-only permissions, without printing it",
            ],
        ),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 100000).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        for phrase in required {
            assert!(text.contains(phrase), "{path}: missing {phrase}");
        }
        if path == "/llms.txt" {
            assert!(text.len() < 2048);
            assert!(text.contains("/openapi.json"));
            assert!(!text.contains("WorkOS"));
        }
    }

    let response = app
        .clone()
        .oneshot(Request::builder().uri("/v1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 100000).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["authentication_mode"], "github_native");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/oauth-protected-resource")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/device")
                .header("content-type", "application/json")
                .extension(axum::extract::ConnectInfo(
                    "127.0.0.1:3456".parse::<std::net::SocketAddr>().unwrap(),
                ))
                .body(Body::from(r#"{"name":"agent"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = to_bytes(response.into_body(), 100000).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["interval"], 5);
    assert!(
        !value["verification_uri"]
            .as_str()
            .unwrap()
            .contains(value["device_code"].as_str().unwrap())
    );
    let device_code = value["device_code"].clone();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/device/token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"device_code":value["device_code"]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 100000).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"], "authorization_pending");
    assert_eq!(value["error_detail"]["code"], "authorization_pending");
    assert!(value["error_description"].is_string());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/device/token")
                .header("content-type", "application/json")
                .body(Body::from(json!({"device_code":device_code}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 100000).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"], "slow_down");
    assert_eq!(value["error_detail"]["code"], "slow_down");
    // Only the token endpoint uses the flat OAuth error contract, including bad input.
    for (path, flat) in [("/auth/device/token", true), ("/auth/device", false)] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from("invalid JSON"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = to_bytes(response.into_body(), 100000).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if flat {
            assert_eq!(value["error"], "invalid_request");
            assert_eq!(value["error_detail"]["code"], "invalid_request");
        } else {
            assert_eq!(value["error"]["code"], "invalid_request");
            assert!(value.get("error_detail").is_none());
        }
    }
}

#[tokio::test]
async fn github_transport_rejects_bad_responses_and_never_follows_redirects() {
    use axum::{
        Form,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let capture = seen.clone();
    let app = Router::new()
        .route(
            "/token",
            post(move |Form(form): Form<HashMap<String, String>>| {
                let capture = capture.clone();
                async move {
                    assert_eq!(form["client_id"], "test");
                    assert_eq!(form["client_secret"], "test-secret");
                    assert!(form["redirect_uri"].ends_with("/auth/callback"));
                    *capture.lock().unwrap() =
                        URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
                    match form["code"].as_str() {
                        "denied" => Json(json!({"error":"access_denied"})).into_response(),
                        "malformed" => "not-json".into_response(),
                        "bad-type" => {
                            Json(json!({"access_token":"x","token_type":"MAC"})).into_response()
                        }
                        "scoped" => {
                            Json(json!({"access_token":"x","token_type":"bearer","scope":"repo"}))
                                .into_response()
                        }
                        "scope-shape" => {
                            Json(json!({"access_token":"42","token_type":"bearer","scope":123}))
                                .into_response()
                        }
                        "redirect" => (StatusCode::TEMPORARY_REDIRECT, [("location", "/user")])
                            .into_response(),
                        "large" => "x".repeat(65537).into_response(),
                        code => {
                            Json(json!({"access_token":code,"token_type":"bearer"})).into_response()
                        }
                    }
                }
            }),
        )
        .route(
            "/user",
            get(|headers: HeaderMap| async move {
                match headers.get("authorization").unwrap().to_str().unwrap() {
                    "Bearer wrong-id" => Json(json!({"id":"42"})).into_response(),
                    "Bearer zero" => Json(json!({"id":0})).into_response(),
                    "Bearer user-error" => StatusCode::FORBIDDEN.into_response(),
                    "Bearer user-redirect" => {
                        (StatusCode::TEMPORARY_REDIRECT, [("location", "/user")]).into_response()
                    }
                    "Bearer other" => Json(json!({"id":43,"login":"same-name"})).into_response(),
                    _ => Json(json!({"id":42,"login":"different-name"})).into_response(),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(axum::serve(listener, app).into_future());
    let dir = tempfile::tempdir().unwrap();
    let native = NativeAuth::new(
        GithubConfig::for_loopback_testing(&origin).unwrap(),
        &dir.path().join("sessions"),
    )
    .unwrap();
    for code in [
        "denied",
        "malformed",
        "bad-type",
        "scoped",
        "scope-shape",
        "redirect",
        "large",
        "wrong-id",
        "zero",
        "user-error",
        "user-redirect",
    ] {
        let login = native.begin_login(None).unwrap();
        let u = reqwest::Url::parse(&login.authorization_url).unwrap();
        let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
        let binding = login
            .set_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1;
        assert!(
            native
                .complete_login(&q["state"], code, binding)
                .await
                .is_err(),
            "case {code}"
        );
        assert_eq!(*seen.lock().unwrap(), q["code_challenge"]);
    }
    let mut ids = Vec::new();
    for code in ["42", "renamed", "other"] {
        let l = native.begin_login(None).unwrap();
        let u = reqwest::Url::parse(&l.authorization_url).unwrap();
        let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
        let b = l
            .set_cookie
            .split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1;
        ids.push(
            native
                .complete_login(&q["state"], code, b)
                .await
                .unwrap()
                .identity
                .subject()
                .to_owned(),
        );
    }
    assert_eq!(ids, vec!["42", "42", "43"]);
    let raw = std::fs::read(dir.path().join("sessions/native-sessions.sqlite3")).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("test-secret"));
    server.abort();
}
#[test]
fn native_configuration_is_complete_and_exclusive() {
    const NAMES: &[&str] = &[
        "BOG_GITHUB_CLIENT_ID",
        "BOG_GITHUB_CLIENT_SECRET",
        "BOG_GITHUB_REDIRECT_URI",
        "BOG_WORKOS_ISSUER",
        "BOG_WORKOS_RESOURCE",
        "BOG_WORKOS_AUDIENCE",
        "BOG_WORKOS_CLIENT_ID",
        "BOG_WORKOS_CLIENT_SECRET",
        "BOG_WORKOS_REDIRECT_URI",
        "BOG_WORKOS_DEVICE_CLIENT_ID",
    ];
    if let Ok(mode) = std::env::var("BOG_NATIVE_CONFIG_TEST") {
        let dir = tempfile::tempdir().unwrap();
        let result = NativeAuth::from_env(dir.path());
        match mode.as_str() {
            "none" => assert!(result.unwrap().is_none()),
            "valid" => assert!(result.unwrap().is_some()),
            _ => assert!(result.is_err()),
        };
        return;
    }
    for mode in ["none", "partial", "valid", "mixed", "device-mixed"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "native_configuration_is_complete_and_exclusive"])
            .env("BOG_NATIVE_CONFIG_TEST", mode);
        for name in NAMES {
            child.env_remove(name);
        }
        if mode != "none" {
            child.env("BOG_GITHUB_CLIENT_ID", "id");
        }
        if mode != "partial" && mode != "none" {
            child.env("BOG_GITHUB_CLIENT_SECRET", "secret").env(
                "BOG_GITHUB_REDIRECT_URI",
                "https://flower-bog-cloud.fly.dev/auth/callback",
            );
        }
        if mode == "mixed" {
            child.env("BOG_WORKOS_ISSUER", "https://issuer.test");
        }
        if mode == "device-mixed" {
            child.env("BOG_WORKOS_DEVICE_CLIENT_ID", "id");
        }
        assert!(child.output().unwrap().status.success(), "mode {mode}");
    }
}
