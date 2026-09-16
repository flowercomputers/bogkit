//! Disposable acceptance run; creates one database and leaves it for inspection.
//! Reads endpoint and owner token from the environment; never prints credentials.
use rmcp::{
    RoleClient, ServiceExt,
    model::CallToolRequestParams,
    service::RunningService,
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
type Client = RunningService<RoleClient, ()>;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
async fn connect(base: &str, token: &str) -> Result<Client> {
    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("{base}/mcp")).auth_header(token);
    Ok(()
        .serve(StreamableHttpClientTransport::with_client(
            reqwest::Client::new(),
            config,
        ))
        .await?)
}
async fn call(client: &Client, name: &str, args: Value) -> Result<Value> {
    let result = client
        .call_tool(
            CallToolRequestParams::new(name.to_owned())
                .with_arguments(args.as_object().ok_or("invalid fixture")?.clone()),
        )
        .await?;
    if result.is_error == Some(true) {
        return Err(format!(
            "{name} failed: {}",
            result
                .structured_content
                .as_ref()
                .and_then(|v| v["error"]["code"].as_str())
                .unwrap_or("tool_error")
        )
        .into());
    }
    Ok(result
        .structured_content
        .ok_or("missing structured result")?["data"]
        .clone())
}
async fn rest_get(http: &reqwest::Client, base: &str, token: &str, path: &str) -> Result<Value> {
    Ok(http
        .get(format!("{base}/v1{path}"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}
#[tokio::main]
async fn main() -> Result<()> {
    let base = std::env::var("BOG_CLOUD_URL")
        .map_err(|_| "BOG_CLOUD_URL is required")?
        .trim_end_matches('/')
        .to_owned();
    let url = reqwest::Url::parse(&base)?;
    if !(url.scheme() == "https"
        || (url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))))
    {
        return Err("remote acceptance requires HTTPS".into());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("BOG_CLOUD_URL must be an origin without credentials, query, or path".into());
    }
    let owner = std::env::var("BOG_CLOUD_TOKEN")
        .map_err(|_| "BOG_CLOUD_TOKEN owner credential required")?;
    let client = connect(&base, &owner).await?;
    let http = reqwest::Client::new();
    let nonce = uuid::Uuid::new_v4().to_string();
    let created=call(&client,"create_bog",json!({"name":format!("MCP acceptance {nonce}"),"template":"records-v1","idempotency_key":nonce})).await?;
    let id = created["id"].as_str().ok_or("missing database ID")?;
    let mut ready = false;
    for _ in 0..150 {
        let state = rest_get(&http, &base, &owner, &format!("/bogs/{id}")).await?;
        if state["status"] == "ready" {
            ready = true;
            break;
        }
        if state["status"] == "failed" {
            return Err("database startup failed".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !ready {
        return Err("database readiness timed out".into());
    }
    let minted: Value = http
        .post(format!("{base}/v1/bogs/{id}/tokens"))
        .bearer_auth(&owner)
        .json(&json!({"scope":"write"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let scoped = connect(
        &base,
        minted["token"].as_str().ok_or("token issuance failed")?,
    )
    .await?;
    let nested = json!({"nested":{"array":[true,null,42,{"unicode":"🌿"}]}});
    call(
        &scoped,
        "upsert_record",
        json!({"bog_id":id,"key":"123","data":nested}),
    )
    .await?;
    assert_eq!(
        call(&scoped, "get_record", json!({"bog_id":id,"key":"123"})).await?["data"],
        nested
    );
    assert_eq!(
        rest_get(&http, &base, &owner, &format!("/bogs/{id}/docs/123")).await?["data"],
        nested
    );
    let replacement = json!({"replaced":true});
    call(
        &scoped,
        "upsert_record",
        json!({"bog_id":id,"key":"123","data":replacement}),
    )
    .await?;
    assert_eq!(
        rest_get(&http, &base, &owner, &format!("/bogs/{id}/docs/123")).await?["data"],
        replacement
    );
    assert_eq!(
        call(&scoped, "read_view", json!({"bog_id":id,"view":"total"})).await?["data"]["value"],
        1
    );
    call(&scoped,"batch",json!({"bog_id":id,"operations":[{"op":"upsert","key":"second","data":{"n":2}},{"op":"remove","key":"123"}]})).await?;
    assert_eq!(
        rest_get(&http, &base, &owner, &format!("/bogs/{id}/views/total")).await?["data"]["value"],
        1
    );
    assert_eq!(
        rest_get(&http, &base, &owner, &format!("/bogs/{id}/docs/second")).await?["data"],
        json!({"n":2})
    );
    call(
        &scoped,
        "delete_record",
        json!({"bog_id":id,"key":"second"}),
    )
    .await?;
    assert_eq!(
        rest_get(&http, &base, &owner, &format!("/bogs/{id}/views/total")).await?["data"]["value"],
        0
    );
    http.delete(format!(
        "{base}/v1/bogs/{id}/tokens/{}",
        minted["id"].as_str().ok_or("token ID missing")?
    ))
    .bearer_auth(&owner)
    .send()
    .await?
    .error_for_status()?;
    assert!(
        scoped
            .call_tool(
                CallToolRequestParams::new("describe_bog")
                    .with_arguments(json!({"bog_id":id}).as_object().unwrap().clone())
            )
            .await
            .is_err(),
        "revoked client retained access"
    );
    println!(
        "PASS SDK 3.4.0 protocol {}: create, nested JSON, replacement, total, batch, delete, independent REST checks, connected-client revocation. Disposable database: {id}",
        client
            .peer_info()
            .ok_or("missing peer info")?
            .protocol_version
            .as_str()
    );
    let _ = scoped.cancel().await;
    client.cancel().await?;
    Ok(())
}
