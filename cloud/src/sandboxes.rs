//! Durable, bounded temporary databases and frozen owner cleanup selections.
use crate::registry::{db_error, now};
use crate::{Auth, Bog, BogId, CloudError, ObservedState, Principal, PrincipalKind, Registry};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub(crate) fn check_live_principal(
    db: &Connection,
    p: &Principal,
    at: i64,
) -> Result<(), CloudError> {
    if p.read_only || p.expires_at.is_some_and(|e| e <= at.max(0) as u64) {
        return Err(CloudError::new(
            "forbidden",
            "active write credential required",
        ));
    }
    if p.kind() == PrincipalKind::Agent {
        // External verified agents may have no native credential identifier.
        let Some(id) = p.token_id.as_deref() else {
            return Ok(());
        };
        let active: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM agent_tokens WHERE id=?1 AND account_id=?2 AND revoked_at IS NULL AND expires_at>?3)", params![id,p.account_id(),at], |r|r.get(0)).map_err(db_error)?;
        if !active {
            return Err(CloudError::new("unauthorized", "invalid credentials"));
        }
    }
    Ok(())
}
impl Registry {
    pub fn create_sandbox_defined(
        &self,
        p: &Principal,
        name: &str,
        key: &str,
        definition: &bog_definition::Definition,
    ) -> Result<Bog, CloudError> {
        if !matches!(p.kind(), PrincipalKind::Human | PrincipalKind::Agent) {
            return Err(CloudError::new("forbidden", "workspace identity required"));
        }
        if p.kind() == PrincipalKind::Agent && p.token_id.is_none() {
            return Err(CloudError::new(
                "forbidden",
                "sandbox requires a durable creator credential",
            ));
        }
        self.create_in_workspace(
            p.workspace_id()
                .ok_or_else(|| CloudError::new("forbidden", "workspace required"))?,
            name,
            "records-v1",
            key,
            3,
            3,
            ObservedState::Creating,
            Some(p),
            32,
            Some(definition),
            true,
        )
    }
    pub fn sandbox_metadata(&self, id: BogId) -> Result<Option<serde_json::Value>, CloudError> {
        self.connection()?.query_row("SELECT expires_at,creator_credential FROM sandboxes WHERE bog_id=?1", [id.to_string()], |r| Ok(serde_json::json!({"expires_at":r.get::<_,i64>(0)?,"creator_credential":r.get::<_,String>(1)?}))).optional().map_err(db_error)
    }
    pub(crate) fn check_sandbox_expiry(&self, id: BogId, at: i64) -> Result<(), CloudError> {
        let expired: bool = self
            .connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sandboxes WHERE bog_id=?1 AND expires_at<=?2)",
                params![id.to_string(), at],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if expired {
            Err(CloudError::new("not_found", "sandbox has expired"))
        } else {
            Ok(())
        }
    }
    /// Tombstoning and credential revocation precede filesystem cleanup and survive restart.
    pub fn expire_sandboxes(&self, at: i64) -> Result<usize, CloudError> {
        let mut db = self.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        tx.execute("UPDATE tokens SET revoked_at=?1 WHERE revoked_at IS NULL AND bog_id IN (SELECT bog_id FROM sandboxes WHERE expires_at<=?1)",[at]).map_err(db_error)?;
        let n=tx.execute("UPDATE bogs SET deleted_at=?1,desired_state='stopped' WHERE deleted_at IS NULL AND id IN (SELECT bog_id FROM sandboxes WHERE expires_at<=?1)",[at]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(n)
    }
}
#[derive(Serialize)]
pub struct CleanupPreview {
    pub id: String,
    pub bog_ids: Vec<BogId>,
}
#[derive(Serialize)]
pub struct CleanupResult {
    pub bog_id: BogId,
    pub status: String,
}
impl Auth {
    pub(crate) fn sandbox_agent_delete(&self, p: &Principal, bog: BogId) -> Result<(), CloudError> {
        let db = self.registry.connection()?;
        check_live_principal(&db, p, now())?;
        let workspace = p
            .workspace_id()
            .ok_or_else(|| CloudError::new("forbidden", "workspace required"))?;
        crate::workspace::member(
            &db,
            p.account_id()
                .ok_or_else(|| CloudError::new("forbidden", "account required"))?,
            workspace,
            false,
        )?;
        sandbox_creator(&db, p, bog)
    }
    pub fn preview_cleanup(
        &self,
        p: &Principal,
        prefix: &str,
    ) -> Result<CleanupPreview, CloudError> {
        let (workspace, _) = self.human_owner(p)?;
        if prefix.is_empty() || prefix.len() > 128 || prefix.chars().any(char::is_control) {
            return Err(CloudError::new(
                "invalid_request",
                "nonempty name prefix required",
            ));
        }
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        crate::workspace::member(&tx, p.account_id().unwrap(), workspace, true)?;
        let ids = {
            let mut stmt=tx.prepare("SELECT id FROM bogs WHERE workspace_id=?1 AND deleted_at IS NULL AND substr(name,1,length(?2))=?2 ORDER BY id").map_err(db_error)?;
            stmt.query_map(params![workspace.to_string(), prefix], |r| {
                r.get::<_, String>(0)
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?
        };
        let preview = CleanupPreview {
            id: uuid::Uuid::new_v4().to_string(),
            bog_ids: ids
                .iter()
                .map(|s| {
                    uuid::Uuid::parse_str(s)
                        .map(BogId)
                        .map_err(|_| CloudError::new("unavailable", "invalid database identifier"))
                })
                .collect::<Result<_, _>>()?,
        };
        tx.execute(
            "INSERT INTO cleanup_previews(id,workspace_id,ids) VALUES(?1,?2,?3)",
            params![
                preview.id,
                workspace.to_string(),
                serde_json::to_string(&ids)
                    .map_err(|_| CloudError::new("unavailable", "invalid cleanup selection"))?
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(preview)
    }
    pub fn execute_cleanup(
        &self,
        p: &Principal,
        preview_id: &str,
    ) -> Result<Vec<CleanupResult>, CloudError> {
        let (workspace, _) = self.human_owner(p)?;
        let raw: String = self
            .registry
            .connection()?
            .query_row(
                "SELECT ids FROM cleanup_previews WHERE id=?1 AND workspace_id=?2",
                params![preview_id, workspace.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| CloudError::new("not_found", "cleanup preview not found"))?;
        let ids: Vec<BogId> = serde_json::from_str(&raw)
            .map_err(|_| CloudError::new("unavailable", "invalid cleanup selection"))?;
        Ok(ids
            .into_iter()
            .map(|id| CleanupResult {
                bog_id: id,
                status: match self.delete_bog(p, id, id) {
                    Ok(()) => "deleted".into(),
                    Err(e) => e.code,
                },
            })
            .collect())
    }
}
pub(crate) fn sandbox_creator(
    db: &Connection,
    p: &Principal,
    bog: BogId,
) -> Result<(), CloudError> {
    let owned:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sandboxes s JOIN bogs b ON b.id=s.bog_id WHERE s.bog_id=?1 AND b.workspace_id=?2 AND s.creator_credential=?3)",params![bog.to_string(),p.workspace_id().map(|w|w.to_string()),p.token_id],|r|r.get(0)).map_err(db_error)?;
    if owned {
        Ok(())
    } else {
        Err(CloudError::new(
            "forbidden",
            "only the creating credential may delete this sandbox",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn setup() -> (tempfile::TempDir, Arc<Registry>, Auth, Principal) {
        let dir = tempfile::tempdir().unwrap();
        let r = Arc::new(Registry::open(&dir.path().join("registry.sqlite")).unwrap());
        let a = Auth::new(r.clone(), &"x".repeat(32)).unwrap();
        let (account, workspace) = a.provision_subject("sandbox-test", "owner").unwrap();
        let p = Principal {
            read_only: false,
            token_id: None,
            kind: PrincipalKind::Human,
            expires_at: None,
            account_id: Some(account.id),
            workspace_id: Some(workspace.id),
            bog_id: None,
        };
        (dir, r, a, p)
    }
    #[test]
    fn sandbox_slot_is_atomic_separate_and_expiry_durable() {
        let (dir, r, _, p) = setup();
        for i in 0..3 {
            r.create_defined(
                &p,
                &format!("normal-{i}"),
                &format!("normal-{i}"),
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
        }
        let winners = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for i in 0..6 {
                let path = dir.path().join("registry.sqlite");
                let p = p.clone();
                handles.push(scope.spawn(move || {
                    Registry::open(&path).unwrap().create_sandbox_defined(
                        &p,
                        &format!("sandbox-{i}"),
                        &format!("sandbox-{i}"),
                        &bog_definition::Definition::records_v1(),
                    )
                }));
            }
            handles
                .into_iter()
                .filter_map(|h| h.join().unwrap().ok())
                .collect::<Vec<_>>()
        });
        assert_eq!(winners.len(), 1);
        let id = winners[0].id;
        let expiry = r.sandbox_metadata(id).unwrap().unwrap()["expires_at"]
            .as_i64()
            .unwrap();
        assert!(r.check_sandbox_expiry(id, expiry - 1).is_ok());
        assert!(r.check_sandbox_expiry(id, expiry).is_err());
        assert_eq!(r.expire_sandboxes(expiry).unwrap(), 1);
        let reopened = Registry::open(&dir.path().join("registry.sqlite")).unwrap();
        assert_eq!(reopened.pending_deletions().unwrap(), vec![id]);
        assert_eq!(reopened.list().unwrap().len(), 3);
        assert_eq!(reopened.expire_sandboxes(expiry + 1).unwrap(), 0);
        assert!(
            reopened
                .create_sandbox_defined(
                    &p,
                    "retry",
                    "retry",
                    &bog_definition::Definition::records_v1()
                )
                .is_err()
        );
        reopened.finish_delete(id).unwrap();
        reopened
            .create_sandbox_defined(
                &p,
                "replacement",
                "replacement",
                &bog_definition::Definition::records_v1(),
            )
            .unwrap();
    }
    #[test]
    fn sandbox_does_not_consume_normal_allowance_and_agent_can_delete_own() {
        let (_dir, r, a, p) = setup();
        let credential = a.issue_agent_token(&p, "creator").unwrap();
        let agent = a
            .authenticate_agent_token(&credential.secret, None)
            .unwrap();
        let sandbox = r
            .create_sandbox_defined(
                &agent,
                "sandbox",
                "sandbox",
                &bog_definition::Definition::records_v1(),
            )
            .unwrap();
        for i in 0..3 {
            r.create_defined(
                &p,
                &format!("normal-{i}"),
                &format!("normal-{i}"),
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
        }
        assert!(
            r.create_defined(
                &p,
                "fourth",
                "fourth",
                &bog_definition::Definition::records_v1(),
                8
            )
            .is_err()
        );
        a.delete_bog(&agent, sandbox.id, sandbox.id).unwrap();
        a.delete_bog(&agent, sandbox.id, sandbox.id).unwrap();
        assert!(r.is_deleted(sandbox.id).unwrap());
    }
    #[test]
    fn expired_sandbox_is_hidden_before_cleanup_without_hiding_normal_bogs() {
        let (_dir, r, _a, p) = setup();
        let normal = r
            .create_defined(
                &p,
                "normal",
                "normal",
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
        let sandbox = r
            .create_sandbox_defined(
                &p,
                "sandbox",
                "sandbox",
                &bog_definition::Definition::records_v1(),
            )
            .unwrap();
        r.connection()
            .unwrap()
            .execute(
                "UPDATE sandboxes SET expires_at=?1 WHERE bog_id=?2",
                params![now() - 1, sandbox.id.to_string()],
            )
            .unwrap();
        assert!(!r.is_deleted(sandbox.id).unwrap());
        assert!(r.get(sandbox.id).is_err());
        assert_eq!(
            r.list().unwrap().iter().map(|b| b.id).collect::<Vec<_>>(),
            vec![normal.id]
        );
        assert_eq!(
            r.list_scoped(p.workspace_id().unwrap())
                .unwrap()
                .iter()
                .map(|b| b.id)
                .collect::<Vec<_>>(),
            vec![normal.id]
        );
    }
    #[test]
    fn cleanup_freezes_selection_and_rechecks_owner() {
        let (_dir, r, a, p) = setup();
        let first = r
            .create_defined(
                &p,
                "test-one",
                "one",
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
        let preview = a.preview_cleanup(&p, "test-").unwrap();
        let second = r
            .create_defined(
                &p,
                "test-two",
                "two",
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
        assert_eq!(preview.bog_ids, vec![first.id]);
        assert_eq!(
            a.execute_cleanup(&p, &preview.id).unwrap()[0].status,
            "deleted"
        );
        assert_eq!(
            a.execute_cleanup(&p, &preview.id).unwrap()[0].status,
            "deleted"
        );
        assert!(r.get(second.id).is_ok());
        r.connection()
            .unwrap()
            .execute(
                "UPDATE memberships SET role='member' WHERE account_id=?1",
                [p.account_id().unwrap()],
            )
            .unwrap();
        assert!(a.execute_cleanup(&p, &preview.id).is_err());
    }
    #[test]
    fn only_creating_live_agent_can_delete_sandbox() {
        let (_dir, r, a, p) = setup();
        let one = a.issue_agent_token(&p, "one").unwrap();
        let two = a.issue_agent_token(&p, "two").unwrap();
        let first = a.authenticate_agent_token(&one.secret, None).unwrap();
        let second = a.authenticate_agent_token(&two.secret, None).unwrap();
        let sandbox = r
            .create_sandbox_defined(
                &first,
                "sandbox",
                "sandbox",
                &bog_definition::Definition::records_v1(),
            )
            .unwrap();
        assert!(a.delete_bog(&second, sandbox.id, sandbox.id).is_err());
        let ordinary = r
            .create_defined(
                &first,
                "normal",
                "normal",
                &bog_definition::Definition::records_v1(),
                8,
            )
            .unwrap();
        assert!(a.delete_bog(&first, ordinary.id, ordinary.id).is_err());
        a.revoke_agent_token(&p, &one.id).unwrap();
        assert!(a.delete_bog(&first, sandbox.id, sandbox.id).is_err());
        assert!(
            r.create_defined(
                &first,
                "revoked",
                "revoked",
                &bog_definition::Definition::records_v1(),
                8
            )
            .is_err()
        );
        a.delete_bog(&p, sandbox.id, sandbox.id).unwrap();
    }
}
