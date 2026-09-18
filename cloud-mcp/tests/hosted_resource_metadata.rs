use bog_cloud::{CloudService, Operation, config::Config};
use serde_json::json;

#[tokio::test]
async fn hosted_mutations_match_published_resource_schemas() {
    let root = tempfile::Builder::new()
        .prefix("bc-meta-")
        .tempdir_in("/tmp")
        .unwrap();
    let worker = std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker")
        });
    let secret = "metadata-owner-secret-at-least-thirty-two-bytes";
    let service = CloudService::open(Config::new(root.path().join("r"), worker), secret).unwrap();
    let owner = service.auth.authenticate(secret).unwrap();
    let id = service
        .registry
        .create("metadata", "records-v1", "metadata")
        .unwrap()
        .id;
    let metadata = service
        .execute(&owner, Operation::ListResources { bog_id: id })
        .await
        .unwrap()
        .body;
    for (action, operation) in [
        (
            "put",
            Operation::UpsertRecord {
                bog_id: id,
                key: "a".into(),
                data: json!({"x":1}),
            },
        ),
        (
            "remove",
            Operation::DeleteRecord {
                bog_id: id,
                key: "a".into(),
            },
        ),
        (
            "batch",
            Operation::Batch {
                bog_id: id,
                operations: json!({"ops":[{"op":"upsert","key":"b","data":{}}]}),
            },
        ),
    ] {
        let entry = metadata["resources"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| r["operations"].as_array().unwrap())
            .find(|o| o["action"] == action)
            .unwrap();
        assert_eq!(entry["hosted"]["response_envelope"], "none");
        let response = service.execute(&owner, operation).await.unwrap().body;
        jsonschema::validator_for(&entry["hosted"]["response_schema"])
            .unwrap()
            .validate(&response)
            .unwrap();
    }
    for (action, resource, query) in [
        ("list", "docs", json!({})),
        ("read", "total", json!({})),
        ("wait", "docs", json!({"action":"wait","timeout":0})),
    ] {
        let entry = metadata["resources"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| r["operations"].as_array().unwrap())
            .find(|o| o["action"] == action)
            .unwrap();
        let response = service
            .execute(
                &owner,
                Operation::QueryResource {
                    bog_id: id,
                    resource: resource.into(),
                    query,
                },
            )
            .await
            .unwrap()
            .body;
        jsonschema::validator_for(&entry["hosted"]["response_schema"])
            .unwrap()
            .validate(&response)
            .unwrap();
    }
    service.supervisor.stop(id).await.unwrap();
}
