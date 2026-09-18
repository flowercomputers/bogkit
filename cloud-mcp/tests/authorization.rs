mod common;
use common::*;
#[tokio::test]
async fn every_request_requires_bearer_and_rejects_unlisted_origin() {
    let h = Harness::new().await;
    let http = reqwest::Client::new();
    assert_eq!(
        http.post(format!("{}/mcp", h.url))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(format!("{}/mcp", h.url))
            .bearer_auth(OWNER)
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    h.close().await;
}
use serde_json::json;
#[tokio::test]
async fn read_scope_cross_database_and_connected_client_revocation() {
    let h = Harness::new().await;
    let owner = h.service.auth.authenticate(OWNER).unwrap();
    let a = h
        .service
        .registry
        .create("first", "records-v1", "first")
        .unwrap();
    let b = h
        .service
        .registry
        .create("second", "records-v1", "second")
        .unwrap();
    ready(&h, a.id).await;
    ready(&h, b.id).await;
    let token = h
        .service
        .auth
        .issue(&owner, a.id, bog_cloud::Scope::Read)
        .unwrap();
    let client = h.client(&token.secret).await;
    assert_ne!(
        call(&client, "describe_bog", json!({"bog_id":a.id}))
            .await
            .is_error,
        Some(true)
    );
    for (name, args, code) in [
        ("get_record", json!({"bog_id":b.id,"key":"x"}), "not_found"),
        (
            "upsert_record",
            json!({"bog_id":a.id,"key":"x","data":{}}),
            "forbidden",
        ),
        (
            "create_bog",
            json!({"name":"denied","template":"records-v1","idempotency_key":"denied"}),
            "forbidden",
        ),
        ("describe_definition", json!({"bog_id":a.id}), "forbidden"),
        ("list_bogs", json!({}), "forbidden"),
    ] {
        let result = call(&client, name, args).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content.unwrap()["error"]["code"], code);
    }
    h.service.auth.revoke(&owner, a.id, &token.id).unwrap();
    assert!(
        client
            .call_tool(
                rmcp::model::CallToolRequestParams::new("describe_bog")
                    .with_arguments(json!({"bog_id":a.id}).as_object().unwrap().clone())
            )
            .await
            .is_err()
    );
    let http = reqwest::Client::new();
    assert_eq!(
        http.post(format!("{}/mcp", h.url))
            .bearer_auth(&token.secret)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(format!("{}/mcp", h.url))
            .bearer_auth(OWNER)
            .header("mcp-session-id", "previous-owner-session")
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let _ = client.cancel().await;
    h.close().await;
}
#[tokio::test]
async fn explicit_origin_allowlist_and_no_authentication_on_other_http_methods() {
    let h = Harness::with_options(bog_cloud_mcp::McpOptions {
        allowed_origins: vec!["https://client.example".into()],
        allowed_hosts: None,
    })
    .await;
    let http = reqwest::Client::new();
    let discovery = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
    for (origin, status) in [
        ("https://client.example", 200),
        ("https://client.example/", 403),
        ("null", 403),
    ] {
        assert_eq!(
            http.post(format!("{}/mcp", h.url))
                .bearer_auth(OWNER)
                .header("origin", origin)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-11-25")
                .json(&discovery)
                .send()
                .await
                .unwrap()
                .status(),
            status
        );
    }
    for method in [
        reqwest::Method::GET,
        reqwest::Method::DELETE,
        reqwest::Method::OPTIONS,
    ] {
        assert_eq!(
            http.request(method, format!("{}/mcp", h.url))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    assert_eq!(
        http.post(format!("{}/mcp", h.url))
            .bearer_auth("invalid-token")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        http.post(format!("{}/mcp", h.url))
            .bearer_auth(OWNER)
            .header("host", "unlisted.example")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    h.close().await;
}
