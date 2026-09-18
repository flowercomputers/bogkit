//! The deployed schema is built from its original migrations, never by downgrading v5.
use bog_cloud::{BogId, CloudService, Operation, WorkspaceId, config::Config};
use rusqlite::{Connection, params, types::Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const BOG: &str = "11111111-1111-4111-8111-111111111111";
const TOKEN: &str = "22222222-2222-4222-8222-222222222222";
const AGENT: &str = "33333333-3333-4333-8333-333333333333";
const OWNER: &str = "migration-test-owner-secret-at-least-32-bytes";
const TABLES: &[&str] = &[
    "accounts",
    "workspaces",
    "memberships",
    "bogs",
    "tokens",
    "agent_tokens",
    "create_requests",
    "workspace_create_requests",
    "invitations",
    "audit_events",
];
fn snapshot(path: &Path) -> Vec<Vec<Vec<Value>>> {
    let db = Connection::open(path).unwrap();
    TABLES
        .iter()
        .map(|table| {
            let mut statement = db
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                .unwrap()
                .map(Result::unwrap)
                .collect()
        })
        .collect()
}
fn version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}
fn fixture(path: &Path) -> (String, String) {
    let db = Connection::open(path).unwrap();
    for sql in [
        include_str!("../migrations/001_registry.sql"),
        include_str!("../migrations/002_workspaces.sql"),
        include_str!("../migrations/003_agent_tokens.sql"),
        include_str!("../migrations/004_workspace_quotas.sql"),
    ] {
        db.execute_batch(sql).unwrap();
    }
    let workspace = WorkspaceId::legacy().to_string();
    db.execute_batch("INSERT INTO accounts(id,issuer,subject,created_at,uncapped_bogs) VALUES('owner','https://cloud.bog.new','owner-subject',17,1); INSERT INTO memberships VALUES('00000000-0000-0000-0000-000000000001','owner','owner'); UPDATE workspaces SET personal_account_id='owner',uncapped_bogs=1; INSERT INTO workspace_create_requests VALUES('owner','workspace-request','Unclaimed legacy','00000000-0000-0000-0000-000000000001'); INSERT INTO audit_events(workspace_id,account_id,action,created_at) VALUES('00000000-0000-0000-0000-000000000001','owner','existing_event',17);").unwrap();
    db.execute("INSERT INTO bogs(id,workspace_id,name,template,template_version,desired_state,observed_state,generation,created_at) VALUES(?1,?2,'existing','records-v1','records-v1','stopped','stopped',7,17)",params![BOG,workspace]).unwrap();
    let secret = format!("{TOKEN}.existing-secret");
    db.execute("INSERT INTO tokens(id,bog_id,secret_hash,scope,created_at,workspace_id,account_id) VALUES(?1,?2,?3,'read',17,?4,'owner')",params![TOKEN,BOG,Sha256::digest(secret.as_bytes()).to_vec(),workspace]).unwrap();
    let agent = format!("bog_agent_{AGENT}.{}", "a".repeat(64));
    db.execute(
        "INSERT INTO agent_tokens VALUES(?1,'owner','existing agent',?2,17,4102444800,NULL)",
        params![AGENT, Sha256::digest(agent.as_bytes()).to_vec()],
    )
    .unwrap();
    let body = serde_json::json!({"name":"existing","template":"records-v1"}).to_string();
    db.execute(
        "INSERT INTO create_requests VALUES(?1,'existing-request',?2,?3)",
        params![workspace, Sha256::digest(body.as_bytes()).to_vec(), BOG],
    )
    .unwrap();
    (secret, agent)
}
fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
async fn record(path: &Path, method: &str) -> serde_json::Value {
    use tower::ServiceExt;
    let service = bog_cloud_records::records_service(path).unwrap();
    let response = service
        .router()
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri("/docs/existing-key")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(r#"{"title":"preserved record"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    service.begin_shutdown();
    service.shutdown().unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
#[tokio::test]
async fn feature_off_upgrade_preserves_v4_identity_permissions_and_recovery_copy() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("active");
    let recovery = temp.path().join("recovery");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&recovery).unwrap();
    let path = root.join("registry.sqlite");
    let (secret, agent) = fixture(&path);
    assert_eq!(version(&path), 4);
    let before = snapshot(&path);
    let data = root.join("instances").join(BOG).join("data");
    record(&data, "PUT").await;
    let existing_record = record(&data, "GET").await;
    // Fixture connections are closed: copying the complete quiescent state is safe.
    let saved = recovery.join("registry.sqlite");
    copy_tree(&root, &recovery);
    let saved_bytes = std::fs::read(&saved).unwrap();
    let config = Config::new(root, temp.path().join("unused-worker"));
    assert!(!config.composable_enabled);
    let service = CloudService::open(config, OWNER).unwrap();
    assert_eq!(version(&path), 5);
    assert_eq!(snapshot(&path), before);
    let id = BogId(uuid::Uuid::parse_str(BOG).unwrap());
    let app = service.auth.authenticate(&secret).unwrap();
    service.auth.authorize(&app, Some(id), false).unwrap();
    assert!(service.auth.authorize(&app, Some(id), true).is_err());
    assert!(
        service
            .auth
            .authorize(&app, Some(BogId(uuid::Uuid::new_v4())), false)
            .is_err()
    );
    let delegated = service.auth.authenticate_agent_token(&agent, None).unwrap();
    service.auth.authorize(&delegated, Some(id), true).unwrap();
    assert_eq!(
        service
            .registry
            .create("existing", "records-v1", "existing-request")
            .unwrap()
            .id,
        id
    );
    let owner = service.auth.authenticate(OWNER).unwrap();
    let catalog = service
        .execute(&owner, Operation::ListComponents)
        .await
        .unwrap();
    assert_eq!(catalog.body["enabled"], false);
    let denied = service
        .execute(
            &owner,
            Operation::CreateDefinedBog {
                name: "disabled".into(),
                definition: serde_json::json!({}),
                idempotency_key: "disabled".into(),
            },
        )
        .await
        .err()
        .unwrap();
    assert_eq!(denied.code, "feature_disabled");
    drop(service);
    assert_eq!(snapshot(&path), before);
    assert_eq!(record(&data, "GET").await, existing_record);
    assert_eq!(
        record(&recovery.join("instances").join(BOG).join("data"), "GET").await,
        existing_record
    );
    assert_eq!(version(&saved), 4);
    assert_eq!(std::fs::read(&saved).unwrap(), saved_bytes);
    // Recover into a separate directory. Never rewrite the upgraded version marker.
    let restored = temp.path().join("restored.sqlite");
    std::fs::copy(&saved, &restored).unwrap();
    assert_eq!(version(&restored), 4);
    assert_eq!(snapshot(&restored), before);
    assert_eq!(version(&path), 5);
    // Optional rehearsal against a preserved old executable, with all inherited
    // credentials removed. Its missing-auth guard fires after Registry::open,
    // before any listener or worker is started.
    if let Some(binary) = std::env::var_os("BOG_TEST_V4_SERVER") {
        let probe = |root: &Path| {
            std::process::Command::new(&binary)
                .env_clear()
                .env("BOG_CLOUD_ROOT", root)
                .env("BOG_WORKER_BINARY", "/unused-migration-worker")
                .env("BOG_CLOUD_OWNER_TOKEN", OWNER)
                .output()
                .unwrap()
        };
        let restored_root = temp.path().join("old-binary-recovery");
        copy_tree(&recovery, &restored_root);
        let old = probe(&restored_root);
        assert!(!old.status.success());
        let error = String::from_utf8_lossy(&old.stderr);
        assert!(
            error.contains("GitHub authentication must be configured"),
            "{error}"
        );
        assert_eq!(version(&restored_root.join("registry.sqlite")), 4);
        assert_eq!(snapshot(&restored_root.join("registry.sqlite")), before);
        let upgraded = probe(path.parent().unwrap());
        assert!(!upgraded.status.success());
        let error = String::from_utf8_lossy(&upgraded.stderr);
        assert!(
            error.contains("registry version is newer than this server"),
            "{error}"
        );
        assert_eq!(version(&path), 5);
        assert_eq!(snapshot(&path), before);
    }
    drop(bog_cloud::Registry::open(&restored).unwrap());
    assert_eq!(snapshot(&restored), before);
    let db = Connection::open(&restored).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}
