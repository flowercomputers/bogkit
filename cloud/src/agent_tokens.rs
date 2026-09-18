//! Account delegation, checked against current membership for each operation.
use crate::{
    Auth, CloudError, Principal, PrincipalKind, WorkspaceId,
    auth::{IssuedToken, random_secret},
    registry::{db_error, hash, now},
};
use rusqlite::{OptionalExtension, params};
use subtle::ConstantTimeEq;
use uuid::Uuid;
fn denied() -> CloudError {
    CloudError::new("unauthorized", "invalid agent credential")
}
pub(crate) fn validate_name(name: &str) -> Result<(), CloudError> {
    if name.trim().is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err(CloudError::new(
            "invalid_request",
            "credential name must contain 1 to 80 bytes without control characters",
        ));
    }
    Ok(())
}
impl Auth {
    fn human_account<'a>(&self, p: &'a Principal) -> Result<&'a str, CloudError> {
        if p.kind() != PrincipalKind::Human {
            return Err(CloudError::new("forbidden", "human session required"));
        }
        self.authorize(p, None, false)?;
        p.account_id.as_deref().ok_or_else(denied)
    }
    pub fn issue_agent_token(&self, p: &Principal, name: &str) -> Result<IssuedToken, CloudError> {
        let account = self.human_account(p)?;
        validate_name(name)?;
        let id = Uuid::new_v4().to_string();
        let secret = format!("bog_agent_{id}.{}", random_secret()?);
        let mut db = self.registry.connection()?;
        let tx = db.transaction().map_err(db_error)?;
        // Recheck suspension in the same write transaction as issuance.
        let active: bool = tx
            .query_row(
                "SELECT suspended_at IS NULL FROM accounts WHERE id=?1",
                [account],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !active {
            return Err(denied());
        }
        tx.execute("INSERT INTO agent_tokens(id,account_id,name,secret_hash,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,account,name,hash(secret.as_bytes()),now(),now()+30*86400]).map_err(db_error)?;
        tx.execute("INSERT INTO audit_events(account_id,action,resource_id,created_at) VALUES(?1,'agent_token_issued',?2,?3)",params![account,id,now()]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(IssuedToken { id, secret })
    }
    pub fn list_agent_tokens(&self, p: &Principal) -> Result<Vec<serde_json::Value>, CloudError> {
        let account = self.human_account(p)?;
        let db = self.registry.connection()?;
        let mut stmt=db.prepare("SELECT id,name,created_at,expires_at,revoked_at FROM agent_tokens WHERE account_id=?1 ORDER BY created_at,id").map_err(db_error)?;
        stmt.query_map([account],|r|Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?,"expires_at":r.get::<_,i64>(3)?,"revoked_at":r.get::<_,Option<i64>>(4)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)
    }
    pub fn revoke_agent_token(&self, p: &Principal, id: &str) -> Result<(), CloudError> {
        let account = self.human_account(p)?;
        let mut db = self.registry.connection()?;
        let tx = db.transaction().map_err(db_error)?;
        let changed=tx.execute("UPDATE agent_tokens SET revoked_at=COALESCE(revoked_at,?1) WHERE id=?2 AND account_id=?3",params![now(),id,account]).map_err(db_error)?;
        if changed != 1 {
            return Err(CloudError::new("not_found", "credential not found"));
        }
        tx.execute("INSERT INTO audit_events(account_id,action,resource_id,created_at) VALUES(?1,'agent_token_revoked',?2,?3)",params![account,id,now()]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    pub fn authenticate_agent_token(
        &self,
        secret: &str,
        workspace: Option<WorkspaceId>,
    ) -> Result<Principal, CloudError> {
        if secret.len() > 200 {
            return Err(denied());
        }
        let (id, part) = secret
            .strip_prefix("bog_agent_")
            .and_then(|s| s.split_once('.'))
            .ok_or_else(denied)?;
        Uuid::parse_str(id).map_err(|_| denied())?;
        if part.len() != 64 {
            return Err(denied());
        }
        let db = self.registry.connection()?;
        let row:Option<(String,Vec<u8>,i64,String)>=db.query_row("SELECT t.account_id,t.secret_hash,t.expires_at,w.id FROM agent_tokens t JOIN accounts a ON a.id=t.account_id JOIN workspaces w ON w.personal_account_id=a.id WHERE t.id=?1 AND t.revoked_at IS NULL AND t.expires_at>?2 AND a.suspended_at IS NULL",params![id,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
        let (_, stored, _, _) = row.ok_or_else(denied)?;
        if !bool::from(stored.ct_eq(&hash(secret.as_bytes()))) {
            return Err(denied());
        }
        drop(db);
        self.principal_for_agent_id(id, workspace)
    }
    pub(crate) fn principal_for_agent_id(
        &self,
        id: &str,
        workspace: Option<WorkspaceId>,
    ) -> Result<Principal, CloudError> {
        let db = self.registry.connection()?;
        let row: Option<(String,i64,String)> = db.query_row("SELECT t.account_id,t.expires_at,w.id FROM agent_tokens t JOIN accounts a ON a.id=t.account_id JOIN workspaces w ON w.personal_account_id=a.id WHERE t.id=?1 AND t.revoked_at IS NULL AND t.expires_at>?2 AND a.suspended_at IS NULL",params![id,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db_error)?;
        let (account, expires, personal) = row.ok_or_else(denied)?;
        drop(db);
        let workspace = workspace.unwrap_or(WorkspaceId(
            Uuid::parse_str(&personal).map_err(|_| denied())?,
        ));
        self.check_member(&account, workspace, false)?;
        Ok(Principal {
            read_only: false,
            token_id: Some(id.into()),
            kind: PrincipalKind::Agent,
            expires_at: Some(expires as u64),
            account_id: Some(account),
            workspace_id: Some(workspace),
            bog_id: None,
        })
    }
    pub(crate) fn revoke_oauth_agent(&self, id: &str) -> Result<(), CloudError> {
        let mut db = self.registry.connection()?;
        let tx = db.transaction().map_err(db_error)?;
        tx.execute(
            "UPDATE agent_tokens SET revoked_at=COALESCE(revoked_at,?1) WHERE id=?2",
            params![now(), id],
        )
        .map_err(db_error)?;
        tx.execute("INSERT INTO audit_events(account_id,action,resource_id,created_at) SELECT account_id,'agent_token_revoked',id,?1 FROM agent_tokens WHERE id=?2",params![now(),id]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    pub(crate) fn check_agent_token(&self, p: &Principal) -> Result<(), CloudError> {
        let Some(id) = &p.token_id else { return Ok(()) };
        let db = self.registry.connection()?;
        let active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM agent_tokens WHERE id=?1 AND account_id=?2 AND revoked_at IS NULL AND expires_at>?3)",params![id,p.account_id,now()],|r|r.get(0)).map_err(db_error)?;
        if !active {
            return Err(denied());
        }
        Ok(())
    }
    pub(crate) fn audit_agent_approval(
        &self,
        p: &Principal,
        approved: bool,
    ) -> Result<(), CloudError> {
        let account = self.human_account(p)?;
        self.registry
            .connection()?
            .execute(
                "INSERT INTO audit_events(account_id,action,created_at) VALUES(?1,?2,?3)",
                params![
                    account,
                    if approved {
                        "device_approved"
                    } else {
                        "device_denied"
                    },
                    now()
                ],
            )
            .map_err(db_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn migration_backup_expiry_membership_and_suspension_preserve_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.sqlite");
        // An actual v2 database is upgraded, including a legacy Bog and scoped token.
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../migrations/001_registry.sql"))
            .unwrap();
        db.execute_batch(include_str!("../migrations/002_workspaces.sql"))
            .unwrap();
        let old_bog = Uuid::new_v4();
        let old_token = Uuid::new_v4().to_string();
        let old_secret = format!("{old_token}.{}", random_secret().unwrap());
        db.execute("INSERT INTO bogs(id,workspace_id,name,template,template_version,desired_state,observed_state,created_at) VALUES(?1,?2,'old','records-v1','records-v1','stopped','stopped',?3)",params![old_bog.to_string(),WorkspaceId::legacy().to_string(),now()]).unwrap();
        db.execute("INSERT INTO tokens(id,bog_id,secret_hash,scope,created_at,workspace_id) VALUES(?1,?2,?3,'write',?4,?5)",params![old_token,old_bog.to_string(),hash(old_secret.as_bytes()),now(),WorkspaceId::legacy().to_string()]).unwrap();
        drop(db);
        let registry = Arc::new(crate::Registry::open(&path).unwrap());
        let auth = Auth::new(registry.clone(), "operator-credential-longer-than-32-bytes").unwrap();
        let bog = registry.get(crate::BogId(old_bog)).unwrap();
        assert!(auth.authenticate(&old_secret).is_ok());
        let identity = crate::oauth::VerifiedIdentity::native("42", (now() + 43200) as u64);
        let (account, w) = auth.provision_identity(&identity).unwrap();
        assert_eq!(auth.provision_identity(&identity).unwrap().0.id, account.id);
        let human = auth.principal_from_verified(&identity, w.id).unwrap();
        let other = crate::oauth::VerifiedIdentity::native("43", (now() + 43200) as u64);
        let (_, other_w) = auth.provision_identity(&other).unwrap();
        assert_ne!(w.id, other_w.id);
        let issued = auth.issue_agent_token(&human, "persisted").unwrap();
        let principal = auth.authenticate_agent_token(&issued.secret, None).unwrap();
        assert!(
            auth.authenticate_agent_token(&issued.secret, Some(other_w.id))
                .is_err()
        );
        {
            let db = registry.connection().unwrap();
            let digest: Vec<u8> = db
                .query_row(
                    "SELECT secret_hash FROM agent_tokens WHERE id=?1",
                    [&issued.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(digest, hash(issued.secret.as_bytes()));
            assert_ne!(digest, issued.secret.as_bytes());
            db.execute(
                "DELETE FROM memberships WHERE account_id=?1 AND workspace_id=?2",
                params![account.id, w.id.to_string()],
            )
            .unwrap();
        }
        assert!(auth.authenticate_agent_token(&issued.secret, None).is_err());
        assert!(auth.authorize(&principal, None, false).is_err());
        {
            let db = registry.connection().unwrap();
            db.execute(
                "INSERT INTO memberships VALUES(?1,?2,'owner')",
                params![w.id.to_string(), account.id],
            )
            .unwrap();
            db.execute(
                "UPDATE accounts SET suspended_at=?1 WHERE id=?2",
                params![now(), account.id],
            )
            .unwrap();
        }
        assert!(auth.authenticate_agent_token(&issued.secret, None).is_err());
        assert!(auth.issue_agent_token(&human, "suspended").is_err());
        {
            let db = registry.connection().unwrap();
            db.execute(
                "UPDATE accounts SET suspended_at=NULL WHERE id=?1",
                [&account.id],
            )
            .unwrap();
            db.execute(
                "UPDATE agent_tokens SET expires_at=?1 WHERE id=?2",
                params![now() - 1, issued.id],
            )
            .unwrap();
        }
        assert!(auth.authenticate_agent_token(&issued.secret, None).is_err());
        let revoked = auth.issue_agent_token(&human, "revoked").unwrap();
        auth.revoke_agent_token(&human, &revoked.id).unwrap();
        // Registry backup is a consistent SQLite copy, as in the encrypted backup workflow.
        let restored = dir.path().join("restored.sqlite");
        registry
            .connection()
            .unwrap()
            .execute("VACUUM INTO ?1", [restored.to_str().unwrap()])
            .unwrap();
        let copy = Arc::new(crate::Registry::open(&restored).unwrap());
        let restored_auth =
            Auth::new(copy.clone(), "operator-credential-longer-than-32-bytes").unwrap();
        assert!(restored_auth.authenticate(&old_secret).is_ok());
        assert_eq!(copy.get(bog.id).unwrap().name, "old");
        assert!(
            restored_auth
                .authenticate_agent_token(&issued.secret, None)
                .is_err()
        );
        let listed = restored_auth.list_agent_tokens(&human).unwrap();
        assert!(
            listed
                .iter()
                .any(|v| v["name"] == "persisted" && v["id"] == issued.id)
        );
        assert!(
            listed
                .iter()
                .any(|v| v["id"] == revoked.id && !v["revoked_at"].is_null())
        );
        assert!(
            restored_auth
                .authenticate_agent_token(&revoked.secret, None)
                .is_err()
        );
        assert_eq!(
            copy.connection()
                .unwrap()
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            5
        );
    }
}
