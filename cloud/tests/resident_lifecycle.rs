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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match svc.supervisor.ensure_running(b.id).await {
            Ok(_) => break,
            Err(error) => {
                assert_eq!(error.code, "capacity");
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    svc.supervisor.shutdown().await.unwrap();
}

struct Orphan(std::process::Child);
impl Drop for Orphan {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn pre_socket_orphan(svc: &CloudService, id: bog_cloud::BogId) -> Orphan {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let dir = svc.supervisor.instance_dir(id);
    std::fs::create_dir_all(&dir).unwrap();
    let lock = std::fs::File::create(dir.join("worker.lock")).unwrap();
    lock.try_lock().unwrap();
    svc.registry
        .start_generation(id, "survivor-test-nonce")
        .unwrap();
    let fd = lock.as_raw_fd();
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "kill -STOP $$; exec \"$@\"", "survivor"])
        .arg(worker())
        .arg("--data-dir")
        .arg(dir.join("data"))
        .arg("--socket")
        .arg(dir.join("worker.sock"))
        .args(["--template-version", "records-v1"])
        .env("BOG_INSTANCE_ID", id.to_string())
        .env("BOG_STARTUP_NONCE", "survivor-test-nonce")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(child.id() as i32, &mut status, libc::WUNTRACED) },
        child.id() as i32
    );
    assert!(libc::WIFSTOPPED(status));
    Orphan(child)
}
async fn wait_for_exit(orphan: &mut Orphan) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while orphan.0.try_wait().unwrap().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "survivor remained alive"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
#[tokio::test]
async fn maintenance_recovers_pre_socket_survivor_then_idles_it_without_a_request() {
    let temp = tempfile::Builder::new()
        .prefix("bc-late-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut config = Config::new(temp.path().join("r"), worker());
    config.max_active = 1;
    config.idle_timeout = Duration::from_millis(80);
    let svc = CloudService::open(config.clone(), OWNER).unwrap();
    let a = svc.registry.create("a", "records-v1", "a").unwrap();
    let b = svc.registry.create("b", "records-v1", "b").unwrap();
    let mut orphan = pre_socket_orphan(&svc, a.id);
    drop(svc);
    let svc = CloudService::open(config, OWNER).unwrap();
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
    let maintenance = svc.supervisor.spawn_maintenance();
    assert_eq!(
        unsafe { libc::kill(orphan.0.id() as i32, libc::SIGCONT) },
        0
    );
    wait_for_exit(&mut orphan).await;
    assert_eq!(svc.registry.get(a.id).unwrap().generation, 1);
    assert_eq!(
        svc.registry.get(a.id).unwrap().desired_state,
        DesiredState::Running
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match svc.supervisor.ensure_running(b.id).await {
            Ok(_) => break,
            Err(error) => {
                assert_eq!(error.code, "capacity");
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "idle shutdown did not release capacity"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    maintenance.abort();
    let _ = maintenance.await;
    svc.supervisor.shutdown().await.unwrap();
}
#[tokio::test]
async fn stopped_intent_survivors_recover_through_maintenance_and_explicit_retry() {
    for automatic in [true, false] {
        let temp = tempfile::Builder::new()
            .prefix("bc-stop-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut config = Config::new(temp.path().join("r"), worker());
        config.idle_timeout = Duration::from_millis(80);
        let svc = CloudService::open(config.clone(), OWNER).unwrap();
        let bog = svc.registry.create("a", "records-v1", "a").unwrap();
        let mut orphan = pre_socket_orphan(&svc, bog.id);
        svc.registry.set_desired(bog.id, false).unwrap(); // Crash between persisting stop intent and shutdown.
        drop(svc);
        let svc = CloudService::open(config, OWNER).unwrap();
        assert_eq!(svc.supervisor.reconcile().await.unwrap().len(), 1);
        assert!(svc.supervisor.stop(bog.id).await.is_err());
        assert_eq!(
            unsafe { libc::kill(orphan.0.id() as i32, libc::SIGCONT) },
            0
        );
        if automatic {
            let maintenance = svc.supervisor.spawn_maintenance();
            wait_for_exit(&mut orphan).await;
            maintenance.abort();
            let _ = maintenance.await;
        } else {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while svc.supervisor.stop(bog.id).await.is_err() {
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            wait_for_exit(&mut orphan).await;
        }
        assert_eq!(
            svc.registry.get(bog.id).unwrap().desired_state,
            DesiredState::Stopped
        );
        assert_eq!(svc.registry.get(bog.id).unwrap().generation, 1);
        assert_eq!(svc.supervisor.resident_count().await, 0);
    }
}

#[tokio::test]
async fn a_held_response_keeps_its_generation_when_another_lease_restarts_the_worker() {
    let temp = tempfile::Builder::new()
        .prefix("bc-gen-")
        .tempdir_in("/tmp")
        .unwrap();
    let svc = CloudService::open(Config::new(temp.path().join("r"), worker()), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let bog = svc.registry.create("a", "records-v1", "a").unwrap();
    let old = svc.supervisor.lease(bog.id).await.unwrap();
    let (_, identity) = old
        .client
        .request(reqwest::Method::GET, "/_cloud/identity", None)
        .await
        .unwrap();
    let (_, response) = old
        .client
        .request(reqwest::Method::GET, "/views/total", None)
        .await
        .unwrap();
    // Keep the earlier response and its read lease alive while a concurrent
    // reader detects the crash and replaces the worker before cursor tagging.
    let old_generation = old.generation;
    assert_eq!(
        unsafe { libc::kill(identity["pid"].as_i64().unwrap() as i32, libc::SIGKILL) },
        0
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let new = svc.supervisor.lease(bog.id).await.unwrap();
    assert!(new.generation > old_generation);
    assert_eq!(old.generation, old_generation);
    let (_, current) = new
        .client
        .request(reqwest::Method::GET, "/views/total", None)
        .await
        .unwrap();
    assert_eq!(
        response["seq"], current["seq"],
        "same sequence still requires generation reset"
    );
    let old_cursor = bog_cloud::changes::cursor_for_response(
        bog.id,
        old.generation,
        response["seq"].as_u64().unwrap(),
    );
    let new_cursor = bog_cloud::changes::cursor_for_response(
        bog.id,
        new.generation,
        current["seq"].as_u64().unwrap(),
    );
    assert_ne!(old_cursor, new_cursor);
    drop(new);
    drop(old);
    let result = svc
        .execute(
            &owner,
            Operation::WaitForChange {
                bog_id: bog.id,
                cursor: Some(old_cursor),
                timeout_seconds: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.body["reset"], true);
    assert_eq!(result.body["cursor"], new_cursor);
    svc.supervisor.shutdown().await.unwrap();
}
