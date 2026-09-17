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

#[tokio::test]
async fn public_legacy_workspace_keeps_configured_creation_allowance() {
    for maximum in [5, 8] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::new(
            dir.path().join("r"),
            std::path::PathBuf::from("/nonexistent-test-worker"),
        );
        config.max_active = maximum;
        let owner = "owner-thirty-two-bytes-long-test-key";
        let svc = CloudService::open(config, owner).unwrap();
        let p = svc.authenticate_bearer(owner, None).await.unwrap();
        assert_eq!(p.workspace_id(), Some(bog_cloud::WorkspaceId::legacy()));
        for i in 0..maximum {
            svc.execute(
                &p,
                Operation::CreateBog {
                    name: format!("db{i}"),
                    template: "records-v1".into(),
                    idempotency_key: format!("db{i}"),
                },
            )
            .await
            .unwrap();
        }
        let retry = svc
            .execute(
                &p,
                Operation::CreateBog {
                    name: "db0".into(),
                    template: "records-v1".into(),
                    idempotency_key: "db0".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(retry.status, 202);
        assert_eq!(svc.registry.list().unwrap().len(), maximum);
        let result = svc
            .execute(
                &p,
                Operation::CreateBog {
                    name: "overflow".into(),
                    template: "records-v1".into(),
                    idempotency_key: "overflow".into(),
                },
            )
            .await;
        assert_eq!(result.err().unwrap().code, "capacity");
        svc.supervisor.shutdown().await.unwrap();
    }
}
