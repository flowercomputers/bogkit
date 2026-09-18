//! Short-lived account-bound handoffs. Identifiers never authorize token issuance.
use crate::registry::{db_error, now};
use crate::{BogId, CloudError, CloudService, Principal, PrincipalKind, Scope, WorkspaceId};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex};
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct PendingAccess(Mutex<HashMap<String, Handoff>>);
// No Debug: keep private account binding and credential lifecycle state out of logs.
#[derive(Clone)]
struct Handoff {
    account: String,
    bog: BogId,
    workspace: WorkspaceId,
    scope: Scope,
    label: String,
    expires: i64,
}
fn missing() -> CloudError {
    CloudError::new("not_found", "app access handoff not found")
}
fn unavailable() -> CloudError {
    CloudError::new("unavailable", "app access unavailable")
}
impl Handoff {
    fn metadata(&self, id: &str, origin: &str) -> Value {
        json!({"origin":origin,"handoff_id":id,"expires_at":self.expires,"bog_id":self.bog,"workspace_id":self.workspace,"scope":self.scope,"label":self.label,"redeem_path":format!("/v1/app-access/{id}/redeem"),"console_path":format!("/console?handoff={id}"),"installation":format!("Download the helper from /bog-app-access.py, then run: python3 bog-app-access.py --origin {origin} --handoff {id} --output /private/path/config.json. Choose an existing private directory and a new output file. The helper requires its own approval with the same account. Alternatively open /console?handoff={id} to review and explicitly download the app configuration. Preparation creates no credential; redemption creates it once.")})
    }
}
impl CloudService {
    fn app_account<'a>(&self, p: &'a Principal) -> Result<&'a str, CloudError> {
        if self.native_auth.is_none()
            || !matches!(p.kind(), PrincipalKind::Human | PrincipalKind::Agent)
        {
            return Err(CloudError::new(
                "forbidden",
                "native account connection required",
            ));
        }
        let account = p.account_id().ok_or_else(missing)?;
        let native: bool = self.registry.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 AND issuer='https://github.com' AND suspended_at IS NULL)", [account], |r| r.get(0)).map_err(db_error)?;
        if !native {
            return Err(CloudError::new(
                "forbidden",
                "native account connection required",
            ));
        }
        Ok(account)
    }
    fn app_target(&self, p: &Principal, h: &Handoff) -> Result<Principal, CloudError> {
        let mut target = p.clone();
        target.workspace_id = Some(h.workspace);
        self.auth.authorize(&target, None, true)?;
        self.auth.authorize(&target, Some(h.bog), true)?;
        Ok(target)
    }
    pub fn prepare_app_access(
        &self,
        p: &Principal,
        bog: BogId,
        scope: Scope,
        label: String,
    ) -> Result<Value, CloudError> {
        let account = self.app_account(p)?;
        crate::agent_tokens::validate_name(&label)?;
        let h = Handoff {
            account: account.into(),
            bog,
            workspace: p.workspace_id().ok_or_else(missing)?,
            scope,
            label,
            expires: now() + 600,
        };
        self.app_target(p, &h)?;
        let mut pending = self.app_access.0.lock().map_err(|_| unavailable())?;
        pending.retain(|_, h| h.expires > now());
        if pending.len() >= 10000 || pending.values().filter(|h| h.account == account).count() >= 10
        {
            return Err(CloudError::new(
                "capacity",
                "pending app access limit reached",
            ));
        }
        let id = Uuid::new_v4().to_string();
        let metadata = h.metadata(
            &id,
            &self
                .native_auth
                .as_ref()
                .ok_or_else(unavailable)?
                .config
                .origin(),
        );
        pending.insert(id, h);
        Ok(metadata)
    }
    fn pending_access(
        &self,
        p: &Principal,
        id: &str,
        consume: bool,
    ) -> Result<Handoff, CloudError> {
        let account = self.app_account(p)?;
        let mut pending = self.app_access.0.lock().map_err(|_| unavailable())?;
        pending.retain(|_, h| h.expires > now());
        let h = pending
            .get(id)
            .filter(|h| h.account == account)
            .ok_or_else(missing)?;
        self.app_target(p, h)?;
        let h = h.clone();
        if consume {
            pending.remove(id);
        }
        Ok(h)
    }
    pub fn describe_app_access(&self, p: &Principal, id: &str) -> Result<Value, CloudError> {
        Ok(self.pending_access(p, id, false)?.metadata(
            id,
            &self
                .native_auth
                .as_ref()
                .ok_or_else(unavailable)?
                .config
                .origin(),
        ))
    }
    // Cleanup authority comes from possession of this just-issued result, never
    // from a caller-supplied token ID or the account's current management role.
    fn rollback_app_access_token(
        &self,
        h: &Handoff,
        token: &crate::IssuedToken,
    ) -> Result<(), CloudError> {
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let changed = tx.execute(
            "UPDATE tokens SET revoked_at=COALESCE(revoked_at,?1) WHERE id=?2 AND secret_hash=?3 AND account_id=?4 AND workspace_id=?5 AND bog_id=?6",
            params![now(),token.id,crate::registry::hash(token.secret.as_bytes()),h.account,h.workspace.to_string(),h.bog.to_string()],
        ).map_err(db_error)?;
        if changed != 1 {
            return Err(unavailable());
        }
        tx.execute("INSERT INTO audit_events(workspace_id,account_id,action,resource_id,created_at) VALUES(?1,?2,'token.revoked',?3,?4)",params![h.workspace.to_string(),h.account,token.id,now()]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    pub fn redeem_app_access(&self, p: &Principal, id: &str) -> Result<Value, CloudError> {
        let h = self.pending_access(p, id, true)?;
        let target = self.app_target(p, &h)?;
        let native = self.native_auth.as_ref().ok_or_else(unavailable)?;
        // Reserve metadata storage before minting; on failure revoke the ordinary,
        // audited token. Consumption is final even when delivery or storage fails.
        let db = native.db.lock().map_err(|_| unavailable())?;
        let token = self.auth.issue(&target, h.bog, h.scope)?;
        if let Err(error) = db.execute(
            "INSERT INTO app_token_labels(token_id,label) VALUES(?1,?2)",
            params![token.id, h.label],
        ) {
            self.rollback_app_access_token(&h, &token)?;
            return Err(db_error(error));
        }
        let expires: Option<i64> = self
            .registry
            .connection()?
            .query_row(
                "SELECT expires_at FROM tokens WHERE id=?1",
                [&token.id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        Ok(
            json!({"token":token.secret,"id":token.id,"bog_id":h.bog,"workspace_id":h.workspace,"scope":h.scope,"label":h.label,"expires_at":expires}),
        )
    }
}
impl crate::native_auth::NativeAuth {
    pub(crate) fn add_app_labels(&self, tokens: &mut Value) -> Result<(), CloudError> {
        let db = self.db.lock().map_err(|_| unavailable())?;
        if let Some(tokens) = tokens.as_array_mut() {
            for token in tokens {
                let label: Option<String> = db
                    .query_row(
                        "SELECT label FROM app_token_labels WHERE token_id=?1",
                        [token["id"].as_str().ok_or_else(unavailable)?],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(db_error)?;
                token["label"] = json!(label);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Operation,
        config::Config,
        native_auth::{GithubConfig, NativeAuth},
    };
    use std::sync::Arc;
    const OWNER: &str = "private-test-owner-credential-over-32-bytes";
    fn service(root: &std::path::Path) -> Arc<CloudService> {
        let mut s = CloudService::open(
            Config::new(root.to_path_buf(), std::env::current_exe().unwrap()),
            OWNER,
        )
        .unwrap();
        Arc::get_mut(&mut s).unwrap().native_auth = Some(
            NativeAuth::new(
                GithubConfig::for_loopback_testing("http://127.0.0.1:8080").unwrap(),
                &root.join("sessions"),
            )
            .unwrap(),
        );
        s
    }
    fn person(s: &CloudService, subject: &str) -> Principal {
        let identity = crate::oauth::VerifiedIdentity::native(subject, (now() + 3600) as u64);
        let (_, w) = s.auth.provision_identity(&identity).unwrap();
        s.auth.principal_from_verified(&identity, w.id).unwrap()
    }
    fn bog(s: &CloudService, p: &Principal) -> BogId {
        s.registry
            .create_for_principal(p, "test", "records-v1", "test", 32)
            .unwrap()
            .id
    }
    fn prepare(s: &CloudService, p: &Principal, b: BogId) -> String {
        s.prepare_app_access(p, b, Scope::Read, "Test app".into())
            .unwrap()["handoff_id"]
            .as_str()
            .unwrap()
            .into()
    }
    #[tokio::test]
    async fn private_handoff_is_scoped_single_use_and_label_survives_restart() {
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let root = tmp.path().join("root");
        let s = service(&root);
        let p = person(&s, "42");
        let b = bog(&s, &p);
        let other = person(&s, "43");
        let other_bog = bog(&s, &other);
        let operator = s.auth.authenticate(OWNER).unwrap();
        assert!(
            s.prepare_app_access(&operator, b, Scope::Read, "denied".into())
                .is_err()
        );
        let id = prepare(&s, &p, b);
        assert!(s.auth.list_tokens(&p).unwrap().is_empty());
        assert_eq!(
            s.describe_app_access(&other, &id).unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            s.redeem_app_access(&other, &id).unwrap_err().code,
            "not_found"
        );
        let connection = s
            .auth
            .issue_agent_token(&p, "Different connection")
            .unwrap();
        let agent = s
            .auth
            .authenticate_agent_token(&connection.secret, None)
            .unwrap();
        let token = s.redeem_app_access(&agent, &id).unwrap();
        assert!(s.describe_app_access(&p, &id).is_err());
        assert!(s.redeem_app_access(&p, &id).is_err());
        let app = s
            .auth
            .authenticate(token["token"].as_str().unwrap())
            .unwrap();
        assert!(s.auth.authorize(&app, Some(b), false).is_ok());
        assert!(s.auth.authorize(&app, Some(b), true).is_err());
        assert!(s.auth.authorize(&app, Some(other_bog), false).is_err());
        assert!(
            s.prepare_app_access(&app, b, Scope::Read, "denied".into())
                .is_err()
        );
        let write_id = s
            .prepare_app_access(&p, b, Scope::Write, "Writer".into())
            .unwrap()["handoff_id"]
            .as_str()
            .unwrap()
            .to_string();
        let writer = s.redeem_app_access(&p, &write_id).unwrap();
        let wp = s
            .auth
            .authenticate(writer["token"].as_str().unwrap())
            .unwrap();
        assert!(s.auth.authorize(&wp, Some(b), true).is_ok());
        s.auth
            .revoke(&p, b, writer["id"].as_str().unwrap())
            .unwrap();
        assert!(
            s.auth
                .authenticate(writer["token"].as_str().unwrap())
                .is_err()
        );
        let pending = prepare(&s, &p, b);
        drop(s);
        let s = service(&root);
        let p = person(&s, "42");
        assert!(s.redeem_app_access(&p, &pending).is_err());
        let listed = s
            .execute(&p, Operation::ListTokens { bog_id: b })
            .await
            .unwrap()
            .body;
        assert!(
            listed["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["label"] == "Test app")
        );
        assert!(
            !listed
                .to_string()
                .contains(token["token"].as_str().unwrap())
        );
    }
    #[test]
    fn member_metadata_failure_revokes_only_new_token_with_audit() {
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let s = service(&tmp.path().join("root"));
        let owner = person(&s, "42");
        let b = bog(&s, &owner);
        let member = person(&s, "43");
        let invitation = s.auth.invite(&owner, "member").unwrap();
        let workspace = s
            .auth
            .accept_invitation_for_principal(&member, &invitation.secret)
            .unwrap();
        let member = s.auth.select_workspace(&member, workspace).unwrap();
        let existing = s.auth.issue(&member, b, Scope::Read).unwrap();
        assert!(s.auth.revoke(&member, b, &existing.id).is_err());
        let id = prepare(&s, &member, b);
        s.native_auth.as_ref().unwrap().db.lock().unwrap().execute_batch("CREATE TRIGGER fail_app_label BEFORE INSERT ON app_token_labels BEGIN SELECT RAISE(FAIL, 'test failure'); END;").unwrap();
        assert!(s.redeem_app_access(&member, &id).is_err());
        assert!(s.redeem_app_access(&member, &id).is_err());
        assert!(s.auth.authenticate(&existing.secret).is_ok());
        let tokens = s.auth.list_tokens(&owner).unwrap();
        assert_eq!(tokens.len(), 2);
        let failed = tokens.iter().find(|t| t.id != existing.id).unwrap();
        assert!(failed.revoked_at.is_some());
        let db = s.registry.connection().unwrap();
        let audited: i64 = db.query_row("SELECT COUNT(*) FROM audit_events WHERE action='token.revoked' AND resource_id=?1 AND account_id=?2 AND workspace_id=?3",params![failed.id,member.account_id(),workspace.to_string()],|r|r.get(0)).unwrap();
        assert_eq!(audited, 1);
    }
    #[tokio::test]
    async fn rest_browser_redemption_requires_csrf_and_never_caches() {
        use axum::{
            body::{Body, to_bytes},
            http::Request,
        };
        use tower::ServiceExt;
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let s = service(&tmp.path().join("root"));
        let p = person(&s, "42");
        let b = bog(&s, &p);
        let session = "a".repeat(64);
        let csrf = "b".repeat(64);
        s.native_auth
            .as_ref()
            .unwrap()
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO native_sessions VALUES(?1,?2,'42',?3)",
                params![
                    crate::registry::hash(session.as_bytes()),
                    csrf,
                    now() + 3600
                ],
            )
            .unwrap();
        let router = crate::build_rest_router(s.clone());
        let id = prepare(&s, &p, b);
        let cookie = format!("{}={session}", crate::browser_auth::SESSION_COOKIE);
        let path = format!("/v1/app-access/{id}/redeem");
        for origin in [None, Some("https://evil.example")] {
            let mut req = Request::builder()
                .method("POST")
                .uri(&path)
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .header("x-csrf-token", &csrf);
            if let Some(origin) = origin {
                req = req.header("origin", origin);
            }
            let response = router
                .clone()
                .oneshot(req.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), 401);
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/app-access/{id}"))
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let metadata: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert!(metadata.get("token").is_none());
        let contract = crate::contract::openapi();
        let schema = &contract["paths"]["/v1/bogs/{bog_id}/app-access"]["post"]["responses"]["200"]
            ["content"]["application/json"]["schema"];
        assert!(schema["properties"].get("revoked").is_none());
        for key in schema["required"].as_array().unwrap() {
            assert!(
                metadata.get(key.as_str().unwrap()).is_some(),
                "missing {key}"
            );
        }
        assert!(schema["properties"].get("handoff_id").is_some());

        assert_eq!(metadata["origin"], "http://127.0.0.1:8080");
        assert!(
            metadata["installation"]
                .as_str()
                .unwrap()
                .contains("--origin http://127.0.0.1:8080")
        );
        assert!(
            metadata["installation"]
                .as_str()
                .unwrap()
                .contains(&format!(
                    "--handoff {id} --output /private/path/config.json"
                ))
        );
        assert!(
            metadata["installation"]
                .as_str()
                .unwrap()
                .contains("/bog-app-access.py")
        );
        for expected in [200, 404] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&path)
                        .header("cookie", &cookie)
                        .header("origin", "http://127.0.0.1:8080")
                        .header("x-csrf-token", &csrf)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }
    #[test]
    fn expiry_capacity_readonly_and_current_membership_are_enforced() {
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let s = service(&tmp.path().join("root"));
        let p = person(&s, "42");
        let b = bog(&s, &p);
        let mut read = p.clone();
        read.read_only = true;
        assert!(
            s.prepare_app_access(&read, b, Scope::Read, "denied".into())
                .is_err()
        );
        let id = prepare(&s, &p, b);
        assert!(s.redeem_app_access(&read, &id).is_err());
        s.app_access.0.lock().unwrap().get_mut(&id).unwrap().expires = now();
        assert!(s.redeem_app_access(&p, &id).is_err());
        for _ in 0..10 {
            prepare(&s, &p, b);
        }
        assert_eq!(
            s.prepare_app_access(&p, b, Scope::Read, "capacity".into())
                .unwrap_err()
                .code,
            "capacity"
        );
        s.app_access.0.lock().unwrap().clear();
        let id = prepare(&s, &p, b);
        s.registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE workspaces SET suspended_at=?1 WHERE id=?2",
                params![now(), p.workspace_id().unwrap().to_string()],
            )
            .unwrap();
        assert!(s.redeem_app_access(&p, &id).is_err());
        s.registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE workspaces SET suspended_at=NULL WHERE id=?1",
                [p.workspace_id().unwrap().to_string()],
            )
            .unwrap();
        {
            let mut pending = s.app_access.0.lock().unwrap();
            let mut h = pending[&id].clone();
            h.account = "other".into();
            for _ in 1..10000 {
                pending.insert(Uuid::new_v4().to_string(), h.clone());
            }
        }
        assert_eq!(
            s.prepare_app_access(&p, b, Scope::Read, "global capacity".into())
                .unwrap_err()
                .code,
            "capacity"
        );
        s.app_access.0.lock().unwrap().retain(|key, _| key == &id);

        s.registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE accounts SET suspended_at=?1 WHERE id=?2",
                params![now(), p.account_id()],
            )
            .unwrap();
        assert!(s.redeem_app_access(&p, &id).is_err());
        s.registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE accounts SET suspended_at=NULL WHERE id=?1",
                [p.account_id()],
            )
            .unwrap();
        s.registry
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM memberships WHERE account_id=?1",
                [p.account_id()],
            )
            .unwrap();
        assert!(s.redeem_app_access(&p, &id).is_err());
        assert!(
            s.prepare_app_access(&p, b, Scope::Read, "removed".into())
                .is_err()
        );
        assert_eq!(
            s.registry
                .connection()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM tokens", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn concurrent_redeemers_have_exactly_one_winner_and_target_stored_workspace() {
        let tmp = tempfile::tempdir_in("/tmp").unwrap();
        let s = service(&tmp.path().join("root"));
        let p = person(&s, "42");
        let b = bog(&s, &p);
        let id = prepare(&s, &p, b);
        let mut differently_selected = p.clone();
        differently_selected.workspace_id = Some(WorkspaceId(Uuid::new_v4()));
        assert!(s.describe_app_access(&differently_selected, &id).is_ok());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let jobs = (0..8)
            .map(|_| {
                let s = s.clone();
                let p = differently_selected.clone();
                let id = id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    s.redeem_app_access(&p, &id).is_ok()
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            jobs.into_iter()
                .filter_map(|j| j.join().ok())
                .filter(|v| *v)
                .count(),
            1
        );
        assert_eq!(s.auth.list_tokens(&p).unwrap().len(), 1);
    }
}
