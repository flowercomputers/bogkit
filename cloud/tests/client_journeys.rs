//! Real local HTTP/worker journeys for the supported clients. Provider identity is
//! a loopback fixture, not an external GitHub login or production approval.
use axum::{
    Json, Router,
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
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

async fn journey(program: &str, script: &str) {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let worker = repository.join("target/debug/bog-records-worker");
    assert!(
        worker.is_file(),
        "Build bog-records-worker before running client journeys"
    );
    let temporary = tempfile::tempdir_in("/tmp").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut config = Config::new(temporary.path().join("cloud"), worker);
    config.composable_enabled = true;
    config.sandboxes_enabled = true;
    let mut service = CloudService::open(
        config,
        "private-local-client-fixture-owner-at-least-32-bytes",
    )
    .unwrap();
    let native = NativeAuth::new(
        GithubConfig::for_loopback_testing(&origin).unwrap(),
        &temporary.path().join("sessions"),
    )
    .unwrap();
    Arc::get_mut(&mut service).unwrap().native_auth = Some(native.clone());
    let provider = Router::new()
        .route(
            "/token",
            post(|| async {
                Json(json!({"access_token":"local-provider-fixture","token_type":"bearer"}))
            }),
        )
        .route("/user", get(|| async { Json(json!({"id":987654321})) }));
    let router = bog_cloud::build_rest_router(service.clone()).merge(provider);
    let server = tokio::spawn(axum::serve(listener, router).into_future());
    let login = native.begin_login(None).unwrap();
    let authorization = reqwest::Url::parse(&login.authorization_url).unwrap();
    let state = authorization
        .query_pairs()
        .find(|(key, _)| key == "state")
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
        .complete_login(&state, "local-fixture-code", binding)
        .await
        .unwrap();
    let (_, workspace) = service.auth.provision_identity(&session.identity).unwrap();
    let human = service
        .auth
        .principal_from_verified(&session.identity, workspace.id)
        .unwrap();
    let credential = service
        .auth
        .issue_agent_token(&human, "client-journey")
        .unwrap();
    let auth_file = temporary.path().join("manager.json");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&auth_file)
        .unwrap();
    serde_json::to_writer(&mut file,&json!({"origin":origin,"access_token":credential.secret,"expires_at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()+600})).unwrap();
    file.flush().unwrap();
    drop(file);
    let mut command = tokio::process::Command::new(program);
    command
        .arg(repository.join(script))
        .current_dir(&repository)
        .env("BOG_TEST_ORIGIN", &origin)
        .env("BOG_TEST_AUTH_FILE", &auth_file)
        .env_remove("BOG_TEST_WORKSPACE_ID")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(120), command.output()).await;
    service.supervisor.shutdown().await.unwrap();
    server.abort();
    let output = output
        .expect("Client journey timed out")
        .expect("Client runtime unavailable");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(&credential.secret) && !stderr.contains(&credential.secret),
        "Client exposed authorization"
    );
    assert!(
        output.status.success(),
        "{program} journey failed: {}",
        stderr.replace(&credential.secret, "[redacted]")
    );
    let result: Value =
        serde_json::from_str(stdout.trim()).expect("Client must emit a JSON summary");
    assert_eq!(result["status"], "passed");
    assert!(result["checks"].as_array().unwrap().len() >= 10);
}

#[tokio::test]
async fn python_and_typescript_complete_private_client_journeys() {
    journey("python3", "clients/python/service_smoke.py").await;
    journey("node", "clients/typescript/test/service-smoke.mjs").await;
}
