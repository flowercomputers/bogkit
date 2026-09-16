use bog_cloud::{CloudService, Operation, Scope, config::Config};
use serde_json::json;
const OWNER: &str = "owner-integration-test-thirty-two-bytes";
fn worker() -> std::path::PathBuf {
    std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker")
        })
}
#[tokio::test]
async fn independent_workers_crash_recovery_and_manager_restart_preserve_records() {
    assert!(
        worker().exists(),
        "build bog-records-worker before running process tests"
    );
    let temp = tempfile::Builder::new()
        .prefix("bc-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temp.path().join("r");
    let svc = CloudService::open(Config::new(root.clone(), worker()), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let a = svc.registry.create("a", "records-v1", "a").unwrap();
    let b = svc.registry.create("b", "records-v1", "b").unwrap();
    let key = "nested 😀?#%".to_string();
    let document = json!({"nested":{"list":[null,1,true,"hi"]},"n":9});
    for bog in [&a, &b] {
        svc.supervisor.ensure_running(bog.id).await.unwrap();
    }
    svc.execute(
        &owner,
        Operation::UpsertRecord {
            bog_id: a.id,
            key: key.clone(),
            data: document.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        svc.execute(
            &owner,
            Operation::GetRecord {
                bog_id: b.id,
                key: key.clone()
            }
        )
        .await
        .err()
        .unwrap()
        .code,
        "not_found"
    );
    let lease = svc.supervisor.lease(a.id).await.unwrap();
    let (_, identity) = lease
        .client
        .request(reqwest::Method::GET, "/_cloud/identity", None)
        .await
        .unwrap();
    let pid = identity["pid"].as_i64().unwrap() as i32;
    drop(lease);
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let result = svc
        .execute(
            &owner,
            Operation::GetRecord {
                bog_id: a.id,
                key: key.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.body["data"], document);
    assert!(svc.registry.get(a.id).unwrap().generation >= 2);
    let token = svc.auth.issue(&owner, a.id, Scope::Read).unwrap();
    svc.supervisor.shutdown().await.unwrap();
    drop(svc);
    let svc = CloudService::open(Config::new(root, worker()), OWNER).unwrap();
    assert!(svc.supervisor.reconcile().await.unwrap().is_empty());
    let reader = svc.auth.authenticate(&token.secret).unwrap();
    assert_eq!(
        svc.execute(&reader, Operation::GetRecord { bog_id: a.id, key })
            .await
            .unwrap()
            .body["data"],
        document
    );
    svc.supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn slow_start_does_not_block_an_existing_database_and_spawn_failure_is_visible() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::Builder::new()
        .prefix("bc-")
        .tempdir_in("/tmp")
        .unwrap();
    let wrapper = temp.path().join("worker");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n/bin/sleep 1\nexec '{}' \"$@\"\n",
            worker().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let svc = CloudService::open(Config::new(temp.path().join("r"), wrapper), OWNER).unwrap();
    let a = svc.registry.create("a", "records-v1", "a").unwrap();
    let b = svc.registry.create("b", "records-v1", "b").unwrap();
    svc.supervisor.ensure_running(a.id).await.unwrap();
    let supervisor = svc.supervisor.clone();
    let pending = tokio::spawn(async move { supervisor.ensure_running(b.id).await });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    tokio::time::timeout(
        std::time::Duration::from_millis(400),
        svc.supervisor.lease(a.id),
    )
    .await
    .expect("healthy database blocked by another startup")
    .unwrap();
    pending.await.unwrap().unwrap();
    svc.supervisor.shutdown().await.unwrap();
    drop(svc);
    let svc = CloudService::open(
        Config::new(
            temp.path().join("failure"),
            temp.path().join("missing-binary"),
        ),
        OWNER,
    )
    .unwrap();
    let b = svc.registry.create("b", "records-v1", "b").unwrap();
    assert_eq!(svc.supervisor.reconcile().await.unwrap().len(), 1);
    let b = svc.registry.get(b.id).unwrap();
    assert_eq!(b.status, bog_cloud::ObservedState::Failed);
    assert_eq!(b.generation, 3);
}
