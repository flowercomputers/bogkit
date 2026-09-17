use bog_cloud::{CloudService, Operation, config::Config};
use serde_json::json;
const OWNER: &str = "owner-test-secret-more-than-thirty-two-bytes";
#[tokio::test]
async fn closed_store_backup_restores_independently_and_detects_corruption() {
    let dir = tempfile::Builder::new()
        .prefix("bc-")
        .tempdir_in("/tmp")
        .unwrap();
    let worker = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/bog-records-worker");
    let svc = CloudService::open(Config::new(dir.path().join("r"), worker), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let original = svc
        .registry
        .create("original", "records-v1", "original")
        .unwrap();
    let operations = json!([{"op":"upsert","key":"a","data":{"nested":{"v":2}}},{"op":"upsert","key":"b","data":{"value":1}},{"op":"upsert","key":"b","data":{"value":3}}]);
    svc.execute(
        &owner,
        Operation::Batch {
            bog_id: original.id,
            operations,
        },
    )
    .await
    .unwrap();
    let archive = svc.supervisor.backup(original.id).await.unwrap();
    assert_eq!(
        svc.registry.get(original.id).unwrap().status,
        bog_cloud::ObservedState::Ready
    );
    let restored = svc.supervisor.restore(&archive, "restored").await.unwrap();
    assert_ne!(restored.id, original.id);
    for view in ["docs", "total"] {
        let a = svc
            .execute(
                &owner,
                Operation::ReadView {
                    bog_id: original.id,
                    view: view.into(),
                    limit: None,
                    offset: None,
                },
            )
            .await
            .unwrap()
            .body;
        let b = svc
            .execute(
                &owner,
                Operation::ReadView {
                    bog_id: restored.id,
                    view: view.into(),
                    limit: None,
                    offset: None,
                },
            )
            .await
            .unwrap()
            .body;
        assert_eq!(a["data"], b["data"]);
        assert_eq!(a["seq"], b["seq"]);
        assert_ne!(
            a["cursor"], b["cursor"],
            "restore has a separate cursor identity"
        );
    }
    svc.execute(
        &owner,
        Operation::UpsertRecord {
            bog_id: restored.id,
            key: "c".into(),
            data: json!({}),
        },
    )
    .await
    .unwrap();
    assert!(
        svc.execute(
            &owner,
            Operation::GetRecord {
                bog_id: original.id,
                key: "c".into()
            }
        )
        .await
        .is_err()
    );
    std::fs::write(
        dir.path()
            .join("r/backups")
            .join(&archive)
            .join("data.schema"),
        b"corrupted",
    )
    .unwrap();
    assert!(svc.supervisor.restore(&archive, "bad").await.is_err());
    assert_eq!(svc.registry.list().unwrap().len(), 2);
    svc.supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_backup_resumes_source_and_interrupted_restore_stays_failed() {
    let dir = tempfile::Builder::new()
        .prefix("bc-")
        .tempdir_in("/tmp")
        .unwrap();
    let worker = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/bog-records-worker");
    let config = Config::new(dir.path().join("r"), worker);
    let svc = CloudService::open(config.clone(), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let original = svc
        .registry
        .create("original", "records-v1", "original")
        .unwrap();
    svc.execute(
        &owner,
        Operation::UpsertRecord {
            bog_id: original.id,
            key: "a".into(),
            data: json!({"v":1}),
        },
    )
    .await
    .unwrap();
    std::fs::write(config.root.join("backups"), b"block-copy").unwrap();
    assert!(svc.supervisor.backup(original.id).await.is_err());
    assert_eq!(
        svc.registry.get(original.id).unwrap().status,
        bog_cloud::ObservedState::Ready
    );
    assert_eq!(
        svc.execute(
            &owner,
            Operation::GetRecord {
                bog_id: original.id,
                key: "a".into()
            }
        )
        .await
        .unwrap()
        .body["data"],
        json!({"v":1})
    );
    svc.supervisor.shutdown().await.unwrap();
    svc.registry
        .set_status(original.id, bog_cloud::ObservedState::Maintenance, None)
        .unwrap();
    let incomplete = svc
        .registry
        .create("incomplete", "records-v1", "incomplete")
        .unwrap();
    svc.registry
        .set_status(incomplete.id, bog_cloud::ObservedState::Restoring, None)
        .unwrap();
    drop(svc);
    let svc = CloudService::open(config, OWNER).unwrap();
    assert!(svc.supervisor.reconcile().await.unwrap().is_empty());
    assert_eq!(
        svc.registry.get(original.id).unwrap().status,
        bog_cloud::ObservedState::Stopped
    );
    assert_eq!(
        svc.registry.get(original.id).unwrap().desired_state,
        bog_cloud::DesiredState::Running
    );
    svc.supervisor.ensure_running(original.id).await.unwrap();
    assert_eq!(
        svc.registry.get(original.id).unwrap().status,
        bog_cloud::ObservedState::Ready
    );
    let failed = svc.registry.get(incomplete.id).unwrap();
    assert_eq!(failed.status, bog_cloud::ObservedState::Failed);
    assert_eq!(failed.desired_state, bog_cloud::DesiredState::Stopped);
    svc.supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn backup_adopts_and_closes_surviving_worker_and_recovers_staging() {
    let dir = tempfile::Builder::new()
        .prefix("bc-")
        .tempdir_in("/tmp")
        .unwrap();
    let worker = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/bog-records-worker");
    let config = Config::new(dir.path().join("r"), worker.clone());
    let svc = CloudService::open(config.clone(), OWNER).unwrap();
    let bog = svc
        .registry
        .create("orphan", "records-v1", "orphan")
        .unwrap();
    svc.registry.start_generation(bog.id, "test-nonce").unwrap();
    let instance = svc.supervisor.instance_dir(bog.id);
    std::fs::create_dir(&instance).unwrap();
    let socket = instance.join("worker.sock");
    let mut child = tokio::process::Command::new(worker)
        .args([
            "--data-dir",
            instance.join("data").to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--template-version",
            "records-v1",
        ])
        .env("BOG_INSTANCE_ID", bog.id.to_string())
        .env("BOG_STARTUP_NONCE", "test-nonce")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = bog_cloud::worker_client::WorkerClient::new(&socket).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if client
            .request(reqwest::Method::GET, "/_cloud/identity", None)
            .await
            .is_ok()
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    assert_eq!(
        client
            .request(
                reqwest::Method::PUT,
                "/docs/a",
                Some(json!({"survived":true}))
            )
            .await
            .unwrap()
            .0,
        200
    );
    let archive = svc.supervisor.backup(bog.id).await.unwrap();
    assert!(child.wait().await.unwrap().success());
    assert!(config.root.join("backups").join(archive).is_dir());
    svc.supervisor.shutdown().await.unwrap();
    drop(svc);
    let stage = config
        .root
        .join("backups")
        .join(format!(".staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("partial"), b"partial").unwrap();
    let svc = CloudService::open(config, OWNER).unwrap();
    svc.supervisor.reconcile().await.unwrap();
    assert!(!stage.exists());
    svc.supervisor.shutdown().await.unwrap();
}
