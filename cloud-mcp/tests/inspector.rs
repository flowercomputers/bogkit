mod common;
use common::*;
use rmcp::model::ReadResourceRequestParams;
use serde_json::json;

#[tokio::test]
async fn inspector_resource_is_static_and_plain_clients_keep_structured_results() {
    let h = Harness::composable().await;
    // This SDK client advertises no MCP Apps capability: plain tools still work.
    let client = h.client(OWNER).await;
    let tools = client.list_all_tools().await.unwrap();
    for tool in tools {
        let value = serde_json::to_value(tool).unwrap();
        let app_read = matches!(
            value["name"].as_str().unwrap(),
            "list_resources" | "query_resource" | "search_resource" | "definition_update_status"
        );
        assert_eq!(
            value["_meta"]["ui"]["visibility"],
            if app_read {
                json!(["model", "app"])
            } else {
                json!(["model"])
            }
        );
        if app_read {
            assert_eq!(
                value["_meta"]["ui"]["resourceUri"],
                "ui://bog-cloud/resource-inspector"
            );
            assert_eq!(value["annotations"]["readOnlyHint"], true);
        }
    }
    let result = call(&client, "discover_capabilities", json!({})).await;
    assert!(result.structured_content.is_some());
    assert!(!result.content.is_empty());
    let resource = client
        .read_resource(ReadResourceRequestParams::new(
            "ui://bog-cloud/resource-inspector",
        ))
        .await
        .unwrap();
    let value = serde_json::to_value(resource).unwrap();
    let content = &value["contents"][0];
    assert_eq!(content["mimeType"], "text/html;profile=mcp-app");
    assert_eq!(content["_meta"]["ui"]["csp"]["connectDomains"], json!([]));
    assert_eq!(content["_meta"]["ui"]["permissions"], json!({}));
    let html = content["text"].as_str().unwrap();
    assert!(html.contains("ui/initialize"));
    assert!(!html.contains("BOG_INSPECTOR_SCRIPT"));
    assert!(!html.contains(OWNER));
    assert!(!html.contains("innerHTML"));
    assert!(!html.contains("fetch("));
    client.cancel().await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn inspector_reads_do_not_bypass_bog_or_management_permissions() {
    let h = Harness::composable().await;
    let owner = h.service.auth.authenticate(OWNER).unwrap();
    let a = h.service.registry.create("a", "records-v1", "a").unwrap();
    let b = h.service.registry.create("b", "records-v1", "b").unwrap();
    ready(&h, a.id).await;
    let token = h
        .service
        .auth
        .issue(&owner, a.id, bog_cloud::Scope::Read)
        .unwrap();
    let client = h.client(&token.secret).await;
    // Static UI is readable, but rendering it grants no additional authority.
    client
        .read_resource(ReadResourceRequestParams::new(
            "ui://bog-cloud/resource-inspector",
        ))
        .await
        .unwrap();
    for (name, args) in [
        ("list_resources", json!({"bog_id":b.id})),
        (
            "query_resource",
            json!({"bog_id":b.id,"resource":"records","query":{"action":"list"}}),
        ),
        (
            "search_resource",
            json!({"bog_id":b.id,"resource":"search","query":{"query":"x"}}),
        ),
        (
            "definition_update_status",
            json!({"bog_id":a.id,"job_id":"job"}),
        ),
    ] {
        let result = call(&client, name, args).await;
        assert_eq!(result.is_error, Some(true));
        let code = result.structured_content.unwrap()["error"]["code"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(
            matches!(code.as_str(), "forbidden" | "not_found"),
            "{name}: {code}"
        );
    }
    client.cancel().await.unwrap();
    h.close().await;
}
