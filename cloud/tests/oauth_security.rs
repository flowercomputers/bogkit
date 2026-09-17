use axum::{
    Json, Router,
    extract::{Form, State},
    routing::{get, post},
};
use bog_cloud::{
    browser_auth::BrowserAuth,
    oauth::{WorkOsConfig, WorkOsVerifier},
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn sign(claims: Value, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.into());
    encode(
        &header,
        &claims,
        &EncodingKey::from_rsa_pem(include_bytes!("fixtures/oauth-test-key.pem")).unwrap(),
    )
    .unwrap()
}
#[derive(Clone)]
struct Fixture {
    issuer: String,
    nonce: Arc<Mutex<String>>,
    jwks: Arc<Mutex<Value>>,
    posts: Arc<Mutex<usize>>,
}
async fn setup() -> (Arc<WorkOsVerifier>, Fixture, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let fixture = Fixture {
        issuer: issuer.clone(),
        nonce: Default::default(),
        jwks: Arc::new(Mutex::new(
            serde_json::from_str(include_str!("fixtures/oauth-test-jwks.json")).unwrap(),
        )),
        posts: Default::default(),
    };
    let app=Router::new().route("/oauth2/jwks",get(|State(f):State<Fixture>|async move{Json(f.jwks.lock().unwrap().clone())})).route("/oauth2/token",post(|State(f):State<Fixture>,Form(fields):Form<HashMap<String,String>>|async move{
  assert_eq!(fields["client_secret"],"test-secret");*f.posts.lock().unwrap()+=1;
  if fields["grant_type"]=="authorization_code" {assert_eq!(fields["code_verifier"].len(),64);}
  Json(json!({"access_token":sign(json!({"iss":f.issuer,"aud":"resource-audience","client_id":"browser-client","sub":"user_1","exp":now()+600}),"one"),"id_token":sign(json!({"iss":f.issuer,"aud":"browser-client","sub":"user_1","exp":now()+600,"nonce":f.nonce.lock().unwrap().clone()}),"one"),"refresh_token":"test-refresh","token_type":"bearer"}))
 })).route("/.well-known/openid-configuration",get(|State(f):State<Fixture>|async move{Json(json!({"issuer":f.issuer,"revocation_endpoint":format!("{}/oauth2/revoke",f.issuer)}))})).route("/oauth2/revoke",post(||async{axum::http::StatusCode::OK})).with_state(fixture.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = WorkOsConfig::for_loopback_testing(
        &issuer,
        "http://127.0.0.1/resource",
        "resource-audience",
        "browser-client",
        "test-secret",
        "http://127.0.0.1/callback",
    )
    .unwrap();
    (WorkOsVerifier::new(config).unwrap(), fixture, task)
}
#[tokio::test]
async fn jwt_validation_and_rotation() {
    let (v, f, t) = setup().await;
    let base = json!({"iss":f.issuer,"sub":"user_1","aud":"resource-audience","client_id":"browser-client","exp":now()+600});
    assert_eq!(
        v.verify_access_token(&sign(base.clone(), "one"))
            .await
            .unwrap()
            .subject(),
        "user_1"
    );
    for (key, value) in [
        ("iss", json!("https://evil.example")),
        ("aud", json!("wrong")),
        ("exp", json!(now() - 1)),
        ("sub", json!("")),
        ("nbf", json!(now() + 1000)),
    ] {
        let mut c = base.clone();
        c[key] = value;
        assert!(
            v.verify_access_token(&sign(c, "one")).await.is_err(),
            "{key}"
        );
    }
    let hmac = encode(
        &Header::new(Algorithm::HS256),
        &base,
        &EncodingKey::from_secret(b"wrong-algorithm"),
    )
    .unwrap();
    assert!(v.verify_access_token(&hmac).await.is_err());
    let mut missing_client = base.clone();
    missing_client.as_object_mut().unwrap().remove("client_id");
    assert!(
        v.verify_access_token(&sign(missing_client, "one"))
            .await
            .is_err()
    );
    let mut id_token = base.clone();
    id_token["nonce"] = json!("nonce");
    assert!(v.verify_access_token(&sign(id_token, "one")).await.is_err());
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("exp");
    assert!(v.verify_access_token(&sign(missing, "one")).await.is_err());
    assert!(
        v.verify_access_token(&sign(base.clone(), "missing"))
            .await
            .is_err()
    );
    f.jwks.lock().unwrap()["keys"][0]["kid"] = json!("two");
    tokio::time::sleep(std::time::Duration::from_millis(5100)).await;
    assert!(v.verify_access_token(&sign(base, "two")).await.is_ok());
    t.abort();
}
#[tokio::test]
async fn browser_login_replay_csrf_persistence_refresh_logout() {
    let (v, f, t) = setup().await;
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("private");
    let browser = BrowserAuth::new(v.clone(), &dir).unwrap();
    let start = browser.begin_login().unwrap();
    assert!(start.set_cookie.contains("Secure; HttpOnly; SameSite=Lax"));
    assert!(!start.authorization_url.contains("test-secret"));
    let url = reqwest::Url::parse(&start.authorization_url).unwrap();
    let fields: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(fields["code_challenge_method"], "S256");
    *f.nonce.lock().unwrap() = fields["nonce"].clone();
    let binding = start
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    assert!(
        browser
            .complete_login(&fields["state"], "code", &"a".repeat(64))
            .await
            .is_err()
    );
    let session = browser
        .complete_login(&fields["state"], "code", binding)
        .await
        .unwrap();
    assert!(
        browser
            .complete_login(&fields["state"], "code", binding)
            .await
            .is_err()
    );
    assert_eq!(*f.posts.lock().unwrap(), 1);
    let id = session
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    assert!(browser.authenticate(id).await.is_ok());
    assert!(
        browser
            .check_csrf(id, "https://evil.example", &session.csrf_token)
            .is_err()
    );
    assert!(browser.check_csrf(id, "http://127.0.0.1", "wrong").is_err());
    assert!(
        browser
            .refresh(id, "http://127.0.0.1", &session.csrf_token)
            .await
            .is_ok()
    );
    drop(browser);
    let browser = BrowserAuth::new(v, &dir).unwrap();
    assert!(browser.authenticate(id).await.is_ok());
    let logout = browser
        .logout(id, "http://127.0.0.1", &session.csrf_token)
        .await
        .unwrap();
    assert!(logout.provider_revoked);
    assert!(logout.set_cookie.contains("Max-Age=0"));
    assert!(browser.authenticate(id).await.is_err());
    t.abort();
}
#[test]
fn insecure_config_rejected() {
    assert!(
        WorkOsConfig::new(
            "http://127.0.0.1",
            "https://api.example",
            "aud",
            "client",
            "secret",
            "https://app.example/callback"
        )
        .is_err()
    );
    assert!(
        WorkOsConfig::for_loopback_testing(
            "http://example.com",
            "https://api.example",
            "aud",
            "client",
            "secret",
            "https://app.example/callback"
        )
        .is_err()
    );
    let config = WorkOsConfig::new(
        "https://auth.example",
        "https://api.example",
        "aud",
        "client",
        "super-secret",
        "https://app.example/callback",
    )
    .unwrap();
    assert!(!format!("{config:?}").contains("super-secret"));
}

#[tokio::test]
async fn nonce_mismatch_and_expired_state_fail_closed() {
    let (v, f, t) = setup().await;
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("private");
    let browser = BrowserAuth::new(v, &dir).unwrap();
    let start = browser.begin_login().unwrap();
    let url = reqwest::Url::parse(&start.authorization_url).unwrap();
    let fields: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let binding = start
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    *f.nonce.lock().unwrap() = "wrong-nonce".into();
    assert!(
        browser
            .complete_login(&fields["state"], "code", binding)
            .await
            .is_err()
    );
    *f.nonce.lock().unwrap() = fields["nonce"].clone();
    assert!(
        browser
            .complete_login(&fields["state"], "code", binding)
            .await
            .is_err()
    );
    let start = browser.begin_login().unwrap();
    let url = reqwest::Url::parse(&start.authorization_url).unwrap();
    let fields: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let binding = start
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    rusqlite::Connection::open(dir.join("sessions.sqlite3"))
        .unwrap()
        .execute("UPDATE logins SET expires=0", [])
        .unwrap();
    assert!(
        browser
            .complete_login(&fields["state"], "code", binding)
            .await
            .is_err()
    );
    assert_eq!(*f.posts.lock().unwrap(), 1);
    t.abort();
}
#[tokio::test]
async fn session_absolute_expiry_is_enforced() {
    let (v, f, t) = setup().await;
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("private");
    let browser = BrowserAuth::new(v, &dir).unwrap();
    let start = browser.begin_login().unwrap();
    let fields: HashMap<_, _> = reqwest::Url::parse(&start.authorization_url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    *f.nonce.lock().unwrap() = fields["nonce"].clone();
    let binding = start
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    let session = browser
        .complete_login(&fields["state"], "code", binding)
        .await
        .unwrap();
    let id = session
        .set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    rusqlite::Connection::open(dir.join("sessions.sqlite3"))
        .unwrap()
        .execute("UPDATE sessions SET expires=0", [])
        .unwrap();
    assert!(browser.authenticate(id).await.is_err());
    assert!(
        browser
            .refresh(id, "http://127.0.0.1", &session.csrf_token)
            .await
            .is_err()
    );
    t.abort();
}
#[test]
fn session_storage_rejects_public_directory() {
    use std::os::unix::fs::PermissionsExt;
    let config = WorkOsConfig::new(
        "https://auth.example",
        "https://api.example",
        "aud",
        "client",
        "secret",
        "https://app.example/callback",
    )
    .unwrap();
    let v = WorkOsVerifier::new(config).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(BrowserAuth::new(v, dir.path()).is_err());
}

#[test]
fn device_auth_document_uses_configured_public_client() {
    let config = WorkOsConfig::new(
        "https://auth.example",
        "https://api.example",
        "aud",
        "browser",
        "super-secret",
        "https://app.example/callback",
    )
    .unwrap();
    assert!(config.auth_markdown().contains("not configured"));
    let config = config.with_device_client_id("client_public_cli").unwrap();
    let doc = config.auth_markdown();
    assert!(doc.contains("client_id=client_public_cli"));
    assert!(doc.contains("/oauth2/device_authorization"));
    assert!(doc.contains("authorization_pending"));
    assert!(doc.contains("slow_down"));
    assert!(!doc.contains("super-secret"));
}
