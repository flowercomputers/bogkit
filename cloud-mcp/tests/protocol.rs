mod common;
use common::*;
fn composable_fixture(name: &str) -> serde_json::Value {
    serde_json::from_slice(
        &std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../docs/examples/composable")
                .join(name),
        )
        .unwrap(),
    )
    .unwrap()
}
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
            "apply_definition_update",
            "batch",
            "bog_events",
            "bog_metrics",
            "create_bog",
            "create_bog_from_definition",
            "definition_update_status",
            "delete_record",
            "describe_bog",
            "describe_definition",
            "discover_capabilities",
            "get_current_context",
            "get_record",
            "issue_token",
            "list_bogs",
            "list_resources",
            "list_templates",
            "list_tokens",
            "list_workspaces",
            "plan_definition_update",
            "prepare_app_access",
            "query_resource",
            "read_view",
            "revoke_token",
            "search_resource",
            "upsert_record",
            "validate_definition",
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
    let batch_schema = serde_json::to_value(
        &list
            .iter()
            .find(|t| t.name == "batch")
            .unwrap()
            .input_schema,
    )
    .unwrap();
    let op = json!([{"op":"upsert","key":"record","data":{}}]);
    let id = "00000000-0000-4000-8000-000000000001";
    assert!(jsonschema::is_valid(
        &batch_schema,
        &json!({"bog_id":id,"ops":op})
    ));
    assert!(jsonschema::is_valid(
        &batch_schema,
        &json!({"bog_id":id,"operations":op})
    ));
    assert!(!jsonschema::is_valid(&batch_schema, &json!({"bog_id":id})));
    assert!(!jsonschema::is_valid(
        &batch_schema,
        &json!({"bog_id":id,"ops":op,"operations":op})
    ));
    for name in ["plan_definition_update", "apply_definition_update"] {
        let revision = &list
            .iter()
            .find(|tool| tool.name == name)
            .unwrap()
            .input_schema["properties"]["expected_revision"];
        assert_eq!(revision["minimum"], 1);
        assert_eq!(revision["maximum"], 9_007_199_254_740_991_u64);
    }
    assert_eq!(
        list.iter()
            .find(|t| t.name == "apply_definition_update")
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
    assert_eq!(data(call(&client, "batch", json!({"bog_id":id,"ops":[{"op":"upsert","key":"canonical","data":{}},{"op":"remove","key":"canonical"}]})).await)["applied"], 2);
    assert!(
        client
            .call_tool(
                rmcp::model::CallToolRequestParams::new("batch").with_arguments(
                    json!({"bog_id":id,"ops":[],"operations":[]})
                        .as_object()
                        .unwrap()
                        .clone()
                )
            )
            .await
            .is_err()
    );
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
async fn composable_resources_hide_unexposed_resources_and_match_rest() {
    let h = Harness::composable().await;
    let client = h.client(OWNER).await;
    let definition = json!({
        "resources": {
            "public_total": {"terminal":{"kind":"count"}},
            "private_total": {"terminal":{"kind":"count"}}
        },
        "expose": {
            "read_public_total": {"target":"public_total","action":"read"}
        }
    });
    let created = data(
        call(
            &client,
            "create_bog_from_definition",
            json!({"name":"private resource fixture","definition":definition,"idempotency_key":"private-resource-fixture"}),
        )
        .await,
    );
    let id = created["id"].as_str().unwrap();
    let mcp = data(call(&client, "list_resources", json!({"bog_id":id})).await);
    assert_eq!(mcp["resources"].as_array().unwrap().len(), 1);
    assert_eq!(mcp["resources"][0]["name"], "public_total");
    assert!(!mcp.to_string().contains("private_total"));

    let mut rest: Value = reqwest::Client::new()
        .get(format!("{}/v1/bogs/{id}/resources", h.url))
        .bearer_auth(OWNER)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    rest.as_object_mut().unwrap().remove("request_id");
    assert_eq!(rest, mcp);

    let hidden = call(
        &client,
        "query_resource",
        json!({"bog_id":id,"resource":"private_total","query":{}}),
    )
    .await;
    assert_eq!(hidden.is_error, Some(true));
    let error = hidden.structured_content.unwrap();
    assert_eq!(error["error"]["code"], "not_found");
    assert!(
        error["error"]["next_action"]
            .as_str()
            .unwrap()
            .contains("list_resources")
    );
    client.cancel().await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn composable_todo_queries_and_searches_match_http() {
    let h = Harness::composable().await;
    let client = h.client(OWNER).await;
    let definition = composable_fixture("todo-semantic.json");
    let operations = composable_fixture("todo-records.json");
    let created = data(
        call(
            &client,
            "create_bog_from_definition",
            json!({"name":"MCP todo semantic fixture","definition":definition,"idempotency_key":"mcp-todo-semantic-fixture"}),
        )
        .await,
    );
    let id = created["id"].as_str().unwrap();
    ready(&h, serde_json::from_value(created["id"].clone()).unwrap()).await;
    data(
        call(
            &client,
            "batch",
            json!({"bog_id":id,"operations":operations}),
        )
        .await,
    );

    assert_eq!(created["kind"], "defined");
    assert!(created["template"].is_null());
    assert!(created["template_version"].is_null());
    let resource_contracts = data(call(&client, "list_resources", json!({"bog_id":id})).await);
    let http = reqwest::Client::new();
    for (tool, resource, query) in [
        ("query_resource", "open", json!({})),
        ("query_resource", "open_count", json!({})),
        ("query_resource", "priority", json!({})),
        (
            "search_resource",
            "text_search",
            json!({"query":"release notes","limit":10,"include_fields":["/title","/missing"]}),
        ),
        (
            "search_resource",
            "semantic_search",
            json!({"query":"publish the changelog","limit":10,"include_fields":["/title"],"max_distance":2}),
        ),
    ] {
        let mcp = data(
            call(
                &client,
                tool,
                json!({"bog_id":id,"resource":resource,"query":query.clone()}),
            )
            .await,
        );
        let resource_contract = resource_contracts["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == resource)
            .unwrap();
        let action = if tool == "search_resource" {
            "search"
        } else {
            resource_contract["default_query_action"].as_str().unwrap()
        };
        let operation = resource_contract["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["action"] == action)
            .unwrap();
        assert!(
            jsonschema::is_valid(&operation["hosted"]["request_schema"], &query),
            "invalid advertised request for {resource}"
        );
        assert!(
            jsonschema::is_valid(&operation["response_schema"], &mcp["data"]),
            "invalid advertised response for {resource}: {mcp}"
        );
        if tool == "search_resource" {
            let hits = mcp["data"].as_array().unwrap();
            assert!(!hits.is_empty());
            for hit in hits {
                assert!(hit["value"]["/title"].is_string());
                assert!(hit["value"].get("/missing").is_none());
            }
        }
        let suffix = if tool == "search_resource" {
            "search"
        } else {
            "query"
        };
        let mut rest: Value = http
            .post(format!(
                "{}/v1/bogs/{id}/resources/{resource}/{suffix}",
                h.url
            ))
            .bearer_auth(OWNER)
            .json(&query)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        rest.as_object_mut().unwrap().remove("request_id");
        assert_eq!(mcp, rest, "MCP/HTTP mismatch for {resource}");
    }
    let resources = data(call(&client, "list_resources", json!({"bog_id":id})).await);
    assert!(!resources.to_string().contains("private_stats"));
    assert_eq!(
        data(
            call(
                &client,
                "query_resource",
                json!({"bog_id":id,"resource":"open_count","query":{}}),
            )
            .await,
        )["data"],
        2
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
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 28);
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
    let invalid = call(
        &client,
        "wait_for_change",
        json!({"bog_id":b.id,"timeout_seconds":26}),
    )
    .await;
    assert_eq!(invalid.is_error, Some(true));
    assert_eq!(
        invalid.structured_content.unwrap()["error"]["code"],
        "invalid_request"
    );
    assert_eq!(
        reqwest::Client::new()
            .get(format!("{}/v1/bogs/{}/changes?timeout=26", h.url, b.id))
            .bearer_auth(OWNER)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
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

#[tokio::test]
async fn repair_schema_and_request_identifiers() {
    let h = Harness::new().await;
    let client = h.client(OWNER).await;
    let list = client.list_all_tools().await.unwrap();
    let create = list.iter().find(|t| t.name == "create_bog").unwrap();
    assert!(
        create.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("name"))
    );
    assert_eq!(create.input_schema["properties"]["name"]["type"], "string");
    let wait = list.iter().find(|t| t.name == "wait_for_change").unwrap();
    assert!(wait.input_schema["properties"]["timeout"].is_object());
    let alias = &wait.input_schema["properties"]["timeout_seconds"];
    assert_eq!(alias["type"], "integer");
    assert_eq!(alias["deprecated"], true);
    assert_eq!(alias["maximum"], 25);
    assert_eq!(wait.input_schema["properties"]["timeout"]["maximum"], 25);
    assert_eq!(
        wait.input_schema["not"]["required"],
        json!(["timeout", "timeout_seconds"])
    );
    assert!(
        !wait.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("timeout_seconds"))
    );

    let listing = list.iter().find(|t| t.name == "list_bogs").unwrap();
    assert_eq!(
        listing.input_schema["properties"]["workspace_id"]["type"],
        "string"
    );
    let denied = call(
        &client,
        "list_bogs",
        json!({"workspace_id":uuid::Uuid::new_v4()}),
    )
    .await;
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(
        denied.structured_content.unwrap()["error"]["code"],
        "forbidden"
    );
    let listed = call(
        &client,
        "list_bogs",
        json!({"workspace_id":"00000000-0000-0000-0000-000000000001"}),
    )
    .await;
    assert_ne!(listed.is_error, Some(true));
    assert!(listed.structured_content.unwrap()["request_id"].is_string());
    client.cancel().await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn repair_timeout_alias_and_errors_correlate_with_http_headers() {
    let h = Harness::new().await;
    let bog = h
        .service
        .registry
        .create("repair", "records-v1", "repair")
        .unwrap();
    ready(&h, bog.id).await;
    for (name, args, invalid) in [
        (
            "wait_for_change",
            json!({"bog_id":bog.id,"timeout":0}),
            false,
        ),
        (
            "wait_for_change",
            json!({"bog_id":bog.id,"timeout_seconds":0}),
            false,
        ),
        (
            "wait_for_change",
            json!({"bog_id":bog.id,"timeout":0,"timeout_seconds":0}),
            true,
        ),
        ("create_bog", json!({}), true),
        (
            "create_bog",
            json!({"name":"x","idempotency_key":null,"template":"nope","colour":"red"}),
            true,
        ),
        ("list_bogs", json!({"workspace_id":"bad"}), true),
        (
            "read_view",
            json!({"bog_id":bog.id,"view":"not-a-view"}),
            false,
        ),
    ] {
        let response=reqwest::Client::new().post(format!("{}/mcp",h.url)).bearer_auth(OWNER)
            .header("accept","application/json, text/event-stream")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}))
            .send().await.unwrap();
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let body: Value = response.json().await.unwrap();
        let reported = if invalid {
            &body["error"]["data"]["request_id"]
        } else {
            &body["result"]["structuredContent"]["request_id"]
        };
        assert_eq!(reported, &request_id, "{body}");
        if name == "create_bog" {
            let message = body["error"]["message"].as_str().unwrap();
            assert!(message.contains("idempotency_key"), "{body}");
            assert!(
                message.contains("name") || message.contains("colour"),
                "{body}"
            );
        }
    }
    h.close().await;
}

#[tokio::test]
async fn newer_clients_negotiate_verified_revision_and_cannot_select_newer_inline_protocol() {
    let h = Harness::new().await;
    let http = reqwest::Client::new();
    let post = |version: &str, method: &str, params: Value| {
        http.post(format!("{}/mcp", h.url))
            .bearer_auth(OWNER)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", version)
            .header("mcp-method", method)
            .json(&json!({"jsonrpc":"2.0","id":81,"method":method,"params":params}))
    };
    let init: Value = post("2026-07-28", "initialize", json!({"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"new-client","version":"1"}}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    let meta = |version: &str| json!({"io.modelcontextprotocol/protocolVersion":version,"io.modelcontextprotocol/clientInfo":{"name":"new-client","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}});
    let discovery: Value = post(
        "2025-11-25",
        "server/discover",
        json!({"_meta":meta("2025-11-25")}),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(
        discovery["result"]["supportedVersions"],
        json!(["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"])
    );
    for method in ["server/discover", "tools/list"] {
        let response: Value = post("2026-07-28", method, json!({"_meta":meta("2026-07-28")}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(response.get("error").is_some(), "{response}");
        assert!(response.get("result").is_none());
    }
    let unsupported_header: Value = post("2026-07-28", "tools/list", json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        unsupported_header.get("error").is_some(),
        "{unsupported_header}"
    );
    let list: Value = post("2025-11-25", "tools/list", json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 28);
    assert!(list["result"].get("resultType").is_none());
    h.close().await;
}
