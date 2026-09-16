use bog_cloud::{
    CloudService, Operation,
    config::{Config, require_free_space},
};
#[tokio::test]
async fn storage_exhaustion_is_rejected_before_provisioning() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        require_free_space(dir.path(), u64::MAX).unwrap_err().code,
        "capacity"
    );
    let mut config = Config::new(dir.path().join("r"), std::env::current_exe().unwrap());
    config.min_free_bytes = u64::MAX;
    let svc = CloudService::open(config, "owner-thirty-two-bytes-long-test-key").unwrap();
    let owner = svc
        .auth
        .authenticate("owner-thirty-two-bytes-long-test-key")
        .unwrap();
    let result = svc
        .execute(
            &owner,
            Operation::CreateBog {
                name: "a".into(),
                template: "records-v1".into(),
                idempotency_key: "a".into(),
            },
        )
        .await;
    assert_eq!(result.err().unwrap().code, "capacity");
    assert!(svc.registry.list().unwrap().is_empty());
}
