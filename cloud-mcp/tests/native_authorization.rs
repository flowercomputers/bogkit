mod common;
use axum::{
    Json, Router,
    routing::{get, post},
};
use bog_cloud::{
    CloudService,
    config::Config,
    native_auth::{GithubConfig, NativeAuth},
};
use rmcp::{
    ServiceExt,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use std::{future::IntoFuture, sync::Arc};

#[tokio::test]
async fn real_http_device_login_approval_rest_and_mcp_share_revocable_bog_credential() {
    let github_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let github = format!("http://{}", github_listener.local_addr().unwrap());
    let github_task=tokio::spawn(axum::serve(github_listener,Router::new().route("/token",post(||async{Json(json!({"access_token":"github-private","token_type":"bearer","scope":""}))})).route("/user",get(||async{Json(json!({"id":42,"login":"owner"}))}))).into_future());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let worker = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/bog-records-worker")
        .canonicalize()
        .unwrap();
    let mut svc =
        CloudService::open(Config::new(root.path().into(), worker), common::OWNER).unwrap();
    let mut config = GithubConfig::for_loopback_testing(&github).unwrap();
    config.redirect_uri = format!("{url}/auth/callback");
    Arc::get_mut(&mut svc).unwrap().native_auth =
        Some(NativeAuth::new(config, &root.path().join("sessions")).unwrap());
    let app = bog_cloud::build_rest_router(svc.clone()).merge(
        bog_cloud_mcp::build_mcp_router_with_options(svc.clone(), Default::default()),
    );
    let gateway = tokio::spawn(
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .into_future(),
    );
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let start: Value = http
        .post(format!("{url}/auth/device"))
        .json(&json!({"name":"My test agent"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let private = start["device_code"].as_str().unwrap();
    let public = start["user_code"].as_str().unwrap();
    let pending: Value = http
        .post(format!("{url}/auth/device/token"))
        .json(&json!({"device_code":private}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pending["error"], "authorization_pending");
    let login = http
        .get(format!("{url}/auth/login?user_code={public}"))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 303);
    let binding = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let location = reqwest::Url::parse(login.headers()["location"].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let callback = http
        .get(format!("{url}/auth/callback?state={state}&code=valid"))
        .header("cookie", binding)
        .send()
        .await
        .unwrap();
    assert_eq!(callback.status(), 303);
    assert_eq!(
        callback.headers()["location"],
        format!("/auth/device/approve?user_code={public}")
    );
    let cookie = callback
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|v| v.starts_with("__Host-bog_session="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = http
        .get(format!("{url}/console-session"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let csrf = session["csrf_token"].as_str().unwrap();
    let approve = || {
        http.post(format!("{url}/auth/device/approve"))
            .header("cookie", &cookie)
            .header("origin", &url)
            .header("x-csrf-token", csrf)
    };
    let details: Value = approve()
        .json(&json!({"user_code":public}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(details["name"], "My test agent");
    assert_eq!(
        http.post(format!("{url}/auth/device/approve"))
            .header("cookie", &cookie)
            .json(&json!({"user_code":public,"approve":true}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(
        approve()
            .json(&json!({"user_code":public,"approve":true}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let token: Value = http
        .post(format!("{url}/auth/device/token"))
        .json(&json!({"device_code":private}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bearer = token["access_token"].as_str().unwrap();
    assert!(bearer.starts_with("bog_agent_"));
    assert!(!bearer.contains("github-private"));
    assert!(
        !http
            .post(format!("{url}/auth/device/token"))
            .json(&json!({"device_code":private}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        http.get(format!("{url}/v1/bogs"))
            .bearer_auth(common::OWNER)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.get(format!("{url}/v1/bogs"))
            .bearer_auth("github-private")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let created = http
        .post(format!("{url}/v1/bogs"))
        .bearer_auth(bearer)
        .header("idempotency-key", "native-create")
        .json(&json!({"name":"native-bog"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 202);
    let created: Value = created.json().await.unwrap();
    let id = created["id"].as_str().unwrap();
    svc.supervisor
        .ensure_running(bog_cloud::BogId(uuid::Uuid::parse_str(id).unwrap()))
        .await
        .unwrap();
    assert_eq!(
        http.delete(format!("{url}/v1/bogs/{id}"))
            .bearer_auth(bearer)
            .json(&json!({"confirm":id}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.post(format!("{url}/v1/agent-tokens"))
            .bearer_auth(bearer)
            .json(&json!({"name":"escalate"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let workspace = session["workspaces"][0]["id"].as_str().unwrap();
    assert_eq!(
        http.post(format!("{url}/v1/workspaces/{workspace}/invitations"))
            .bearer_auth(bearer)
            .json(&json!({"role":"owner"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let app_token: Value = http
        .post(format!("{url}/v1/bogs/{id}/tokens"))
        .bearer_auth(bearer)
        .json(&json!({"scope":"write"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        http.post(format!("{url}/v1/bogs"))
            .bearer_auth(app_token["token"].as_str().unwrap())
            .header("idempotency-key", "app-denied")
            .json(&json!({"name":"denied"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let transport =
        StreamableHttpClientTransportConfig::with_uri(format!("{url}/mcp")).auth_header(bearer);
    let mcp = ()
        .serve(StreamableHttpClientTransport::with_client(
            http.clone(),
            transport,
        ))
        .await
        .unwrap();
    let out = common::call(
        &mcp,
        "upsert_record",
        json!({"bog_id":id,"key":"hello","data":{"message":"native"}}),
    )
    .await;
    assert_ne!(out.is_error, Some(true));
    let doc: Value = http
        .get(format!("{url}/v1/bogs/{id}/docs/hello"))
        .bearer_auth(bearer)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(doc.to_string().contains("native"));
    let listed: Value = http
        .get(format!("{url}/v1/agent-tokens"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tid = listed["tokens"][0]["id"].as_str().unwrap();
    assert!(
        http.delete(format!("{url}/v1/agent-tokens/{tid}"))
            .header("cookie", &cookie)
            .header("origin", &url)
            .header("x-csrf-token", csrf)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        http.get(format!("{url}/v1/bogs"))
            .bearer_auth(bearer)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(
        mcp.call_tool(rmcp::model::CallToolRequestParams::new("list_bogs"))
            .await
            .is_err()
    );
    let _ = mcp.cancel().await;
    svc.supervisor.shutdown().await.unwrap();
    gateway.abort();
    github_task.abort();
}
