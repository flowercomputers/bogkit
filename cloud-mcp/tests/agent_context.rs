mod common;
use bog_cloud::{
    CloudService,
    config::Config,
    native_auth::{GithubConfig, NativeAuth},
};
use rmcp::{
    ServiceExt,
    model::*,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use std::sync::Arc;
#[tokio::test]
async fn context_resources_and_private_handoff_use_current_account_permissions() {
    let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}", provider.local_addr().unwrap());
    let counter = Arc::new(std::sync::atomic::AtomicU64::new(42));
    let provider_app = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(|| async {
                axum::Json(
                    json!({"access_token":"fixture-provider","token_type":"bearer","scope":""}),
                )
            }),
        )
        .route(
            "/user",
            axum::routing::get(move || {
                let counter = counter.clone();
                async move {
                    axum::Json(
                        json!({"id":counter.fetch_add(1,std::sync::atomic::Ordering::SeqCst)}),
                    )
                }
            }),
        );
    let provider_task =
        tokio::spawn(async move { axum::serve(provider, provider_app).await.unwrap() });
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let worker = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/bog-records-worker")
        .canonicalize()
        .unwrap();
    let mut service =
        CloudService::open(Config::new(root.path().into(), worker), common::OWNER).unwrap();
    Arc::get_mut(&mut service).unwrap().native_auth = Some(
        NativeAuth::new(
            GithubConfig::for_loopback_testing(&provider_url).unwrap(),
            &root.path().join("auth"),
        )
        .unwrap(),
    );
    let mut people = Vec::new();
    for _ in 0..2 {
        let native = service.native_auth.as_ref().unwrap();
        let login = native.begin_login(None).unwrap();
        let state = reqwest::Url::parse(&login.authorization_url)
            .unwrap()
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
            .complete_login(&state, "fixture-code", binding)
            .await
            .unwrap();
        let identity = session.identity;
        let (_, workspace) = service.auth.provision_identity(&identity).unwrap();
        people.push(
            service
                .auth
                .principal_from_verified(&identity, workspace.id)
                .unwrap(),
        );
    }
    let token = service
        .auth
        .issue_agent_token(&people[0], "test connection")
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = bog_cloud::build_rest_router(service.clone())
        .merge(bog_cloud_mcp::build_mcp_router(service.clone()));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp"))
        .auth_header(token.secret.clone());
    let client = ()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::new(),
            config,
        ))
        .await
        .unwrap();
    assert!(
        client
            .peer_info()
            .unwrap()
            .instructions
            .as_ref()
            .unwrap()
            .contains("prepare_app_access")
    );
    let context = common::call(&client, "get_current_context", json!({}))
        .await
        .structured_content
        .unwrap();
    assert_eq!(context["data"]["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(context["data"]["workspaces"][0]["bog_limit"], 3);
    assert_eq!(
        context["data"]["workspace_id"],
        people[0].workspace_id().unwrap().to_string()
    );
    let templates = common::call(&client, "list_templates", json!({}))
        .await
        .structured_content
        .unwrap();
    assert_eq!(templates["data"]["templates"][0]["id"], "records-v1");
    let resources = client.list_all_resources().await.unwrap();
    assert_eq!(resources.len(), 4);
    for resource in resources {
        let read = client
            .read_resource(ReadResourceRequestParams::new(resource.uri))
            .await
            .unwrap();
        let serialized = serde_json::to_string(&read).unwrap();
        assert!(!serialized.contains(&people[1].workspace_id().unwrap().to_string()));
        assert!(!serialized.contains(&token.secret));
    }
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("bog://guide/unknown"))
            .await
            .is_err()
    );
    let foreign = common::call(
        &client,
        "get_current_context",
        json!({"workspace_id":people[1].workspace_id()}),
    )
    .await;
    assert_eq!(foreign.is_error, Some(true));
    let created = common::call(
        &client,
        "create_bog",
        json!({"name":"private-app","idempotency_key":"private-app"}),
    )
    .await
    .structured_content
    .unwrap();
    let id = created["data"]["id"].clone();
    service
        .supervisor
        .ensure_running(serde_json::from_value(id.clone()).unwrap())
        .await
        .unwrap();
    let prepared = common::call(
        &client,
        "prepare_app_access",
        json!({"bog_id":id,"scope":"write","label":"Private test"}),
    )
    .await
    .structured_content
    .unwrap();
    assert!(prepared["data"].get("token").is_none());
    assert!(service.auth.list_tokens(&people[0]).unwrap().is_empty());
    // Run the shipped helper against this real gateway, using its own private fixture cache.
    let helper_handoff = common::call(
        &client,
        "prepare_app_access",
        json!({"bog_id":id,"scope":"read","label":"Helper test"}),
    )
    .await
    .structured_content
    .unwrap();
    let cache = root.path().join("helper-auth.json");
    let output = root.path().join("private-app.json");
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(&cache,json!({"origin":base,"access_token":token.secret,"expires_at":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()+600}).to_string()).unwrap();
    std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o600)).unwrap();
    let args = vec![
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../scripts/cloud/bog_app_access.py")
            .to_string_lossy()
            .into_owned(),
        "--origin".into(),
        base.clone(),
        "--handoff".into(),
        helper_handoff["data"]["handoff_id"]
            .as_str()
            .unwrap()
            .into(),
        "--auth-file".into(),
        cache.to_string_lossy().into_owned(),
        "--output".into(),
        output.to_string_lossy().into_owned(),
    ];
    let process = tokio::task::spawn_blocking(move || {
        std::process::Command::new("python3")
            .args(args)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(process.status.success(), "private helper failed");
    let private: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(
        std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(
        !String::from_utf8_lossy(&process.stdout)
            .contains(private["BOG_CLOUD_TOKEN"].as_str().unwrap())
    );
    assert!(
        !String::from_utf8_lossy(&process.stderr)
            .contains(private["BOG_CLOUD_TOKEN"].as_str().unwrap())
    );
    let app_principal = service
        .auth
        .authenticate(private["BOG_CLOUD_TOKEN"].as_str().unwrap())
        .unwrap();
    assert!(
        service
            .auth
            .authorize(
                &app_principal,
                Some(serde_json::from_value(id.clone()).unwrap()),
                false
            )
            .is_ok()
    );
    assert!(service.auth.authorize(&app_principal, None, true).is_err());
    let http = reqwest::Client::new();
    let response = http
        .post(format!(
            "{base}{}",
            prepared["data"]["redeem_path"].as_str().unwrap()
        ))
        .bearer_auth(&token.secret)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let credential: Value = response.json().await.unwrap();
    let app_config = StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp"))
        .auth_header(credential["token"].as_str().unwrap());
    let app_client = ()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::new(),
            app_config,
        ))
        .await
        .unwrap();
    common::call(
        &app_client,
        "upsert_record",
        json!({"bog_id":id,"key":"hello","data":{"message":"private access"}}),
    )
    .await;
    let read = common::call(
        &app_client,
        "get_record",
        json!({"bog_id":id,"key":"hello"}),
    )
    .await
    .structured_content
    .unwrap();
    assert_eq!(read["data"]["data"]["message"], "private access");
    assert_eq!(
        common::call(
            &app_client,
            "prepare_app_access",
            json!({"bog_id":id,"scope":"read","label":"forbidden"})
        )
        .await
        .is_error,
        Some(true)
    );
    assert!(
        app_client
            .read_resource(ReadResourceRequestParams::new("bog://guide/allowances"))
            .await
            .is_err()
    );
    let listed = common::call(&client, "list_tokens", json!({"bog_id":id}))
        .await
        .structured_content
        .unwrap();
    assert!(
        listed["data"]["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|token| token["label"] == "Private test")
    );
    assert!(
        !listed
            .to_string()
            .contains(credential["token"].as_str().unwrap())
    );
    common::call(
        &client,
        "revoke_token",
        json!({"bog_id":id,"token_id":credential["id"]}),
    )
    .await;
    assert_eq!(
        http.get(format!(
            "{base}/v1/bogs/{}/docs/hello",
            id.as_str().unwrap()
        ))
        .bearer_auth(credential["token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .status(),
        401
    );
    app_client.cancel().await.unwrap();
    client.cancel().await.unwrap();
    service.supervisor.shutdown().await.unwrap();
    server.abort();
    provider_task.abort();
}
