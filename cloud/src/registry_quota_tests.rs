use super::*;
use crate::{Auth, Principal, PrincipalKind};
use std::sync::Arc;

fn setup() -> (tempfile::TempDir, Arc<Registry>, Auth) {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::open(&dir.path().join("registry.sqlite")).unwrap());
    let auth = Auth::new(registry.clone(), &"x".repeat(32)).unwrap();
    (dir, registry, auth)
}
fn person(auth: &Auth, subject: &str) -> Principal {
    let (account, workspace) = auth.provision_subject("quota-test", subject).unwrap();
    Principal {
        read_only: false,
        kind: PrincipalKind::Human,
        expires_at: None,
        account_id: Some(account.id),
        workspace_id: Some(workspace.id),
        token_id: None,
        bog_id: None,
    }
}
fn nine_retained(registry: &Registry, auth: &Auth) {
    for workspace in 0..3 {
        let p = person(auth, &format!("existing-{workspace}"));
        for i in 0..3 {
            registry
                .create_for_principal(
                    &p,
                    &format!("existing-{i}"),
                    "records-v1",
                    &format!("existing-{i}"),
                    32,
                )
                .unwrap();
        }
    }
}
#[test]
fn configured_workspace_allowance_is_separate_from_eight_resident_slots() {
    let (_dir, registry, auth) = setup();
    nine_retained(&registry, &auth);
    let p = person(&auth, "new");
    for i in 0..3 {
        registry
            .create_defined(
                &p,
                &format!("new-{i}"),
                &format!("new-{i}"),
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
    }
    assert_eq!(
        registry
            .create_defined(
                &p,
                "fourth",
                "fourth",
                &bog_definition::Definition::records_v1(),
                8
            )
            .unwrap_err()
            .code,
        "capacity"
    );
    assert_eq!(registry.list().unwrap().len(), 12);
}
#[test]
fn uncapped_configured_account_still_obeys_global_retained_limit() {
    let (_dir, registry, auth) = setup();
    nine_retained(&registry, &auth);
    let p = person(&auth, "uncapped");
    registry
        .connection()
        .unwrap()
        .execute(
            "UPDATE accounts SET uncapped_bogs=1 WHERE id=?1",
            [p.account_id().unwrap()],
        )
        .unwrap();
    for i in 9..32 {
        registry
            .create_defined(
                &p,
                &format!("new-{i}"),
                &format!("new-{i}"),
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
    }
    assert_eq!(
        registry
            .create_defined(
                &p,
                "over-global",
                "over-global",
                &bog_definition::Definition::records_v1(),
                8
            )
            .unwrap_err()
            .code,
        "capacity"
    );
    assert_eq!(registry.list().unwrap().len(), 32);
    assert_eq!(
        registry
            .create_restoring("over-global-restore", "over-global-restore", 8)
            .unwrap_err()
            .code,
        "capacity"
    );
}
#[test]
fn legacy_creation_and_restore_keep_workspace_eight_after_platform_exceeds_eight() {
    let (_dir, registry, auth) = setup();
    nine_retained(&registry, &auth);
    let p = auth.authenticate(&"x".repeat(32)).unwrap();
    registry
        .create_restoring("restored", "restored", 8)
        .unwrap();
    registry
        .create_limited("plain", "records-v1", "plain", 8)
        .unwrap();
    for i in 2..8 {
        registry
            .create_defined(
                &p,
                &format!("legacy-{i}"),
                &format!("legacy-{i}"),
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
    }
    assert_eq!(
        registry
            .create_restoring("ninth-restore", "ninth-restore", 8)
            .unwrap_err()
            .code,
        "capacity"
    );
    assert_eq!(
        registry
            .create_defined(
                &p,
                "ninth",
                "ninth",
                &bog_definition::Definition::records_v1(),
                8
            )
            .unwrap_err()
            .code,
        "capacity"
    );
    assert_eq!(registry.list().unwrap().len(), 17);
}
