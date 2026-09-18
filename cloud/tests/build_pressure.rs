use bog_cloud::{CloudService, Operation, config::Config};
use serde_json::json;
use std::time::Duration;
const OWNER: &str = "build-pressure-owner-over-thirty-two-bytes";
fn config(root: std::path::PathBuf, slots: usize) -> Config {
    let worker = std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/debug/bog-records-worker")
        });
    let mut c = Config::new(root, worker);
    c.max_active = slots;
    c.composable_enabled = true;
    c
}
#[tokio::test]
async fn pressure_preserves_every_protected_build_state() {
    for state in ["building", "activating", "recovery_required"] {
        let temp = tempfile::Builder::new()
            .prefix("bc-bp-")
            .tempdir_in("/tmp")
            .unwrap();
        let svc = CloudService::open(config(temp.path().join("r"), 1), OWNER).unwrap();
        let a = svc.registry.create("a", "records-v1", "a").unwrap();
        let b = svc.registry.create("b", "records-v1", "b").unwrap();
        svc.supervisor.ensure_running(a.id).await.unwrap();
        let generation = svc.registry.get(a.id).unwrap().generation;
        let db = rusqlite::Connection::open(temp.path().join("r/registry.sqlite")).unwrap();
        db.execute("INSERT INTO definition_jobs(id,bog_id,status,payload,created_at) VALUES ('job',?1,?2,'{}',0)", rusqlite::params![a.id.to_string(),state]).unwrap();
        let result = svc.supervisor.ensure_running(b.id).await;
        assert_eq!(
            result
                .err()
                .expect("protected source must not be reclaimed")
                .code,
            "capacity"
        );
        assert_eq!(svc.registry.get(a.id).unwrap().generation, generation);
        svc.supervisor.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn builds_reclaim_warm_capacity_with_awake_and_sleeping_sources() {
    for asleep in [false, true] {
        let temp = tempfile::Builder::new()
            .prefix("bc-bp-")
            .tempdir_in("/tmp")
            .unwrap();
        let svc = CloudService::open(config(temp.path().join("r"), 2), OWNER).unwrap();
        let owner = svc.auth.authenticate(OWNER).unwrap();
        let a = svc.registry.create("a", "records-v1", "a").unwrap();
        svc.execute(
            &owner,
            Operation::UpsertRecord {
                bog_id: a.id,
                key: "saved".into(),
                data: json!({"value":42}),
            },
        )
        .await
        .unwrap();
        for i in 0..if asleep { 2 } else { 1 } {
            let b = svc
                .registry
                .create(&format!("b{i}"), "records-v1", &format!("b{i}"))
                .unwrap();
            svc.supervisor.ensure_running(b.id).await.unwrap();
        }
        let active = svc.registry.definition(a.id).unwrap();
        let mut definition = serde_json::to_value(active.definition).unwrap();
        definition["resources"]["extra"] = json!({"terminal":{"kind":"count"}});
        let result = svc
            .execute(
                &owner,
                Operation::ApplyDefinitionUpdate {
                    bog_id: a.id,
                    definition,
                    expected_revision: active.revision,
                },
            )
            .await
            .expect("warm capacity should be reclaimable");
        let job = result.body["job_id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                assert!(svc.supervisor.resident_count().await <= 2);
                let status = svc.registry.definition_job(a.id, job).unwrap();
                if status["status"] == "succeeded" {
                    break;
                }
                assert!(
                    matches!(status["status"].as_str(), Some("building" | "activating")),
                    "{status}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let record = svc
            .execute(
                &owner,
                Operation::GetRecord {
                    bog_id: a.id,
                    key: "saved".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(record.body["data"], json!({"value":42}));
        svc.supervisor.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn builds_do_not_preempt_active_leases_or_leak_capacity_on_rejection() {
    for slots in [1, 2] {
        let temp = tempfile::Builder::new()
            .prefix("bc-bp-")
            .tempdir_in("/tmp")
            .unwrap();
        let svc = CloudService::open(config(temp.path().join("r"), slots), OWNER).unwrap();
        let owner = svc.auth.authenticate(OWNER).unwrap();
        let a = svc.registry.create("a", "records-v1", "a").unwrap();
        let b = svc.registry.create("b", "records-v1", "b").unwrap();
        svc.supervisor.ensure_running(a.id).await.unwrap();
        let held = if slots == 2 {
            Some(svc.supervisor.lease(b.id).await.unwrap())
        } else {
            None
        };
        let active = svc.registry.definition(a.id).unwrap();
        let mut definition = serde_json::to_value(active.definition).unwrap();
        definition["resources"]["extra"] = json!({"terminal":{"kind":"count"}});
        for _ in 0..2 {
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                svc.execute(
                    &owner,
                    Operation::ApplyDefinitionUpdate {
                        bog_id: a.id,
                        definition: definition.clone(),
                        expected_revision: active.revision,
                    },
                ),
            )
            .await
            .unwrap();
            assert_eq!(result.err().unwrap().code, "capacity");
        }
        if let Some(lease) = &held {
            assert_eq!(
                lease
                    .client
                    .request(reqwest::Method::GET, "/views/docs", None)
                    .await
                    .unwrap()
                    .0,
                200
            );
        }
        let db = rusqlite::Connection::open(temp.path().join("r/registry.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM definition_jobs", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(held);
        svc.supervisor.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn configured_restore_reclaims_warm_workers_but_counts_pre_socket_survivors() {
    for survivor in [false, true] {
        let temp = tempfile::Builder::new()
            .prefix("bc-bp-")
            .tempdir_in("/tmp")
            .unwrap();
        let svc = CloudService::open(config(temp.path().join("r"), 2), OWNER).unwrap();
        let owner = svc.auth.authenticate(OWNER).unwrap();
        let definition: serde_json::Value =
            serde_json::from_str(include_str!("../../docs/examples/composable/todo.json")).unwrap();
        let created = svc
            .execute(
                &owner,
                Operation::CreateDefinedBog {
                    name: "source".into(),
                    definition,
                    idempotency_key: "source".into(),
                },
            )
            .await
            .unwrap();
        let id = serde_json::from_value(created.body["id"].clone()).unwrap();
        svc.execute(
            &owner,
            Operation::UpsertRecord {
                bog_id: id,
                key: "saved".into(),
                data: json!({"value":42}),
            },
        )
        .await
        .unwrap();
        let archive = svc.supervisor.backup(id).await.unwrap();
        svc.supervisor.ensure_running(id).await.unwrap();
        let lease = if survivor {
            Some(svc.supervisor.lease(id).await.unwrap())
        } else {
            None
        };
        let orphan_lock = if survivor {
            // Simulate a surviving process discovered on disk before its socket
            // exists, without adding another retained database to the quota.
            let dir = svc
                .supervisor
                .instance_dir(bog_cloud::BogId(uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let lock = std::fs::File::create(dir.join("worker.lock")).unwrap();
            lock.try_lock().unwrap();
            Some(lock)
        } else {
            // Separate workspace keeps the legacy retained quota independent
            // from a globally full resident pool.
            let workspace = bog_cloud::WorkspaceId(uuid::Uuid::new_v4());
            let db = rusqlite::Connection::open(temp.path().join("r/registry.sqlite")).unwrap();
            db.execute(
                "INSERT INTO workspaces(id,name,created_at) VALUES (?1,'other',0)",
                [workspace.to_string()],
            )
            .unwrap();
            let b = svc
                .registry
                .create_scoped(workspace, "b", "records-v1", "b")
                .unwrap();
            svc.supervisor.ensure_running(b.id).await.unwrap();
            None
        };
        let before = svc.registry.list().unwrap().len();
        let restored = if survivor {
            let error = svc
                .supervisor
                .restore(&archive, "blocked")
                .await
                .err()
                .unwrap();
            assert_eq!(error.code, "capacity");
            assert_eq!(svc.registry.list().unwrap().len(), before);
            assert!(error.message.contains("surviving workers"));
            drop(orphan_lock);
            drop(lease);
            svc.supervisor.restore(&archive, "retry").await.unwrap()
        } else {
            svc.supervisor
                .restore(&archive, "restored")
                .await
                .expect("restore reclaims unrelated warm capacity")
        };
        assert!(svc.supervisor.resident_count().await <= 2);
        let result = svc
            .execute(
                &owner,
                Operation::GetRecord {
                    bog_id: restored.id,
                    key: "saved".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(result.body["data"], json!({"value":42}));
        svc.supervisor.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn an_adopted_legacy_worker_without_freeze_support_remains_writable() {
    use axum::{
        Json,
        routing::{get, post},
    };
    let temp = tempfile::Builder::new()
        .prefix("bc-bp-")
        .tempdir_in("/tmp")
        .unwrap();
    let svc = CloudService::open(config(temp.path().join("r"), 2), OWNER).unwrap();
    let owner = svc.auth.authenticate(OWNER).unwrap();
    let bog = svc
        .registry
        .create("legacy", "records-v1", "legacy")
        .unwrap();
    svc.registry
        .start_generation(bog.id, "legacy-nonce")
        .unwrap();
    let dir = svc.supervisor.instance_dir(bog.id);
    std::fs::create_dir_all(&dir).unwrap();
    // Adopt an old-worker protocol fixture: it serves identity/schema/records,
    // but predates both freeze endpoints. No database mutation is frozen.
    let identity =
        json!({"instance_id":bog.id,"nonce":"legacy-nonce","template_version":"records-v1"});
    let records = bog_cloud_records::records_service(&dir.join("data")).unwrap();
    let router = records
        .router()
        .route(
            "/_cloud/identity",
            get(move || async move { Json(identity) }),
        )
        .route(
            "/_cloud/shutdown",
            post(|| async { Json(json!({"ok":true})) }),
        );
    let listener = tokio::net::UnixListener::bind(dir.join("worker.sock")).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    svc.supervisor.ensure_running(bog.id).await.unwrap();
    let active = svc.registry.definition(bog.id).unwrap();
    let mut definition = serde_json::to_value(active.definition).unwrap();
    definition["resources"]["extra"] = json!({"terminal":{"kind":"count"}});
    let result = svc
        .execute(
            &owner,
            Operation::ApplyDefinitionUpdate {
                bog_id: bog.id,
                definition,
                expected_revision: active.revision,
            },
        )
        .await
        .unwrap();
    let job = result.body["job_id"].as_str().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = svc.registry.definition_job(bog.id, job).unwrap();
            if status["status"] != "building" {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        status["status"], "failed",
        "a definitive 404 cannot have paused writes: {status}"
    );
    let result = svc
        .execute(
            &owner,
            Operation::UpsertRecord {
                bog_id: bog.id,
                key: "saved".into(),
                data: json!({"saved":true}),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.status, 200);
    let result = svc
        .execute(
            &owner,
            Operation::GetRecord {
                bog_id: bog.id,
                key: "saved".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(result.body["data"], json!({"saved":true}));
    std::fs::remove_file(dir.join("worker.sock")).unwrap();
    svc.supervisor.shutdown().await.unwrap();
    server.abort();
}
