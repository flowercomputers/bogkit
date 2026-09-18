use bog_cloud::{Auth, Registry, WorkspaceId};
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::sync::Arc;
#[test]
fn v1_upgrade_preserves_ids_requests_and_unexpired_legacy_tokens() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("registry.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../migrations/001_registry.sql"))
        .unwrap();
    let bog = uuid::Uuid::new_v4().to_string();
    let token = uuid::Uuid::new_v4().to_string();
    let secret = format!("{token}.legacy-secret");
    db.execute("INSERT INTO bogs(id,name,template,template_version,desired_state,observed_state,created_at) VALUES(?1,'old','records-v1','records-v1','running','ready',0)",[&bog]).unwrap();
    db.execute(
        "INSERT INTO tokens(id,bog_id,secret_hash,scope,created_at) VALUES(?1,?2,?3,'write',0)",
        params![token, bog, Sha256::digest(secret.as_bytes()).to_vec()],
    )
    .unwrap();
    let body = Sha256::digest(
        serde_json::json!({"name":"old","template":"records-v1"})
            .to_string()
            .as_bytes(),
    )
    .to_vec();
    db.execute(
        "INSERT INTO create_requests(request_key,body_hash,bog_id) VALUES('legacy-create',?1,?2)",
        params![body, bog],
    )
    .unwrap();
    drop(db);
    let r = Arc::new(Registry::open(&path).unwrap());
    assert_eq!(
        r.list_scoped(WorkspaceId::legacy()).unwrap()[0]
            .id
            .to_string(),
        bog
    );
    assert_eq!(
        r.create("old", "records-v1", "legacy-create")
            .unwrap()
            .id
            .to_string(),
        bog
    );
    let a = Auth::new(r.clone(), &"x".repeat(32)).unwrap();
    let p = a.authenticate(&secret).unwrap();
    a.authorize(
        &p,
        Some(bog_cloud::BogId(uuid::Uuid::parse_str(&bog).unwrap())),
        true,
    )
    .unwrap();
    drop(a);
    drop(r);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    let expiry: Option<i64> = db
        .query_row("SELECT expires_at FROM tokens", [], |r| r.get(0))
        .unwrap();
    assert_eq!(expiry, None);
}

#[test]
fn concurrent_v1_open_serializes_migration() {
    for round in 0..20 {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("registry.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../migrations/001_registry.sql"))
            .unwrap();
        drop(db);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let registry =
                        Registry::open(&path).unwrap_or_else(|e| panic!("round {round}: {e}"));
                    assert!(registry.list().unwrap().is_empty());
                    assert_migrated(&path);
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
fn assert_migrated(path: &std::path::Path) {
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}
#[test]
fn migration_child_process() {
    let Some(path) = std::env::var_os("BOG_TEST_MIGRATION_PATH") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let ready = std::path::PathBuf::from(std::env::var_os("BOG_TEST_MIGRATION_START").unwrap());
    let start = std::time::Instant::now();
    while !ready.exists() {
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let registry = Registry::open(&path).unwrap();
    assert!(registry.list().unwrap().is_empty());
    assert_migrated(&path);
}
#[test]
fn concurrent_processes_migrate_v1_twenty_times() {
    for round in 0..20 {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("registry.sqlite");
        let ready = d.path().join("start");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../migrations/001_registry.sql"))
            .unwrap();
        drop(db);
        let children: Vec<_> = (0..4)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "migration_child_process", "--nocapture"])
                    .env("BOG_TEST_MIGRATION_PATH", &path)
                    .env("BOG_TEST_MIGRATION_START", &ready)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        std::fs::write(&ready, b"start").unwrap();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "round {round}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_migrated(&path);
    }
}
#[test]
fn future_registry_version_is_rejected_without_migration() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("registry.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version=99;").unwrap();
    drop(db);
    assert_eq!(
        Registry::open(&path).err().unwrap().code,
        "incompatible_registry"
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        99
    );
}

#[test]
fn v3_upgrade_preserves_memberships_and_does_not_grant_privileges() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("registry.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../migrations/001_registry.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/002_workspaces.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/003_agent_tokens.sql"))
        .unwrap();
    db.execute("INSERT INTO accounts(id,issuer,subject,created_at) VALUES('owner','issuer','owner',0),('member','issuer','member',0)",[]).unwrap();
    db.execute(
        "INSERT INTO memberships VALUES(?1,'owner','owner'),(?1,'member','member')",
        [WorkspaceId::legacy().to_string()],
    )
    .unwrap();
    drop(db);
    drop(Registry::open(&path).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    let roles: String = db
        .query_row(
            "SELECT group_concat(role) FROM (SELECT role FROM memberships ORDER BY account_id)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(roles, "member,owner");
    let elevated: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM accounts WHERE uncapped_bogs OR platform_operator",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(elevated, 0);
    let uncapped: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM workspaces WHERE uncapped_bogs",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(uncapped, 0);
    assert_migrated(&path);
}
