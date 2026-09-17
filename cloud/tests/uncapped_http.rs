use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::Request,
    routing::{get, post},
};
use bog_cloud::{
    CloudService,
    config::Config,
    native_auth::{GithubConfig, NativeAuth},
};
use serde_json::{Value, json};
use std::{
    future::IntoFuture,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tower::ServiceExt;
const OWNER: &str = "private-operator-test-secret-at-least32bytes";
#[allow(clippy::too_many_arguments)] // Explicit credentials make each authorization case readable.
async fn request(
    router: &Router,
    method: &str,
    path: &str,
    cookie: &str,
    csrf: &str,
    origin: &str,
    token: Option<&str>,
    body: Value,
) -> (u16, Value) {
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", "org-retry-key");
    if let Some(token) = token {
        r = r.header("authorization", format!("Bearer {token}"));
    } else {
        r = r
            .header("cookie", cookie)
            .header("origin", origin)
            .header("x-csrf-token", csrf);
    }
    let response = router
        .clone()
        .oneshot(
            r.body(Body::from(if body.is_null() {
                String::new()
            } else {
                body.to_string()
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let b = to_bytes(response.into_body(), 1048576).await.unwrap();
    (status, serde_json::from_slice(&b).unwrap_or(Value::Null))
}
#[tokio::test]
async fn human_operator_controls_are_csrf_protected_and_never_inherited_by_agents() {
    let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", provider.local_addr().unwrap());
    let identity = Arc::new(AtomicU64::new(42));
    let identity2 = identity.clone();
    let mock = tokio::spawn(
        axum::serve(
            provider,
            Router::new()
                .route(
                    "/token",
                    post(|| async {
                        Json(json!({"access_token":"test-provider-token","token_type":"bearer"}))
                    }),
                )
                .route(
                    "/user",
                    get(move || {
                        let id = identity2.load(Ordering::Relaxed);
                        async move { Json(json!({"id":id})) }
                    }),
                ),
        )
        .into_future(),
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut svc = CloudService::open(
        Config::new(tmp.path().join("cloud"), "/tmp/not-started-worker".into()),
        OWNER,
    )
    .unwrap();
    let native = NativeAuth::new(
        GithubConfig::for_loopback_testing(&origin).unwrap(),
        &tmp.path().join("sessions"),
    )
    .unwrap();
    let mut sessions = Vec::new();
    for id in [42, 43] {
        identity.store(id, Ordering::Relaxed);
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
        sessions.push(
            native
                .complete_login(&state, "code", binding)
                .await
                .unwrap(),
        );
    }
    let (account, personal) = svc.auth.provision_identity(&sessions[0].identity).unwrap();
    let human = svc
        .auth
        .principal_from_verified(&sessions[0].identity, personal.id)
        .unwrap();
    svc.auth.provision_identity(&sessions[1].identity).unwrap();
    let operator = svc.auth.authenticate(OWNER).unwrap();
    let agent = svc.auth.issue_agent_token(&human, "test-agent").unwrap();
    Arc::get_mut(&mut svc).unwrap().native_auth = Some(native);
    let router = bog_cloud::build_rest_router(svc.clone());
    let cookie = sessions[0].set_cookie.split(';').next().unwrap();
    let csrf = &sessions[0].csrf_token;
    let call = |method, path, token, body| {
        request(&router, method, path, cookie, csrf, &origin, token, body)
    };
    assert_eq!(call("GET", "/v1/platform", None, Value::Null).await.0, 403);
    svc.auth
        .set_platform_operator(&operator, &account.id, true)
        .unwrap();
    assert_eq!(
        call("GET", "/v1/me", None, Value::Null).await.1["platform_operator"],
        true
    );
    let (status, created) = call("POST", "/v1/workspaces", None, json!({"name":"Flower"})).await;
    assert_eq!(status, 200, "{created}");
    let id = created["workspace"]["id"].as_str().unwrap();
    assert_eq!(
        call("POST", "/v1/workspaces", None, json!({"name":"Flower"}))
            .await
            .1["workspace"]["id"],
        id
    );
    assert_eq!(
        call("POST", "/v1/workspaces", None, json!({"name":"Other"}))
            .await
            .0,
        409
    );
    assert_eq!(
        call(
            "POST",
            "/v1/workspaces",
            Some(agent.secret.as_str()),
            json!({"name":"Agent org"})
        )
        .await
        .0,
        403
    );
    let path = format!("/v1/platform/workspaces/{id}/quota");
    assert_eq!(
        request(
            &router,
            "PUT",
            &path,
            cookie,
            "wrong",
            &origin,
            None,
            json!({"uncapped_bogs":true})
        )
        .await
        .0,
        401
    );
    assert_eq!(
        call(
            "PUT",
            &path,
            Some(agent.secret.as_str()),
            json!({"uncapped_bogs":true})
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(
            "GET",
            "/v1/platform",
            Some(agent.secret.as_str()),
            Value::Null
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call("PUT", &path, None, json!({"uncapped_bogs":"yes"}))
            .await
            .0,
        400
    );
    assert_eq!(
        call(
            "PUT",
            &path,
            None,
            json!({"uncapped_bogs":true,"platform_operator":true})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call("PUT", &path, None, json!({"uncapped_bogs":true}))
            .await
            .0,
        200
    );
    let list = call("GET", "/v1/workspaces", None, Value::Null).await.1;
    let flower = list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == id)
        .unwrap();
    assert!(flower["bog_limit"].is_null());
    let other_cookie = sessions[1].set_cookie.split(';').next().unwrap();
    assert_eq!(
        request(
            &router,
            "PUT",
            &path,
            other_cookie,
            &sessions[1].csrf_token,
            &origin,
            None,
            json!({"uncapped_bogs":true})
        )
        .await
        .0,
        403
    );
    let other_list = request(
        &router,
        "GET",
        "/v1/workspaces",
        other_cookie,
        &sessions[1].csrf_token,
        &origin,
        None,
        Value::Null,
    )
    .await
    .1;
    assert_eq!(other_list["workspaces"].as_array().unwrap().len(), 1);
    let b = svc
        .registry
        .create_for_principal(&human, "test", "records-v1", "test", 32)
        .unwrap();
    let app = svc
        .auth
        .issue_app_token(&human, b.id, bog_cloud::Scope::Write)
        .unwrap();
    assert_eq!(
        call(
            "GET",
            "/v1/platform",
            Some(app.secret.as_str()),
            Value::Null
        )
        .await
        .0,
        403
    );
    assert_eq!(
        call(
            "PUT",
            &path,
            Some(app.secret.as_str()),
            json!({"uncapped_bogs":true})
        )
        .await
        .0,
        403
    );
    let account_path = format!("/v1/platform/accounts/{}/quota", account.id);
    assert_eq!(
        call("PUT", &account_path, None, json!({"uncapped_bogs":true}))
            .await
            .0,
        200
    );
    let data = call("GET", "/v1/platform", None, Value::Null).await.1;
    assert!(
        data["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == account.id && a["uncapped_bogs"] == true)
    );
    svc.auth
        .set_platform_operator(&operator, &account.id, false)
        .unwrap();
    assert_eq!(call("GET", "/v1/platform", None, Value::Null).await.0, 403);
    mock.abort();
}
