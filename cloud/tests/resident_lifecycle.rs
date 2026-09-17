use bog_cloud::{CloudService, DesiredState, Operation, config::Config};
use serde_json::json;
use std::time::Duration;
const OWNER: &str = "owner-lifecycle-test-thirty-two-bytes";
fn worker() -> std::path::PathBuf {
    std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker")
        })
}
#[tokio::test]
async fn idle_eviction_preserves_data_and_intent_and_never_preempts_a_lease() {
    let temp = tempfile::Builder::new()
        .prefix("bc-idle-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut config = Config::new(temp.path().join("r"), worker());
    config.max_active = 1;
    config.idle_timeout = Duration::from_millis(40);
    let svc = CloudService::open(config.clone(), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let a = svc.registry.create("a", "records-v1", "a").unwrap();
    let b = svc.registry.create("b", "records-v1", "b").unwrap();
    svc.execute(
        &owner,
        Operation::UpsertRecord {
            bog_id: a.id,
            key: "k".into(),
            data: json!({"saved":true}),
        },
    )
    .await
    .unwrap();
    let generation = svc.registry.get(a.id).unwrap().generation;
    let lease = svc.supervisor.lease(a.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert_eq!(svc.supervisor.evict_idle().await.unwrap(), 0);
    assert_eq!(
        svc.supervisor
            .ensure_running(b.id)
            .await
            .err()
            .unwrap()
            .code,
        "capacity"
    );
    drop(lease);
    assert_eq!(
        svc.supervisor.evict_idle().await.unwrap(),
        0,
        "idle starts at completion"
    );
    tokio::time::sleep(Duration::from_millis(70)).await;
    assert_eq!(svc.supervisor.evict_idle().await.unwrap(), 1);
    assert_eq!(
        svc.registry.get(a.id).unwrap().desired_state,
        DesiredState::Running
    );
    svc.supervisor.ensure_running(b.id).await.unwrap();
    assert_eq!(svc.supervisor.resident_count().await, 1);
    svc.supervisor.shutdown().await.unwrap();
    drop(svc);
    let svc = CloudService::open(config, OWNER).unwrap();
    assert!(svc.supervisor.reconcile().await.unwrap().is_empty());
    assert_eq!(svc.supervisor.resident_count().await, 0);
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let result = svc
        .execute(
            &owner,
            Operation::GetRecord {
                bog_id: a.id,
                key: "k".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.body["data"], json!({"saved":true}));
    assert!(svc.registry.get(a.id).unwrap().generation > generation);
    svc.supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn thirty_two_retained_bogs_use_at_most_eight_workers_and_deletion_is_retryable() {
    let temp = tempfile::Builder::new()
        .prefix("bc-cap-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temp.path().join("r");
    let svc = CloudService::open(Config::new(root.clone(), worker()), OWNER).unwrap();
    let mut bogs = Vec::new();
    for i in 0..32 {
        bogs.push(
            svc.registry
                .create_limited(&format!("b{i}"), "records-v1", &format!("r{i}"), 32)
                .unwrap(),
        );
    }
    assert_eq!(
        svc.registry
            .create_limited("overflow", "records-v1", "overflow", 32)
            .err()
            .unwrap()
            .code,
        "capacity"
    );
    assert!(svc.supervisor.reconcile().await.unwrap().is_empty());
    assert_eq!(svc.supervisor.resident_count().await, 0);
    for bog in &bogs[..8] {
        svc.supervisor.ensure_running(bog.id).await.unwrap();
    }
    assert_eq!(svc.supervisor.resident_count().await, 8);
    assert_eq!(
        svc.supervisor
            .ensure_running(bogs[8].id)
            .await
            .err()
            .unwrap()
            .code,
        "capacity"
    );
    let id = bogs[0].id;
    assert!(svc.supervisor.cleanup_deleted(id).await.is_err());
    // Model the separately tested transactional authorization tombstone.
    let db = rusqlite::Connection::open(root.join("registry.sqlite")).unwrap();
    db.execute(
        "UPDATE bogs SET deleted_at=1,desired_state='stopped' WHERE id=?1",
        [id.to_string()],
    )
    .unwrap();
    assert!(svc.supervisor.lease(id).await.is_err());
    svc.supervisor.cleanup_deleted(id).await.unwrap();
    svc.supervisor.cleanup_deleted(id).await.unwrap();
    assert!(!svc.supervisor.instance_dir(id).exists());
    assert!(svc.registry.pending_deletions().unwrap().is_empty());
    svc.supervisor.ensure_running(bogs[8].id).await.unwrap();
    assert_eq!(svc.supervisor.resident_count().await, 8);
    svc.supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_pre_socket_orphan_reserves_physical_capacity() {
    let temp = tempfile::Builder::new()
        .prefix("bc-orphan-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut config = Config::new(temp.path().join("r"), worker());
    config.max_active = 1;
    let svc = CloudService::open(config, OWNER).unwrap();
    let a = svc.registry.create("a", "records-v1", "a").unwrap();
    let b = svc.registry.create("b", "records-v1", "b").unwrap();
    let dir = svc.supervisor.instance_dir(a.id);
    std::fs::create_dir_all(&dir).unwrap();
    let orphan = std::fs::File::create(dir.join("worker.lock")).unwrap();
    orphan.try_lock().unwrap();
    assert!(svc.supervisor.reconcile().await.unwrap().is_empty());
    assert_eq!(
        svc.supervisor
            .ensure_running(b.id)
            .await
            .err()
            .unwrap()
            .code,
        "capacity"
    );
    assert_eq!(svc.registry.get(b.id).unwrap().generation, 0);
    drop(orphan);
    svc.supervisor.ensure_running(b.id).await.unwrap();
    svc.supervisor.shutdown().await.unwrap();
}
