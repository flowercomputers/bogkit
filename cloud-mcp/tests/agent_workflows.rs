mod common;
use common::*;
use serde_json::json;

#[tokio::test]
async fn provisioning_routes_and_request_lookup_share_http_contract() {
    let h = Harness::composable().await;
    let client = h.client(OWNER).await;
    let created = call(
        &client,
        "create_bog",
        json!({"name":"agent-workflow","idempotency_key":"workflow","wait":true}),
    )
    .await;
    assert_ne!(created.is_error, Some(true), "{created:?}");
    let data = created.structured_content.unwrap()["data"].clone();
    assert_eq!(data["status"], "ready");
    let id = data["id"].as_str().unwrap();
    let routes = call(&client, "list_routes", json!({"bog_id":id})).await;
    let routes_data = routes.structured_content.unwrap()["data"].clone();
    let http = reqwest::Client::new();
    let rest: serde_json::Value = http
        .get(format!("{}/v1/bogs/{id}/routes", h.url))
        .bearer_auth(OWNER)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(routes_data["routes"], rest["routes"]);
    call(
        &client,
        "upsert_record",
        json!({"bog_id":id,"key":"a","data":{"title":"visible","private":"hidden"}}),
    )
    .await;
    call(
        &client,
        "upsert_record",
        json!({"bog_id":id,"key":"b","data":{"title":"second"}}),
    )
    .await;
    let batch = call(&client,"query_resource",json!({"bog_id":id,"resource":"docs","query":{"action":"batch_get","keys":["a","missing"]}})).await;
    assert_ne!(batch.is_error, Some(true), "{batch:?}");
    let rows = batch.structured_content.unwrap()["data"]["data"].clone();
    assert_eq!(rows[0]["key"], "a");
    assert!(rows[1]["value"].is_null());
    let page = call(
        &client,
        "query_resource",
        json!({"bog_id":id,"resource":"docs","query":{"action":"list","after":"a","before":"z"}}),
    )
    .await;
    assert_ne!(page.is_error, Some(true), "{page:?}");
    let rows = page.structured_content.unwrap()["data"]["data"].clone();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["key"], "b");
    let read = call(&client, "list_routes", json!({"bog_id":id})).await;
    let request_id = read.structured_content.unwrap()["request_id"].clone();
    let info = call(&client, "request_info", json!({"request_id":request_id})).await;
    let info = info.structured_content.unwrap();
    assert_eq!(info["data"]["request_id"], request_id);
    assert_eq!(info["data"]["bog_id"], id);
    assert_eq!(info["data"]["operation"], "list_routes");
    client.cancel().await.unwrap();
    h.close().await;
}

#[tokio::test]
async fn new_workflow_tools_preserve_single_bog_app_permissions() {
    let h = Harness::composable().await;
    let owner = h.service.auth.authenticate(OWNER).unwrap();
    let a = h
        .service
        .registry
        .create("workflow-a", "records-v1", "a")
        .unwrap();
    let b = h
        .service
        .registry
        .create("workflow-b", "records-v1", "b")
        .unwrap();
    let token = h
        .service
        .auth
        .issue(&owner, a.id, bog_cloud::Scope::Read)
        .unwrap();
    let client = h.client(&token.secret).await;
    for (name, args) in [
        (
            "create_bog",
            json!({"name":"sandbox","idempotency_key":"sandbox","wait":true,"sandbox":true,"app_access":{"scope":"read"}}),
        ),
        ("preview_cleanup", json!({"prefix":"workflow"})),
        ("execute_cleanup", json!({"preview_id":"never-created"})),
        ("list_routes", json!({"bog_id":b.id})),
    ] {
        assert_eq!(
            call(&client, name, args).await.is_error,
            Some(true),
            "{name}"
        );
    }
    assert_ne!(
        call(&client, "list_routes", json!({"bog_id":a.id}))
            .await
            .is_error,
        Some(true)
    );
    // The lookup is tied to the calling credential, even within the same Bog.
    let observed = call(&client, "describe_bog", json!({"bog_id":a.id})).await;
    let request_id = observed.structured_content.unwrap()["request_id"].clone();
    let own = call(&client, "request_info", json!({"request_id":request_id})).await;
    assert_ne!(own.is_error, Some(true), "{own:?}");
    assert_eq!(
        own.structured_content.unwrap()["data"]["request_id"],
        request_id
    );
    let other_token = h
        .service
        .auth
        .issue(&owner, a.id, bog_cloud::Scope::Read)
        .unwrap();
    let other = h.client(&other_token.secret).await;
    let denied = call(&other, "request_info", json!({"request_id":request_id})).await;
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(
        denied.structured_content.unwrap()["error"]["code"],
        "not_found"
    );
    other.cancel().await.unwrap();
    client.cancel().await.unwrap();
    h.close().await;
}
