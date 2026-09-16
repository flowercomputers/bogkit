use bog_cloud::{CloudService, Operation, Scope, config::Config};
const OWNER: &str = "test-owner-secret-at-least-thirty-two-bytes";
#[tokio::test]
async fn resource_management_authorizes_and_preserves_retry_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(tmp.path().join("root"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let result = svc.execute(&owner, Operation::ListBogs).await.unwrap();
    assert_eq!(result.body["bogs"], serde_json::json!([]));
    let bog = svc.registry.create("alpha", "records-v1", "alpha").unwrap();
    let issued = svc.auth.issue(&owner, bog.id, Scope::Read).unwrap();
    let reader = svc.auth.authenticate(&issued.secret).unwrap();
    assert_eq!(
        svc.execute(&reader, Operation::ListBogs)
            .await
            .err()
            .unwrap()
            .code,
        "forbidden"
    );
    assert_eq!(
        svc.execute(
            &reader,
            Operation::UpsertRecord {
                bog_id: bog.id,
                key: "a".into(),
                data: serde_json::json!({})
            }
        )
        .await
        .err()
        .unwrap()
        .code,
        "forbidden"
    );
    let described = svc
        .execute(&reader, Operation::DescribeBog { bog_id: bog.id })
        .await
        .unwrap();
    assert_eq!(described.body["id"], bog.id.to_string());
    svc.auth.revoke(&owner, bog.id, &issued.id).unwrap();
    assert_eq!(
        svc.execute(&reader, Operation::DescribeBog { bog_id: bog.id })
            .await
            .err()
            .unwrap()
            .code,
        "unauthorized"
    );
}

#[test]
fn unsafe_root_is_rejected_before_registry_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(
        CloudService::open(Config::new(link, std::env::current_exe().unwrap()), OWNER).is_err()
    );
    assert!(!target.join("registry.sqlite").exists());
}

#[tokio::test]
async fn invalid_documents_and_batches_are_rejected_before_starting_worker() {
    let dir = tempfile::tempdir().unwrap();
    let svc = CloudService::open(
        Config::new(dir.path().join("r"), dir.path().join("missing")),
        OWNER,
    )
    .unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let id = svc.registry.create("a", "records-v1", "a").unwrap().id;
    for data in [
        serde_json::json!(null),
        serde_json::json!({"x":"x".repeat(256*1024)}),
    ] {
        assert_eq!(
            svc.execute(
                &owner,
                Operation::UpsertRecord {
                    bog_id: id,
                    key: "k".into(),
                    data
                }
            )
            .await
            .err()
            .unwrap()
            .code,
            "invalid_request"
        );
    }
    for operations in [
        serde_json::json!({}),
        serde_json::json!([{"op":"upsert","key":"k","data":null}]),
        serde_json::json!(vec![serde_json::json!({"op":"remove","key":"a"}); 101]),
    ] {
        assert_eq!(
            svc.execute(
                &owner,
                Operation::Batch {
                    bog_id: id,
                    operations
                }
            )
            .await
            .err()
            .unwrap()
            .code,
            "invalid_request"
        );
    }
    assert_eq!(svc.registry.get(id).unwrap().generation, 0);
}
