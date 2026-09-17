mod common;
use common::*;
#[tokio::test]
async fn sdk_initializes_and_discovers_exact_tools() {
    let h = Harness::new().await;
    let client = h.client(OWNER).await;
    let list = client.list_all_tools().await.unwrap();
    let mut names = list.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec![
            "batch",
            "create_bog",
            "delete_record",
            "describe_bog",
            "get_record",
            "issue_token",
            "list_bogs",
            "list_tokens",
            "list_workspaces",
            "read_view",
            "revoke_token",
            "upsert_record",
            "wait_for_change"
        ]
    );
    for tool in &list {
        assert!(tool.output_schema.is_some());
        assert_eq!(tool.input_schema["additionalProperties"], false);
        let hints = tool.annotations.as_ref().unwrap();
        assert_eq!(hints.open_world_hint, Some(false));
        if ["get_record", "list_bogs", "describe_bog", "read_view"].contains(&tool.name.as_ref()) {
            assert_eq!(hints.read_only_hint, Some(true));
        }
    }
    assert_eq!(
        list.iter()
            .find(|t| t.name == "batch")
            .unwrap()
            .annotations
            .as_ref()
            .unwrap()
            .idempotent_hint,
        Some(false)
    );
    client.cancel().await.unwrap();
    h.close().await;
}

use serde_json::{Value, json};
fn data(result: rmcp::model::CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result.structured_content.unwrap()["data"].clone()
}
#[tokio::test]
async fn sdk_crud_batch_idempotency_and_independent_rest() {
    let h = Harness::new().await;
    let client = h.client(OWNER).await;
    assert_eq!(
        client.peer_info().unwrap().protocol_version.as_str(),
        "2025-11-25"
    );
    let args =
        json!({"name":"sdk fixture","template":"records-v1","idempotency_key":"sdk-fixture"});
    let created = data(call(&client, "create_bog", args.clone()).await);
    let id = created["id"].as_str().unwrap();
    assert_eq!(data(call(&client, "create_bog", args).await)["id"], id);
    let conflict = call(
        &client,
        "create_bog",
        json!({"name":"changed","template":"records-v1","idempotency_key":"sdk-fixture"}),
    )
    .await;
    assert_eq!(
        conflict.structured_content.unwrap()["error"]["code"],
        "conflict"
    );
    ready(&h, serde_json::from_value(created["id"].clone()).unwrap()).await;
    assert_eq!(
        data(call(&client, "describe_bog", json!({"bog_id":id})).await)["schema"]["template_version"],
        "records-v1"
    );
    assert_eq!(
        data(call(&client, "list_bogs", json!({})).await)["bogs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let nested = json!({"title":"moss 🌿","nested":{"array":[null,1,true,{"x":"y"}]}});
    assert_eq!(
        data(
            call(
                &client,
                "upsert_record",
                json!({"bog_id":id,"key":"123","data":nested})
            )
            .await
        )["replaced"],
        false
    );
    assert_eq!(
        data(call(&client, "get_record", json!({"bog_id":id,"key":"123"})).await)["data"],
        nested
    );
    let rest: Value = reqwest::Client::new()
        .get(format!("{}/v1/bogs/{id}/docs/123", h.url))
        .bearer_auth(OWNER)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rest["data"], nested);
    assert_eq!(
        data(
            call(
                &client,
                "upsert_record",
                json!({"bog_id":id,"key":"123","data":{"replacement":true}})
            )
            .await
        )["replaced"],
        true
    );
    assert_eq!(
        data(call(&client, "read_view", json!({"bog_id":id,"view":"total"})).await)["data"]["value"],
        1
    );
    let failed=call(&client,"batch",json!({"bog_id":id,"operations":[{"op":"upsert","key":"new","data":{}},{"op":"upsert","key":"bad/key","data":{}}]})).await;
    assert_eq!(failed.is_error, Some(true));
    assert_eq!(
        data(call(&client, "read_view", json!({"bog_id":id,"view":"total"})).await)["data"]["value"],
        1
    );
    assert_eq!(data(call(&client,"batch",json!({"bog_id":id,"operations":[{"op":"upsert","key":"new","data":{"v":2}},{"op":"remove","key":"123"}]})).await)["applied"],2);
    assert_eq!(
        data(call(&client, "delete_record", json!({"bog_id":id,"key":"new"})).await)["removed"],
        true
    );
    assert_eq!(
        data(call(&client, "read_view", json!({"bog_id":id,"view":"total"})).await)["data"]["value"],
        0
    );
    for (name, args) in [
        ("unknown", json!({})),
        ("get_record", json!({"bog_id":id})),
        (
            "get_record",
            json!({"bog_id":id,"key":"x","principal":"owner"}),
        ),
        ("upsert_record", json!({"bog_id":id,"key":"x","data":1})),
    ] {
        assert!(
            client
                .call_tool(
                    rmcp::model::CallToolRequestParams::new(name)
                        .with_arguments(args.as_object().unwrap().clone())
                )
                .await
                .is_err()
        );
    }
    let missing = call(&client, "get_record", json!({"bog_id":id,"key":"missing"})).await;
    assert_eq!(missing.is_error, Some(true));
    assert_eq!(
        missing.structured_content.unwrap()["error"]["code"],
        "not_found"
    );
    client.cancel().await.unwrap();
    h.close().await;
}
#[tokio::test]
async fn legacy_initialize_wire_discovery_and_bad_jsonrpc() {
    let h = Harness::new().await;
    let http = reqwest::Client::new();
    let post = |body: Value| {
        http.post(format!("{}/mcp", h.url))
            .bearer_auth(OWNER)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-11-25")
            .json(&body)
    };
    let init=post(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"legacy-test","version":"1"}}})).send().await.unwrap();
    assert_eq!(init.status(), 200);
    assert!(!init.headers().contains_key("mcp-session-id"));
    let init: Value = init.json().await.unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        post(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    let list: Value = post(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 13);
    let bad = post(json!({"jsonrpc":"bogus","id":3,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert!(bad.status().is_client_error());
    h.close().await;
}
#[tokio::test]
async fn results_and_request_bodies_are_bounded() {
    let h = Harness::new().await;
    let client = h.client(OWNER).await;
    let created = data(
        call(
            &client,
            "create_bog",
            json!({"name":"large","template":"records-v1","idempotency_key":"large"}),
        )
        .await,
    );
    let id = created["id"].as_str().unwrap();
    ready(&h, serde_json::from_value(created["id"].clone()).unwrap()).await;
    for n in 0..3 {
        data(
            call(
                &client,
                "upsert_record",
                json!({"bog_id":id,"key":n.to_string(),"data":{"text":"a".repeat(200_000)}}),
            )
            .await,
        );
    }
    let result = call(&client, "read_view", json!({"bog_id":id,"view":"docs"})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.unwrap()["error"]["code"],
        "result_too_large"
    );
    assert_ne!(
        call(
            &client,
            "read_view",
            json!({"bog_id":id,"view":"docs","limit":1})
        )
        .await
        .is_error,
        Some(true)
    );
    let response = reqwest::Client::new()
        .post(format!("{}/mcp", h.url))
        .bearer_auth(OWNER)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body("x".repeat(1024 * 1024 + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    client.cancel().await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn mcp_and_rest_share_change_cursors() {
    let h = Harness::new().await;
    let client = h.client(OWNER).await;
    let b = h
        .service
        .registry
        .create("changes", "records-v1", "changes")
        .unwrap();
    ready(&h, b.id).await;
    let initial = data(
        call(
            &client,
            "wait_for_change",
            json!({"bog_id":b.id,"timeout_seconds":0}),
        )
        .await,
    );
    assert_eq!(initial["changed"], false);
    data(
        call(
            &client,
            "upsert_record",
            json!({"bog_id":b.id,"key":"one","data":{"value":1}}),
        )
        .await,
    );
    let response = reqwest::Client::new()
        .get(format!(
            "{}/v1/bogs/{}/changes?cursor={}&timeout=0",
            h.url,
            b.id,
            initial["cursor"].as_str().unwrap()
        ))
        .bearer_auth(OWNER)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let changed: Value = response.json().await.unwrap();
    assert_eq!(changed["changed"], true);
    assert_eq!(changed["reset"], false);
    let read = data(call(&client, "read_view", json!({"bog_id":b.id,"view":"total"})).await);
    assert_eq!(read["cursor"], changed["cursor"]);
    let timeout = data(
        call(
            &client,
            "wait_for_change",
            json!({"bog_id":b.id,"cursor":read["cursor"],"timeout_seconds":0}),
        )
        .await,
    );
    assert_eq!(timeout["changed"], false);
    client.cancel().await.unwrap();
    h.close().await;
}
