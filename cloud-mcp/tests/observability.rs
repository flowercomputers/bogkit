mod common;
use common::*;
use serde_json::{Value, json};
fn data(result: rmcp::model::CallToolResult) -> Value {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result.structured_content.unwrap()["data"].clone()
}
#[tokio::test]
async fn transport_observability_contracts_validation_privacy_and_correlation() {
    let h = Harness::new().await;
    let owner = h.client(OWNER).await;
    let definitions = owner.list_all_tools().await.unwrap();
    for name in ["bog_metrics", "bog_events"] {
        let definition = definitions.iter().find(|tool| tool.name == name).unwrap();
        assert_eq!(
            definition.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        assert_eq!(definition.input_schema["additionalProperties"], false);
        assert!(definition.output_schema.is_some());
        assert!(
            definition.input_schema["properties"]
                .get("workspace_id")
                .is_some()
        );
    }
    let created = data(
        call(
            &owner,
            "create_bog",
            json!({"name":"observability", "idempotency_key":"observability"}),
        )
        .await,
    );
    let id = created["id"].as_str().unwrap();
    let bog = serde_json::from_value(created["id"].clone()).unwrap();
    ready(&h, bog).await;
    let token = data(call(&owner, "issue_token", json!({"bog_id":id,"scope":"read"})).await);
    let app = h.client(token["token"].as_str().unwrap()).await;
    let http = reqwest::Client::new();
    let rest_error: Value = http
        .get(format!("{}/v1/bogs/{id}/docs/missing", h.url))
        .bearer_auth(OWNER)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mcp_error = call(&app, "get_record", json!({"bog_id":id,"key":"missing"}))
        .await
        .structured_content
        .unwrap();
    assert!(rest_error["request_id"].is_string());
    assert!(mcp_error["request_id"].is_string());
    let tokens = data(call(&owner, "list_tokens", json!({"bog_id":id})).await);
    let observed = tokens["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == token["id"])
        .unwrap();
    assert!(
        observed["last_used_at"]
            .as_i64()
            .is_some_and(|at| at % 60 == 0)
    );
    h.service.supervisor.stop(bog).await.unwrap();
    let before = data(call(&owner, "describe_bog", json!({"bog_id":id})).await);
    let metrics = data(call(&owner, "bog_metrics", json!({"bog_id":id})).await);
    assert_eq!(metrics["window_seconds"], 3600);
    assert_eq!(metrics["scope"], "bog");
    for request_id in [&rest_error["request_id"], &mcp_error["request_id"]] {
        assert!(
            metrics["error_samples"]
                .as_array()
                .unwrap()
                .iter()
                .any(|sample| &sample["request_id"] == request_id)
        );
    }
    let own = data(call(&app, "bog_metrics", json!({"bog_id":id,"window":"5m"})).await);
    assert_eq!(own["scope"], "credential");
    assert_eq!(own["window_seconds"], 300);
    for key in ["requests", "error_samples"] {
        assert!(
            own[key]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["credential_id"] == token["id"])
        );
    }
    let denied = call(&app, "bog_events", json!({"bog_id":id})).await;
    assert_eq!(
        denied.structured_content.unwrap()["error"]["code"],
        "forbidden"
    );
    let denied = http
        .get(format!("{}/v1/bogs/{id}/events", h.url))
        .bearer_auth(token["token"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let events = data(call(&owner, "bog_events", json!({"bog_id":id,"limit":1})).await);
    assert_eq!(events["events"].as_array().unwrap().len(), 1);
    let _ = data(
        call(
            &owner,
            "bog_events",
            json!({"bog_id":id,"cursor":events["next_cursor"]}),
        )
        .await,
    );
    for query in [
        "metrics?window=2h",
        "metrics?window=1h&window=5m",
        "metrics?unexpected=1",
        "events?limit=0",
        "events?limit=101",
        "events?limit=no",
        "events?limit=1&limit=2",
        "events?cursor=bad",
        "events?unexpected=1",
        "events?cursor=a&cursor=b",
    ] {
        let response = http
            .get(format!("{}/v1/bogs/{id}/{query}", h.url))
            .bearer_auth(OWNER)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{query}");
    }
    for (name, args) in [
        ("bog_metrics", json!({"bog_id":id,"window":"2h"})),
        ("bog_events", json!({"bog_id":id,"limit":0})),
        ("bog_events", json!({"bog_id":id,"limit":101})),
        ("bog_metrics", json!({"bog_id":id,"unexpected":true})),
    ] {
        assert!(
            owner
                .call_tool(
                    rmcp::model::CallToolRequestParams::new(name)
                        .with_arguments(args.as_object().unwrap().clone())
                )
                .await
                .is_err()
        );
    }
    let workspace = h
        .service
        .authenticate_bearer(OWNER, None)
        .await
        .unwrap()
        .workspace_id()
        .unwrap();
    for endpoint in ["metrics", "events"] {
        let response = http
            .get(format!(
                "{}/v1/bogs/{id}/{endpoint}?workspace_id={workspace}&workspace_id={workspace}",
                h.url
            ))
            .bearer_auth(OWNER)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    for (path, tool) in [("metrics", "bog_metrics"), ("events", "bog_events")] {
        let response: Value = http
            .get(format!("{}/v1/bogs/{id}/{path}", h.url))
            .bearer_auth(OWNER)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        let definition = bog_cloud_mcp::tools::definitions()
            .into_iter()
            .find(|d| d.name == tool)
            .unwrap();
        let schema = serde_json::to_value(definition.output_schema.unwrap()).unwrap();
        assert!(jsonschema::is_valid(
            &schema["properties"]["data"],
            &response
        ));
    }
    let after = data(call(&owner, "describe_bog", json!({"bog_id":id})).await);
    assert_eq!(before["status"], "stopped");
    assert_eq!(after["status"], before["status"]);
    assert_eq!(after["generation"], before["generation"]);
    app.cancel().await.unwrap();
    owner.cancel().await.unwrap();
    h.close().await;
}
