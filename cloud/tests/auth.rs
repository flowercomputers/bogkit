use bog_cloud::{Auth, Registry, Scope};
use std::sync::Arc;
const OWNER: &str = "test-owner-credential-at-least-32-bytes";
#[test]
fn scope_and_revocation_apply_to_existing_principals_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let registry = Arc::new(Registry::open(&path).unwrap());
    let a = registry.create("a", "records-v1", "a").unwrap().id;
    let b = registry.create("b", "records-v1", "b").unwrap().id;
    let auth = Auth::new(registry.clone(), OWNER).unwrap();
    let owner = auth.authenticate(OWNER).unwrap();
    let issued = auth.issue(&owner, a, Scope::Read).unwrap();
    let reader = auth.authenticate(&issued.secret).unwrap();
    auth.authorize(&reader, Some(a), false).unwrap();
    assert_eq!(
        auth.authorize(&reader, Some(a), true).unwrap_err().code,
        "forbidden"
    );
    assert_eq!(
        auth.authorize(&reader, Some(b), false).unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        auth.authorize(&reader, None, false).unwrap_err().code,
        "forbidden"
    );
    assert!(auth.issue(&reader, a, Scope::Write).is_err());
    let write = auth.issue(&owner, a, Scope::Write).unwrap();
    auth.authorize(&auth.authenticate(&write.secret).unwrap(), Some(a), true)
        .unwrap();
    auth.revoke(&owner, a, &issued.id).unwrap();
    assert_eq!(
        auth.authorize(&reader, Some(a), false).unwrap_err().code,
        "unauthorized"
    );
    drop(auth);
    drop(registry);
    let registry = Arc::new(Registry::open(&path).unwrap());
    let auth = Auth::new(registry, OWNER).unwrap();
    assert!(auth.authenticate(&issued.secret).is_err());
    let disk = std::fs::read(&path).unwrap();
    assert!(
        !disk
            .windows(issued.secret.len())
            .any(|w| w == issued.secret.as_bytes())
    );
    assert!(!disk.windows(OWNER.len()).any(|w| w == OWNER.as_bytes()));
}
#[test]
fn malformed_credentials_and_short_bootstrap_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::open(&dir.path().join("db")).unwrap());
    assert!(Auth::new(registry.clone(), "short").is_err());
    let auth = Auth::new(registry, OWNER).unwrap();
    for value in ["", "not-a-token", "test-owner-credential-at-least-32-byteX"] {
        assert!(auth.authenticate(value).is_err());
    }
}
