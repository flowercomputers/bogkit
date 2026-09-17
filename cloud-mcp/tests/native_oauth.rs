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
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

trait EncodedRequest {
    fn form(self, pairs: &[(&str, &str)]) -> Self;
    fn query(self, pairs: &[(&str, &str)]) -> Self;
}
fn encoded(pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("https://example.invalid/").unwrap();
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query().unwrap_or("").to_owned()
}
impl EncodedRequest for reqwest::RequestBuilder {
    fn form(self, pairs: &[(&str, &str)]) -> Self {
        self.header("content-type", "application/x-www-form-urlencoded")
            .body(encoded(pairs))
    }
    fn query(self, pairs: &[(&str, &str)]) -> Self {
        let (client, request) = self.build_split();
        let mut request = request.unwrap();
        request.url_mut().set_query(Some(&encoded(pairs)));
        reqwest::RequestBuilder::from_parts(client, request)
    }
}

#[tokio::test]
async fn pkce_consent_scopes_replay_revocation_and_restart_over_real_rest_and_mcp() {
    let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_url = format!("http://{}", provider.local_addr().unwrap());
    let provider_task=tokio::spawn(axum::serve(provider,Router::new().route("/token",post(||async{Json(json!({"access_token":"fixture-provider-token","token_type":"bearer","scope":""}))})).route("/user",get(||async{Json(json!({"id":42}))}))).into_future());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let worker = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/bog-records-worker")
        .canonicalize()
        .unwrap();
    let mut service =
        CloudService::open(Config::new(root.path().into(), worker), common::OWNER).unwrap();
    let mut config = GithubConfig::for_loopback_testing(&provider_url).unwrap();
    config.redirect_uri = format!("{base}/auth/callback");
    let native = NativeAuth::new(config.clone(), &root.path().join("sessions")).unwrap();
    Arc::get_mut(&mut service).unwrap().native_auth = Some(native.clone());
    let app = bog_cloud::build_rest_router(service.clone())
        .merge(bog_cloud_mcp::build_mcp_router(service.clone()));
    let server = tokio::spawn(
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
    let metadata: Value = http
        .get(format!("{base}/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["issuer"], base);
    assert_eq!(
        metadata["code_challenge_methods_supported"],
        json!(["S256"])
    );
    let challenge = http.post(format!("{base}/mcp")).send().await.unwrap();
    assert_eq!(challenge.status(), 401);
    assert!(
        challenge.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("resource_metadata=")
    );
    assert_eq!(metadata["client_id_metadata_document_supported"], true);
    assert_eq!(
        challenge.headers()["www-authenticate"],
        format!(
            "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\", scope=\"bog:read\""
        )
    );
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let resource: Value = http
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(resource["resource"], format!("{base}/mcp"));
    }
    // Invalid CIMD locations fail closed over the authorization transport.
    for client_id in [
        "https://127.0.0.1/client.json",
        "https://localhost/client.json",
        "https://client.example/a#fragment",
    ] {
        assert_eq!(
            http.get(format!("{base}/oauth/authorize"))
                .query(&[
                    ("client_id", client_id),
                    ("redirect_uri", "http://127.0.0.1/cb")
                ])
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let redirect = "http://127.0.0.1:9999/callback";
    for bad in [
        "https://example.com/cb#fragment",
        "http://remote.example/cb",
        "https://user:pass@example.com/cb",
    ] {
        assert_eq!(
            http.post(format!("{base}/oauth/register"))
                .json(&json!({"redirect_uris":[bad]}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    for grants in [
        json!(["refresh_token"]),
        json!(["client_credentials"]),
        json!(["authorization_code", "authorization_code"]),
    ] {
        assert_eq!(
            http.post(format!("{base}/oauth/register"))
                .json(&json!({"redirect_uris":[redirect],"grant_types":grants}))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let registered:Value=http.post(format!("{base}/oauth/register")).json(&json!({"client_name":"Codex","redirect_uris":[redirect],"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]})).send().await.unwrap().json().await.unwrap();
    assert_eq!(registered["grant_types"], json!(["authorization_code"]));
    let client = registered["client_id"].as_str().unwrap();
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
    service.auth.provision_identity(&session.identity).unwrap();
    let cookie = session.set_cookie.split(';').next().unwrap();
    let mut issued = Vec::new();
    for scope in ["bog:read", "bog:write"] {
        let params = [
            ("client_id", client),
            ("redirect_uri", redirect),
            ("response_type", "code"),
            ("resource", &format!("{base}/mcp")),
            ("scope", scope),
            ("state", "fixture-state"),
            ("code_challenge", CHALLENGE),
            ("code_challenge_method", "S256"),
        ];
        let mut wrong = params.to_vec();
        wrong[1].1 = "https://attacker.example/callback";
        assert_eq!(
            http.get(format!("{base}/oauth/authorize"))
                .query(&wrong)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        let mut wrong = params.to_vec();
        wrong[3].1 = "https://another.example/mcp";
        assert_eq!(
            http.get(format!("{base}/oauth/authorize"))
                .query(&wrong)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        let auth = http
            .get(format!("{base}/oauth/authorize"))
            .query(&params)
            .send()
            .await
            .unwrap();
        assert_eq!(auth.status(), 303);
        let public = auth.headers()["location"]
            .to_str()
            .unwrap()
            .split("user_code=")
            .nth(1)
            .unwrap();
        let approve = || {
            http.post(format!("{base}/auth/device/approve"))
                .header("cookie", cookie)
                .header("origin", &base)
                .header("x-csrf-token", &session.csrf_token)
        };
        if scope == "bog:read" {
            let denied_auth = http
                .get(format!("{base}/oauth/authorize"))
                .query(&params)
                .send()
                .await
                .unwrap();
            let denied_public = denied_auth.headers()["location"]
                .to_str()
                .unwrap()
                .split("user_code=")
                .nth(1)
                .unwrap();
            let denial: Value = approve()
                .json(&json!({"user_code":denied_public,"approve":false}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let callback = reqwest::Url::parse(denial["redirect_uri"].as_str().unwrap()).unwrap();
            let query: std::collections::HashMap<_, _> =
                callback.query_pairs().into_owned().collect();
            assert_eq!(query["error"], "access_denied");
            assert_eq!(query["state"], "fixture-state");
            assert!(!query.contains_key("code"));
            assert_eq!(
                approve()
                    .json(&json!({"user_code":denied_public,"approve":true}))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                400
            );
        }
        let details: Value = approve()
            .json(&json!({"user_code":public}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(details["access"].as_str().unwrap().contains(scope));
        assert!(details["access"].as_str().unwrap().contains(redirect));
        assert_eq!(
            http.post(format!("{base}/auth/device/approve"))
                .header("cookie", cookie)
                .json(&json!({"user_code":public,"approve":true}))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let approval: Value = approve()
            .json(&json!({"user_code":public,"approve":true}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let callback = reqwest::Url::parse(approval["redirect_uri"].as_str().unwrap()).unwrap();
        let query: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
        assert_eq!(query["state"], "fixture-state");
        assert_eq!(query["iss"], base);
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", client),
            ("redirect_uri", redirect),
            ("resource", &format!("{base}/mcp")),
            ("code", query["code"].as_str()),
            ("code_verifier", VERIFIER),
        ];
        let mut wrong = form.to_vec();
        wrong[5].1 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        assert_eq!(
            http.post(format!("{base}/oauth/token"))
                .form(&wrong)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        // A valid code cannot be exchanged by another client, at another
        // callback, or for another service. Failed attempts do not consume it.
        for (index, value) in [
            (1, "other-client"),
            (2, "http://127.0.0.1:9998/callback"),
            (3, "https://another.example/mcp"),
        ] {
            let mut wrong = form.to_vec();
            wrong[index].1 = value;
            let response = http
                .post(format!("{base}/oauth/token"))
                .form(&wrong)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 400);
            let error: Value = response.json().await.unwrap();
            assert_eq!(error["error"], "invalid_grant");
        }
        let token: Value = http
            .post(format!("{base}/oauth/token"))
            .form(&form)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(token["scope"], scope);
        assert!(token.get("refresh_token").is_none());
        assert_eq!(
            http.post(format!("{base}/oauth/token"))
                .form(&form)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
        let secret = token["access_token"].as_str().unwrap().to_owned();
        assert_eq!(
            http.get(format!("{base}/v1/bogs"))
                .bearer_auth(&secret)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            http.get(format!("{base}/v1/workspaces"))
                .bearer_auth(&secret)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            http.post(format!("{base}/v1/agent-tokens"))
                .bearer_auth(&secret)
                .json(&json!({"name":"escalation"}))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        let sdk = ()
            .serve(StreamableHttpClientTransport::with_client(
                http.clone(),
                StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp"))
                    .auth_header(&secret),
            ))
            .await
            .unwrap();
        assert!(
            !common::call(&sdk, "list_bogs", json!({}))
                .await
                .is_error
                .unwrap_or(false)
        );
        if scope == "bog:read" {
            let call = |name: &str, arguments: Value| {
                http.post(format!("{base}/mcp"))
                .bearer_auth(&secret).header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-11-25")
                .json(&json!({"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
            };
            let denied = call(
                "create_bog",
                json!({"name":"oauth-test","idempotency_key":"oauth-create"}),
            )
            .send()
            .await
            .unwrap();
            assert_eq!(denied.status(), 403);
            assert_eq!(
                denied.headers()["www-authenticate"],
                format!(
                    "Bearer error=\"insufficient_scope\", scope=\"bog:write\", resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\""
                )
            );
            let list: Value = http
                .get(format!("{base}/v1/bogs"))
                .bearer_auth(&secret)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(list["bogs"], json!([]));
            for args in [
                json!({"name":"missing-key"}),
                json!({"name":"valid","idempotency_key":"foreign","workspace_id":uuid::Uuid::new_v4().to_string()}),
            ] {
                let response = call("create_bog", args).send().await.unwrap();
                assert_eq!(response.status(), 200);
                assert!(response.headers().get("www-authenticate").is_none());
                let value: Value = response.json().await.unwrap();
                assert!(value.get("error").is_some() || value["result"]["isError"] == true);
            }
        } else {
            let created = common::call(
                &sdk,
                "create_bog",
                json!({"name":"oauth-test","idempotency_key":"oauth-create"}),
            )
            .await;
            assert!(!created.is_error.unwrap_or(false));
        }
        sdk.cancel().await.unwrap();
        issued.push(secret);
    }
    let bogs: Value = http
        .get(format!("{base}/v1/bogs"))
        .bearer_auth(&issued[1])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bog = bogs["bogs"][0]["id"].as_str().unwrap();
    service
        .supervisor
        .ensure_running(bog_cloud::BogId(uuid::Uuid::parse_str(bog).unwrap()))
        .await
        .unwrap();
    assert_eq!(
        http.put(format!("{base}/v1/bogs/{bog}/docs/blocked"))
            .bearer_auth(&issued[1])
            .json(&json!({"original":true}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let existing: Value = http
        .get(format!("{base}/v1/bogs/{bog}/docs/blocked"))
        .bearer_auth(&issued[1])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let app_token: Value = http
        .post(format!("{base}/v1/bogs/{bog}/tokens"))
        .bearer_auth(&issued[1])
        .json(&json!({"scope":"read"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // All native OAuth write tools are challenged before any worker or token mutation.
    for (name, args) in [
        (
            "upsert_record",
            json!({"bog_id":bog,"key":"blocked","data":{"bad":true}}),
        ),
        ("delete_record", json!({"bog_id":bog,"key":"blocked"})),
        (
            "batch",
            json!({"bog_id":bog,"operations":[{"op":"upsert","key":"blocked","data":{"bad":true}}]}),
        ),
        ("issue_token", json!({"bog_id":bog,"scope":"write"})),
        (
            "revoke_token",
            json!({"bog_id":bog,"token_id":app_token["id"]}),
        ),
    ] {
        let response = http.post(format!("{base}/mcp")).bearer_auth(&issued[0])
            .header("accept","application/json, text/event-stream").header("mcp-protocol-version","2025-11-25")
            .json(&json!({"jsonrpc":"2.0","id":100,"method":"tools/call","params":{"name":name,"arguments":args}}))
            .send().await.unwrap();
        assert_eq!(response.status(), 403, "{name}");
        assert!(
            response.headers()["www-authenticate"]
                .to_str()
                .unwrap()
                .contains("insufficient_scope")
        );
    }
    let unchanged: Value = http
        .get(format!("{base}/v1/bogs/{bog}/docs/blocked"))
        .bearer_auth(&issued[1])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unchanged["data"], existing["data"]);
    assert_eq!(unchanged["seq"], existing["seq"]);
    assert_eq!(unchanged["cursor"], existing["cursor"]);
    assert_eq!(
        http.get(format!("{base}/v1/bogs/{bog}/docs/blocked"))
            .bearer_auth(app_token["token"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let tokens: Value = http
        .get(format!("{base}/v1/bogs/{bog}/tokens"))
        .bearer_auth(&issued[1])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tokens["tokens"].as_array().unwrap().len(), 1);
    for (token, expected) in [(&issued[0], 403), (&issued[1], 200)] {
        assert_eq!(
            http.put(format!("{base}/v1/bogs/{bog}/docs/item"))
                .bearer_auth(token)
                .json(&json!({"ok":true}))
                .send()
                .await
                .unwrap()
                .status(),
            expected
        );
    }
    assert_eq!(
        http.post(format!("{base}/v1/bogs/{bog}/tokens"))
            .bearer_auth(&issued[0])
            .json(&json!({"scope":"write"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        http.delete(format!("{base}/v1/bogs/{bog}"))
            .bearer_auth(&issued[1])
            .json(&json!({"confirm":bog}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    // Authentication storage survives reopen; the active token is still checked
    // against the account registry and cannot be used as a regular agent token.
    let reopened = NativeAuth::new(config, &root.path().join("sessions")).unwrap();
    drop(reopened);
    assert!(
        service
            .auth
            .authenticate_agent_token(&issued[0], None)
            .is_err()
    );
    assert_eq!(
        http.post(format!("{base}/oauth/revoke"))
            .form(&[("token", issued[0].as_str()), ("client_id", client)])
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        http.get(format!("{base}/v1/bogs"))
            .bearer_auth(&issued[0])
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.get(format!("{base}/v1/bogs"))
            .bearer_auth(&issued[1])
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let agent_list: Value = http
        .get(format!("{base}/v1/agent-tokens"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let active = agent_list["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["revoked_at"].is_null())
        .unwrap();
    assert_eq!(
        http.delete(format!(
            "{base}/v1/agent-tokens/{}",
            active["id"].as_str().unwrap()
        ))
        .header("cookie", cookie)
        .header("origin", &base)
        .header("x-csrf-token", &session.csrf_token)
        .send()
        .await
        .unwrap()
        .status(),
        200
    );
    assert_eq!(
        http.get(format!("{base}/v1/bogs"))
            .bearer_auth(&issued[1])
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    service.supervisor.shutdown().await.unwrap();
    server.abort();
    provider_task.abort();
}
