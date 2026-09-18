use bog_cloud::{CloudService, Operation, Scope, changes::ChangeWaiter, config::Config};
use std::{sync::Arc, time::Duration};
const OWNER: &str = "change-wait-owner-at-least-thirty-two-bytes";
#[tokio::test]
async fn changes_wake_timeout_reset_revoke_and_cancel() {
    let tmp = tempfile::Builder::new()
        .prefix("bc-wait-")
        .tempdir_in("/tmp")
        .unwrap();
    let worker = std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker")
        });
    let svc = CloudService::open(Config::new(tmp.path().join("r"), worker), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let id = svc
        .registry
        .create("wait", "records-v1", "wait")
        .unwrap()
        .id;
    let waiter = Arc::new(ChangeWaiter::new());
    let initial = waiter.read_cursor(&svc, &owner, id).await.unwrap();
    let timeout = waiter
        .wait(&svc, &owner, id, &initial.cursor, Duration::from_millis(40))
        .await
        .unwrap();
    assert!(!timeout.changed);
    assert!(!timeout.reset);
    assert_eq!(timeout.cursor, initial.cursor);
    let waiting = {
        let svc = svc.clone();
        let owner = owner.clone();
        let waiter = waiter.clone();
        let cursor = initial.cursor.clone();
        tokio::spawn(async move {
            waiter
                .wait(&svc, &owner, id, &cursor, Duration::from_secs(3))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(60)).await;
    svc.execute(
        &owner,
        Operation::UpsertRecord {
            bog_id: id,
            key: "hello".into(),
            data: serde_json::json!({"message":"world"}),
        },
    )
    .await
    .unwrap();
    let changed = waiting.await.unwrap().unwrap();
    assert!(changed.changed);
    assert!(!changed.reset);
    assert!(changed.seq > initial.seq);
    svc.supervisor.shutdown().await.unwrap();
    let reset = waiter
        .wait(&svc, &owner, id, &changed.cursor, Duration::ZERO)
        .await
        .unwrap();
    assert!(reset.changed && reset.reset);
    let token = svc.auth.issue(&owner, id, Scope::Read).unwrap();
    let reader = svc.auth.authenticate(&token.secret).unwrap();
    let waiting = {
        let svc = svc.clone();
        let waiter = waiter.clone();
        let cursor = reset.cursor.clone();
        tokio::spawn(async move {
            waiter
                .wait(&svc, &reader, id, &cursor, Duration::from_secs(3))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(60)).await;
    svc.auth.revoke(&owner, id, &token.id).unwrap();
    assert_eq!(waiting.await.unwrap().unwrap_err().code, "unauthorized");
    let mut waits = Vec::new();
    for _ in 0..8 {
        let svc = svc.clone();
        let waiter = waiter.clone();
        let owner = owner.clone();
        let cursor = reset.cursor.clone();
        waits.push(tokio::spawn(async move {
            waiter
                .wait(&svc, &owner, id, &cursor, Duration::from_secs(10))
                .await
        }));
    }
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        waiter
            .wait(&svc, &owner, id, &reset.cursor, Duration::ZERO)
            .await
            .unwrap_err()
            .code,
        "capacity"
    );
    assert_eq!(
        waiter.read_cursor(&svc, &owner, id).await.unwrap_err().code,
        "capacity"
    );
    assert_eq!(
        waiter
            .initialize(&svc, &owner, id, Duration::from_secs(26))
            .await
            .unwrap_err()
            .code,
        "invalid_request"
    );
    for wait in waits {
        wait.abort();
        let _ = wait.await;
    }
    assert!(
        waiter
            .wait(&svc, &owner, id, &reset.cursor, Duration::ZERO)
            .await
            .is_ok()
    );
    assert_eq!(
        svc.execute(
            &owner,
            Operation::WaitForChange {
                bog_id: id,
                cursor: None,
                timeout_seconds: 26
            }
        )
        .await
        .err()
        .unwrap()
        .code,
        "invalid_request"
    );
    svc.supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_wait_and_default_empty_queries_use_hosted_contracts() {
    use serde_json::json;
    let tmp = tempfile::Builder::new()
        .prefix("bc-resource-")
        .tempdir_in("/tmp")
        .unwrap();
    let worker = std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker")
        });
    let svc = CloudService::open(Config::new(tmp.path().join("r"), worker), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let id = svc
        .registry
        .create("resource", "records-v1", "resource")
        .unwrap()
        .id;
    let token = svc.auth.issue(&owner, id, Scope::Read).unwrap();
    let reader = svc.auth.authenticate(&token.secret).unwrap();
    let query = |resource: &str, query| Operation::QueryResource {
        bog_id: id,
        resource: resource.into(),
        query,
    };
    let table = svc
        .execute(&reader, query("docs", json!({})))
        .await
        .unwrap();
    assert_eq!(table.body["data"], json!([]));
    let count = svc
        .execute(&reader, query("total", json!({})))
        .await
        .unwrap();
    assert_eq!(count.body["data"], json!(0));
    let initial = svc
        .execute(&reader, query("docs", json!({"action":"wait","timeout":0})))
        .await
        .unwrap();
    assert!(initial.body["cursor"].is_string());
    let again = svc
        .execute(
            &reader,
            query(
                "docs",
                json!({"action":"wait","cursor":initial.body["cursor"],"timeout":0}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(again.body["changed"], false);
    assert_eq!(
        svc.execute(
            &reader,
            query("docs", json!({"action":"wait","timeout":0,"timeout_ms":0}))
        )
        .await
        .err()
        .unwrap()
        .code,
        "invalid_request"
    );
    svc.auth.revoke(&owner, id, &token.id).unwrap();
    assert_eq!(
        svc.execute(&reader, query("docs", json!({"action":"wait","timeout":0})))
            .await
            .err()
            .unwrap()
            .code,
        "unauthorized"
    );
    svc.supervisor.stop(id).await.unwrap();
}
