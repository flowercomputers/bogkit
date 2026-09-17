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
        2
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
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("registry.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../migrations/001_registry.sql"))
        .unwrap();
    drop(db);
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || Registry::open(&path).unwrap().list().unwrap())
        })
        .collect();
    for t in threads {
        assert!(t.join().unwrap().is_empty());
    }
}
