use bog_cloud::{CloudService, Operation, Scope, config::Config};
use serde_json::json;
const OWNER: &str = "test-owner-secret-at-least-thirty-two-bytes";

#[tokio::test]
async fn scoped_context_and_compact_routes_preserve_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(dir.path().join("r"), dir.path().join("missing")),
        OWNER,
    )
    .unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let bog = svc.registry.create("notes", "records-v1", "notes").unwrap();
    let issued = svc.auth.issue(&owner, bog.id, Scope::Read).unwrap();
    let reader = svc.auth.authenticate(&issued.secret).unwrap();
    let context = svc
        .execute(&reader, Operation::GetCurrentContext)
        .await
        .unwrap()
        .body;
    assert_eq!(context["credential"]["scope"], "read");
    assert_eq!(context["credential"]["bog_id"], bog.id.to_string());
    assert!(context["credential"]["expires_at"].is_number());
    assert_eq!(context["workspaces"], json!([]));
    assert!(context["allowance"].is_null());
    let routes = svc
        .execute(&reader, Operation::ListRoutes { bog_id: bog.id })
        .await
        .unwrap()
        .body;
    assert!(routes.to_string().len() < 2048);
    assert!(
        routes["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["body_example"]["action"] == "batch_get")
    );
    let foreign = svc
        .registry
        .create("foreign", "records-v1", "foreign")
        .unwrap();
    assert!(
        svc.execute(&reader, Operation::ListRoutes { bog_id: foreign.id })
            .await
            .is_err()
    );
    // Invalid template arguments fail before a worker can start.
    for query in [
        json!({"action":"get","key":"a","keys":["b"]}),
        json!({"action":"batch_get","key":"a"}),
        json!({"action":"list","after":"a","offset":0}),
        json!({"action":"list","keys":["a"]}),
    ] {
        let error = svc
            .execute(
                &reader,
                Operation::QueryResource {
                    bog_id: bog.id,
                    resource: "docs".into(),
                    query,
                },
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "invalid_request");
    }
}

#[tokio::test]
async fn optional_creation_rejects_invalid_access_before_provisioning() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(dir.path().join("r"), dir.path().join("missing")),
        OWNER,
    )
    .unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let err = svc
        .execute(
            &owner,
            Operation::ProvisionBog {
                name: "invalid".into(),
                template: None,
                definition: None,
                idempotency_key: "invalid".into(),
                wait: false,
                sandbox: false,
                app_access: Some(json!({"label":""})),
            },
        )
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, "invalid_request");
    assert!(svc.registry.list().unwrap().is_empty());
    let err = svc
        .execute(
            &owner,
            Operation::ProvisionBog {
                name: "sandbox".into(),
                template: None,
                definition: None,
                idempotency_key: "sandbox".into(),
                wait: false,
                sandbox: true,
                app_access: None,
            },
        )
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, "feature_disabled");
    assert!(svc.registry.list().unwrap().is_empty());
}
