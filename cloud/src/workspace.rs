//! Workspace membership and credential operations. Verified identities are created only by OAuth verification.
use crate::{
    auth::{PrincipalKind, random_secret},
    registry::{db_error, hash, now},
    *,
};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
// The account override applies only through the personal workspace's account join.
// Prefer the workspace override when both flags independently remove the limit.
fn allowance_source(workspace_uncapped: bool, effective_uncapped: bool) -> &'static str {
    if workspace_uncapped {
        "workspace"
    } else if effective_uncapped {
        "account"
    } else {
        "default"
    }
}
fn forbidden() -> CloudError {
    CloudError::new("forbidden", "workspace access denied")
}
fn audit(
    db: &Connection,
    workspace: WorkspaceId,
    account: Option<&str>,
    action: &str,
    resource: &str,
) -> Result<(), CloudError> {
    db.execute("INSERT INTO audit_events(workspace_id,account_id,action,resource_id,created_at) VALUES(?1,?2,?3,?4,?5)",params![workspace.to_string(),account,action,resource,now()]).map_err(db_error)?;
    Ok(())
}
pub(crate) fn member(
    db: &Connection,
    account: &str,
    workspace: WorkspaceId,
    owner: bool,
) -> Result<(), CloudError> {
    let role:Option<String>=db.query_row("SELECT m.role FROM memberships m JOIN accounts a ON a.id=m.account_id JOIN workspaces w ON w.id=m.workspace_id WHERE m.account_id=?1 AND m.workspace_id=?2 AND a.suspended_at IS NULL AND w.suspended_at IS NULL AND w.deleted_at IS NULL",params![account,workspace.to_string()],|r|r.get(0)).optional().map_err(db_error)?;
    if role.is_none() || owner && role.as_deref() != Some("owner") {
        return Err(forbidden());
    }
    Ok(())
}
pub struct IssuedInvitation {
    pub id: String,
    pub secret: String,
    pub expires_at: i64,
}
impl Auth {
    pub(crate) fn check_member(
        &self,
        account: &str,
        workspace: WorkspaceId,
        owner: bool,
    ) -> Result<(), CloudError> {
        {
            let db = self.registry.connection()?;
            member(&db, account, workspace, owner)
        }
    }
    fn managed<'a>(
        &self,
        p: &'a Principal,
        owner: bool,
    ) -> Result<(WorkspaceId, &'a str), CloudError> {
        if !matches!(p.kind, PrincipalKind::Human | PrincipalKind::Agent) {
            return Err(forbidden());
        }
        self.authorize(p, None, false)?;
        let w = p.workspace_id.ok_or_else(forbidden)?;
        let a = p.account_id.as_deref().ok_or_else(forbidden)?;
        self.check_member(a, w, owner)?;
        Ok((w, a))
    }
    pub(crate) fn human_owner<'a>(
        &self,
        p: &'a Principal,
    ) -> Result<(WorkspaceId, &'a str), CloudError> {
        if p.kind != PrincipalKind::Human {
            return Err(forbidden());
        }
        self.managed(p, true)
    }
    pub fn provision_identity(
        &self,
        identity: &crate::oauth::VerifiedIdentity,
    ) -> Result<(Account, Workspace), CloudError> {
        self.provision_subject(identity.issuer(), identity.subject())
    }
    pub(crate) fn provision_subject(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<(Account, Workspace), CloudError> {
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        tx.execute("INSERT INTO accounts(id,issuer,subject,created_at) VALUES(?1,?2,?3,?4) ON CONFLICT(issuer,subject) DO NOTHING",params![Uuid::new_v4().to_string(),issuer,subject,now()]).map_err(db_error)?;
        let (account, suspended): (String, Option<i64>) = tx
            .query_row(
                "SELECT id,suspended_at FROM accounts WHERE issuer=?1 AND subject=?2",
                params![issuer, subject],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_error)?;
        if suspended.is_some() {
            return Err(forbidden());
        }
        let created_workspace=tx.execute("INSERT INTO workspaces(id,name,personal_account_id,created_at) VALUES(?1,'Personal',?2,?3) ON CONFLICT(personal_account_id) DO NOTHING",params![Uuid::new_v4().to_string(),account,now()]).map_err(db_error)?;
        let id: String = tx
            .query_row(
                "SELECT id FROM workspaces WHERE personal_account_id=?1",
                [&account],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if created_workspace > 0 {
            tx.execute(
                "INSERT INTO memberships VALUES(?1,?2,'owner')",
                params![id, account],
            )
            .map_err(db_error)?;
        }
        let role: Option<String> = tx
            .query_row(
                "SELECT role FROM memberships WHERE workspace_id=?1 AND account_id=?2",
                params![id, account],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        let (uncapped, effective): (bool,bool) = tx.query_row("SELECT w.uncapped_bogs,w.uncapped_bogs OR a.uncapped_bogs FROM workspaces w JOIN accounts a ON a.id=w.personal_account_id WHERE w.id=?1", [&id], |r| Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok((
            Account { id: account },
            Workspace {
                id: WorkspaceId(Uuid::parse_str(&id).map_err(|_| forbidden())?),
                name: "Personal".into(),
                uncapped_bogs: uncapped,
                effective_uncapped_bogs: effective,
                bog_limit_source: allowance_source(uncapped, effective).into(),
                bog_limit: if effective { None } else { Some(3) },
                personal: true,
                role: role.unwrap_or_default(),
            },
        ))
    }
    pub fn principal_from_verified(
        &self,
        identity: &crate::oauth::VerifiedIdentity,
        workspace: WorkspaceId,
    ) -> Result<Principal, CloudError> {
        self.verified_principal(identity, workspace, PrincipalKind::Human)
    }
    pub fn agent_from_verified(
        &self,
        identity: &crate::oauth::VerifiedIdentity,
        workspace: WorkspaceId,
    ) -> Result<Principal, CloudError> {
        self.verified_principal(identity, workspace, PrincipalKind::Agent)
    }
    fn verified_principal(
        &self,
        identity: &crate::oauth::VerifiedIdentity,
        workspace: WorkspaceId,
        kind: PrincipalKind,
    ) -> Result<Principal, CloudError> {
        let (account, _) = self.provision_identity(identity)?;
        self.check_member(&account.id, workspace, false)?;
        Ok(Principal {
            read_only: false,
            kind,
            expires_at: Some(identity.expires_at()),
            account_id: Some(account.id),
            workspace_id: Some(workspace),
            token_id: None,
            bog_id: None,
        })
    }
    pub fn list_workspaces(
        &self,
        identity: &crate::oauth::VerifiedIdentity,
    ) -> Result<Vec<Workspace>, CloudError> {
        let (account, _) = self.provision_identity(identity)?;
        self.workspaces_for_account(&account.id)
    }
    pub fn workspaces_for_principal(&self, p: &Principal) -> Result<Vec<Workspace>, CloudError> {
        self.authorize(p, None, false)?;
        if p.legacy_public_operator() {
            return Ok(vec![Workspace {
                id: WorkspaceId::legacy(),
                name: "Legacy".into(),
                uncapped_bogs: false,
                effective_uncapped_bogs: false,
                bog_limit_source: "legacy".into(),
                bog_limit: Some(self.legacy_bog_limit),
                personal: false,
                role: "owner".into(),
            }]);
        }
        self.workspaces_for_account(p.account_id.as_deref().ok_or_else(forbidden)?)
    }
    fn workspaces_for_account(&self, account: &str) -> Result<Vec<Workspace>, CloudError> {
        let db = self.registry.connection()?;
        let mut s=db.prepare("SELECT w.id,w.name,m.role,w.uncapped_bogs,w.uncapped_bogs OR COALESCE(a.uncapped_bogs,0),w.personal_account_id IS NOT NULL FROM workspaces w LEFT JOIN accounts a ON a.id=w.personal_account_id JOIN memberships m ON m.workspace_id=w.id WHERE m.account_id=?1 AND w.suspended_at IS NULL AND w.deleted_at IS NULL").map_err(db_error)?;
        s.query_map([account], |r| {
            let id: String = r.get(0)?;
            Ok(Workspace {
                id: WorkspaceId(Uuid::parse_str(&id).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?),
                name: r.get(1)?,
                role: r.get(2)?,
                uncapped_bogs: r.get(3)?,
                effective_uncapped_bogs: id != WorkspaceId::legacy().to_string()
                    && r.get::<_, bool>(4)?,
                bog_limit_source: if id == WorkspaceId::legacy().to_string() {
                    "legacy"
                } else {
                    allowance_source(r.get(3)?, r.get(4)?)
                }
                .into(),
                bog_limit: if id == WorkspaceId::legacy().to_string() {
                    Some(self.legacy_bog_limit)
                } else if r.get::<_, bool>(4)? {
                    None
                } else {
                    Some(3)
                },
                personal: r.get(5)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)
    }
    pub fn invite(&self, p: &Principal, role: &str) -> Result<IssuedInvitation, CloudError> {
        let (w, a) = self.human_owner(p)?;
        if !matches!(role, "owner" | "member") {
            return Err(CloudError::new("invalid_request", "invalid role"));
        }
        let id = Uuid::new_v4().to_string();
        let secret = format!("{id}.{}", random_secret()?);
        let expires_at = now() + 7 * 86400;
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        member(&tx, a, w, true)?;
        tx.execute("INSERT INTO invitations(id,workspace_id,issuer_account_id,secret_hash,role,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,w.to_string(),a,hash(secret.as_bytes()),role,expires_at,now()]).map_err(db_error)?;
        audit(&tx, w, Some(a), "invitation.created", &id)?;
        tx.commit().map_err(db_error)?;
        Ok(IssuedInvitation {
            id,
            secret,
            expires_at,
        })
    }
    fn accept_for_account(&self, a: &str, secret: &str) -> Result<WorkspaceId, CloudError> {
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let row:Option<(String,String,String,String)>=tx.query_row("SELECT id,workspace_id,role,issuer_account_id FROM invitations WHERE secret_hash=?1 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>?2",params![hash(secret.as_bytes()),now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(db_error)?;
        let (id, w, role, issuer) = row.ok_or_else(forbidden)?;
        let w = WorkspaceId(Uuid::parse_str(&w).map_err(|_| forbidden())?);
        member(&tx, &issuer, w, true)?;
        let active: bool = tx
            .query_row(
                "SELECT suspended_at IS NULL FROM accounts WHERE id=?1",
                [a],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !active {
            return Err(forbidden());
        }
        let membership_added=tx.execute("INSERT INTO memberships VALUES(?1,?2,?3) ON CONFLICT(workspace_id,account_id) DO NOTHING",params![w.to_string(),a,role]).map_err(db_error)?;
        tx.execute(
            "UPDATE invitations SET accepted_at=?2 WHERE id=?1",
            params![id, now()],
        )
        .map_err(db_error)?;
        audit(&tx, w, Some(a), "invitation.accepted", &id)?;
        tx.commit().map_err(db_error)?;
        drop(db);
        if membership_added > 0 {
            self.membership_event(w, "member_added");
        }
        Ok(w)
    }
    pub fn revoke_invitation(&self, p: &Principal, id: &str) -> Result<(), CloudError> {
        let (w, a) = self.human_owner(p)?;
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        member(&tx, a, w, true)?;
        let n = tx
            .execute(
                "UPDATE invitations SET revoked_at=?3 WHERE id=?1 AND workspace_id=?2",
                params![id, w.to_string(), now()],
            )
            .map_err(db_error)?;
        if n == 0 {
            return Err(CloudError::new("not_found", "invitation not found"));
        }
        audit(&tx, w, Some(a), "invitation.revoked", id)?;
        tx.commit().map_err(db_error)
    }
    pub fn list_members(&self, p: &Principal) -> Result<Vec<Member>, CloudError> {
        let (w, _) = self.managed(p, false)?;
        let db = self.registry.connection()?;
        let mut s = db
            .prepare(
                "SELECT account_id,role FROM memberships WHERE workspace_id=?1 ORDER BY account_id",
            )
            .map_err(db_error)?;
        s.query_map([w.to_string()], |r| {
            Ok(Member {
                account_id: r.get(0)?,
                role: r.get(1)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)
    }
    pub fn remove_member(&self, p: &Principal, account: &str) -> Result<(), CloudError> {
        let (w, a) = self.human_owner(p)?;
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        member(&tx, a, w, true)?;
        let owners:i64=tx.query_row("SELECT COUNT(*) FROM memberships WHERE workspace_id=?1 AND role='owner' AND account_id<>?2",params![w.to_string(),account],|r|r.get(0)).map_err(db_error)?;
        if owners == 0 {
            return Err(CloudError::new(
                "conflict",
                "workspace must retain an owner",
            ));
        }
        let removed = tx
            .execute(
                "DELETE FROM memberships WHERE workspace_id=?1 AND account_id=?2",
                params![w.to_string(), account],
            )
            .map_err(db_error)?;
        if removed == 0 {
            return Err(CloudError::new("not_found", "member not found"));
        }
        tx.execute("UPDATE tokens SET revoked_at=?3 WHERE workspace_id=?1 AND account_id=?2 AND revoked_at IS NULL",params![w.to_string(),account,now()]).map_err(db_error)?;
        tx.execute("UPDATE invitations SET revoked_at=?3 WHERE workspace_id=?1 AND issuer_account_id=?2 AND revoked_at IS NULL",params![w.to_string(),account,now()]).map_err(db_error)?;
        audit(&tx, w, Some(a), "member.removed", account)?;
        tx.commit().map_err(db_error)?;
        drop(db);
        self.membership_event(w, "member_removed");
        Ok(())
    }
    pub fn issue_app_token(
        &self,
        p: &Principal,
        bog: BogId,
        scope: Scope,
    ) -> Result<IssuedToken, CloudError> {
        let (w, a) = self.managed(p, false)?;
        self.registry.get_scoped(w, bog)?;
        let id = Uuid::new_v4().to_string();
        let secret = format!("{id}.{}", random_secret()?);
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        member(&tx, a, w, false)?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bogs WHERE id=?1 AND workspace_id=?2 AND deleted_at IS NULL)",params![bog.to_string(),w.to_string()],|r|r.get(0)).map_err(db_error)?;
        if !exists {
            return Err(CloudError::new("not_found", "database not found"));
        }
        tx.execute("INSERT INTO tokens(id,bog_id,secret_hash,scope,created_at,workspace_id,account_id,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id,bog.to_string(),hash(secret.as_bytes()),match scope{Scope::Read=>"read",Scope::Write=>"write"},now(),w.to_string(),a,now()+90*86400]).map_err(db_error)?;
        audit(&tx, w, Some(a), "token.issued", &id)?;
        tx.commit().map_err(db_error)?;
        self.credential_event(bog, "credential_issued", &id);
        Ok(IssuedToken { id, secret })
    }
    pub fn list_tokens(&self, p: &Principal) -> Result<Vec<TokenInfo>, CloudError> {
        let w = if p.legacy_public_operator() {
            self.authorize(p, None, false)?;
            WorkspaceId::legacy()
        } else {
            self.managed(p, false)?.0
        };
        let db = self.registry.connection()?;
        let mut s=db.prepare("SELECT id,bog_id,account_id,scope,created_at,expires_at,revoked_at FROM tokens WHERE workspace_id=?1").map_err(db_error)?;
        s.query_map([w.to_string()], |r| {
            let b: String = r.get(1)?;
            Ok(TokenInfo {
                id: r.get(0)?,
                bog_id: BogId(Uuid::parse_str(&b).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?),
                account_id: r.get(2)?,
                scope: r.get(3)?,
                created_at: r.get(4)?,
                expires_at: r.get(5)?,
                revoked_at: r.get(6)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)
    }
    pub fn revoke_app_token(&self, p: &Principal, id: &str) -> Result<(), CloudError> {
        let (w, a) = self.managed(p, true)?;
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        member(&tx, a, w, true)?;
        let n = tx
            .execute(
                "UPDATE tokens SET revoked_at=?3 WHERE workspace_id=?1 AND id=?2 AND revoked_at IS NULL",
                params![w.to_string(), id, now()],
            )
            .map_err(db_error)?;
        if n == 0 {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM tokens WHERE workspace_id=?1 AND id=?2)",
                    params![w.to_string(), id],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            return if exists {
                Ok(())
            } else {
                Err(CloudError::new("not_found", "token not found"))
            };
        }
        let bog: String = tx
            .query_row("SELECT bog_id FROM tokens WHERE id=?1", [id], |r| r.get(0))
            .map_err(db_error)?;
        audit(&tx, w, Some(a), "token.revoked", id)?;
        tx.commit().map_err(db_error)?;
        if let Ok(id_bog) = Uuid::parse_str(&bog) {
            self.credential_event(BogId(id_bog), "credential_revoked", id);
        }
        Ok(())
    }
    pub fn claim_legacy(
        &self,
        operator: &Principal,
        identity: &crate::oauth::VerifiedIdentity,
        configured_issuer: &str,
        configured_subject: &str,
    ) -> Result<WorkspaceId, CloudError> {
        self.claim_legacy_subject(
            operator,
            identity.issuer(),
            identity.subject(),
            configured_issuer,
            configured_subject,
        )
    }
    fn claim_legacy_subject(
        &self,
        operator: &Principal,
        issuer: &str,
        subject: &str,
        configured_issuer: &str,
        configured_subject: &str,
    ) -> Result<WorkspaceId, CloudError> {
        if operator.kind != PrincipalKind::Operator
            || issuer != configured_issuer
            || subject != configured_subject
        {
            return Err(forbidden());
        }
        let (a, _) = self.provision_subject(issuer, subject)?;
        let w = WorkspaceId::legacy();
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT account_id FROM memberships WHERE workspace_id=?1",
                [w.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if existing.as_ref().is_some_and(|v| v != &a.id) {
            return Err(CloudError::new(
                "conflict",
                "legacy workspace already claimed",
            ));
        }
        tx.execute(
            "INSERT OR IGNORE INTO memberships VALUES(?1,?2,'owner')",
            params![w.to_string(), a.id],
        )
        .map_err(db_error)?;
        audit(&tx, w, Some(&a.id), "legacy.claimed", &w.to_string())?;
        tx.commit().map_err(db_error)?;
        Ok(w)
    }
    pub fn suspend_account(&self, operator: &Principal, account: &str) -> Result<(), CloudError> {
        if operator.kind != PrincipalKind::Operator {
            return Err(forbidden());
        }
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let n = tx
            .execute(
                "UPDATE accounts SET suspended_at=?2 WHERE id=?1",
                params![account, now()],
            )
            .map_err(db_error)?;
        if n == 0 {
            return Err(CloudError::new("not_found", "account not found"));
        }
        tx.execute("INSERT INTO audit_events(action,resource_id,created_at) VALUES('account.suspended',?1,?2)",params![account,now()]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    pub fn suspend_workspace(
        &self,
        operator: &Principal,
        w: WorkspaceId,
    ) -> Result<(), CloudError> {
        if operator.kind != PrincipalKind::Operator {
            return Err(forbidden());
        }
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let n = tx
            .execute(
                "UPDATE workspaces SET suspended_at=?2 WHERE id=?1",
                params![w.to_string(), now()],
            )
            .map_err(db_error)?;
        if n == 0 {
            return Err(CloudError::new("not_found", "workspace not found"));
        }
        audit(&tx, w, None, "workspace.suspended", &w.to_string())?;
        tx.commit().map_err(db_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    fn setup() -> (tempfile::TempDir, Arc<Registry>, Auth) {
        let d = tempfile::tempdir().unwrap();
        let r = Arc::new(Registry::open(&d.path().join("registry.sqlite")).unwrap());
        let a = Auth::new(r.clone(), &"x".repeat(32)).unwrap();
        (d, r, a)
    }
    fn person(a: &Auth, subject: &str) -> Principal {
        let (account, w) = a.provision_subject("issuer", subject).unwrap();
        Principal {
            read_only: false,
            kind: PrincipalKind::Human,
            expires_at: None,
            account_id: Some(account.id),
            workspace_id: Some(w.id),
            token_id: None,
            bog_id: None,
        }
    }
    #[test]
    fn legacy_workspace_reports_configured_finite_limit() {
        let (_d, r, _a) = setup();
        let a = Auth::new_with_legacy_limit(r, "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", 17).unwrap();
        let mut operator = a.authenticate("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx").unwrap();
        operator.workspace_id = Some(WorkspaceId::legacy());
        let workspace = a.workspaces_for_principal(&operator).unwrap().remove(0);
        assert_eq!(workspace.bog_limit, Some(17));
        assert!(!workspace.effective_uncapped_bogs);
        assert_eq!(workspace.bog_limit_source, "legacy");
        assert!(!workspace.uncapped_bogs);
        let owner = person(&a, "owner");
        a.claim_legacy_subject(&operator, "issuer", "owner", "issuer", "owner")
            .unwrap();
        let legacy = a
            .workspaces_for_principal(&owner)
            .unwrap()
            .into_iter()
            .find(|w| w.id == WorkspaceId::legacy())
            .unwrap();
        assert_eq!(legacy.bog_limit, Some(17));
        assert!(!legacy.effective_uncapped_bogs);
        assert_eq!(legacy.bog_limit_source, "legacy");
        assert!(!legacy.uncapped_bogs);
    }
    #[test]
    fn concurrent_app_revocation_records_only_one_change() {
        let (d, r, a) = setup();
        let obs = Arc::new(crate::observability::Observability::open(d.path()).unwrap());
        let a = a.with_observability(obs.clone());
        let owner = person(&a, "revocation-owner");
        let stranger = person(&a, "revocation-stranger");
        let bog = r
            .create_for_principal(&owner, "revocation", "records-v1", "revocation", 32)
            .unwrap();
        let token = a.issue_app_token(&owner, bog.id, Scope::Write).unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    barrier.wait();
                    a.revoke_app_token(&owner, &token.id).unwrap();
                });
            }
        });
        a.revoke_app_token(&owner, &token.id).unwrap();
        assert_eq!(
            a.revoke_app_token(&stranger, &token.id).unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            a.revoke_app_token(&owner, "missing-token")
                .unwrap_err()
                .code,
            "not_found"
        );
        let db = r.connection().unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM audit_events WHERE action='token.revoked' AND resource_id=?1",
                [&token.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let events = obs.events(bog.id, None, 100).unwrap();
        assert_eq!(
            events["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "credential_revoked")
                .count(),
            1
        );
        drop(db);
        assert!(a.authenticate(&token.secret).is_err());
    }
    #[test]
    fn allowance_explanations_follow_live_flags_without_crossing_memberships() {
        let (_d, _r, a) = setup();
        let owner = person(&a, "allowance-owner");
        let guest = person(&a, "allowance-guest");
        let operator = a.authenticate(&"x".repeat(32)).unwrap();
        a.set_platform_operator(&operator, owner.account_id.as_deref().unwrap(), true)
            .unwrap();
        let personal = owner.workspace_id.unwrap();
        let shared = a
            .create_workspace(&owner, "Shared", "allowance-shared")
            .unwrap();
        assert_eq!(shared.bog_limit_source, "default");
        assert!(!shared.effective_uncapped_bogs);
        let selected = a.select_workspace(&owner, shared.id).unwrap();
        let invite = a.invite(&selected, "member").unwrap();
        a.accept_invitation_for_principal(&guest, &invite.secret)
            .unwrap();
        let check = |id: WorkspaceId, local: bool, effective: bool, source: &str| {
            let rows = a.workspaces_for_principal(&owner).unwrap();
            let w = rows.iter().find(|w| w.id == id).unwrap();
            assert_eq!(w.uncapped_bogs, local);
            assert_eq!(w.effective_uncapped_bogs, effective);
            assert_eq!(w.bog_limit_source, source);
            assert_eq!(w.bog_limit, if effective { None } else { Some(3) });
            let platform = a.platform_workspaces(&owner).unwrap();
            let row = platform.iter().find(|w| w["id"] == id.to_string()).unwrap();
            assert_eq!(row["uncapped_bogs"], local);
            assert_eq!(row["effective_uncapped_bogs"], effective);
            assert_eq!(row["bog_limit_source"], source);
            assert_eq!(row["bog_limit"], serde_json::to_value(w.bog_limit).unwrap());
        };
        a.set_account_uncapped(&owner, owner.account_id.as_deref().unwrap(), true)
            .unwrap();
        check(personal, false, true, "account");
        check(shared.id, false, false, "default");
        let (_, provisioned) = a.provision_subject("issuer", "allowance-owner").unwrap();
        assert!(!provisioned.uncapped_bogs);
        assert!(provisioned.effective_uncapped_bogs);
        assert_eq!(provisioned.bog_limit_source, "account");
        a.set_workspace_uncapped(&owner, personal, true).unwrap();
        check(personal, true, true, "workspace");
        a.set_account_uncapped(&owner, owner.account_id.as_deref().unwrap(), false)
            .unwrap();
        check(personal, true, true, "workspace");
        a.set_workspace_uncapped(&owner, personal, false).unwrap();
        check(personal, false, false, "default");
        a.set_workspace_uncapped(&owner, shared.id, true).unwrap();
        check(shared.id, true, true, "workspace");
        let replay = a
            .create_workspace(&owner, "Shared", "allowance-shared")
            .unwrap();
        assert!(replay.effective_uncapped_bogs);
        assert_eq!(replay.bog_limit_source, "workspace");
        let guest_rows = a.workspaces_for_principal(&guest).unwrap();
        assert!(!guest_rows.iter().any(|w| w.id == personal));
        let guest_shared = guest_rows.iter().find(|w| w.id == shared.id).unwrap();
        assert!(guest_shared.effective_uncapped_bogs);
        assert_eq!(guest_shared.bog_limit_source, "workspace");
        assert_eq!(a.platform_workspaces(&guest).unwrap_err().code, "forbidden");
        a.set_workspace_uncapped(&owner, shared.id, false).unwrap();
        check(shared.id, false, false, "default");
    }
    #[test]
    fn uncapped_flags_are_scoped_revocable_and_still_globally_bounded() {
        let (_d, r, a) = setup();
        let owner = person(&a, "owner");
        let guest = person(&a, "guest");
        let operator = a.authenticate(&"x".repeat(32)).unwrap();
        a.set_platform_operator(&operator, owner.account_id.as_deref().unwrap(), true)
            .unwrap();
        assert!(a.is_platform_operator(&owner).unwrap());
        let org = a.create_workspace(&owner, "Flower", "org").unwrap();
        let selected = a.select_workspace(&owner, org.id).unwrap();
        a.set_account_uncapped(&owner, owner.account_id.as_deref().unwrap(), true)
            .unwrap();
        for i in 0..4 {
            r.create_for_principal(&owner, &format!("p{i}"), "records-v1", &format!("p{i}"), 32)
                .unwrap();
        }
        for i in 0..3 {
            r.create_for_principal(
                &selected,
                &format!("s{i}"),
                "records-v1",
                &format!("s{i}"),
                32,
            )
            .unwrap();
        }
        assert_eq!(
            r.create_for_principal(&selected, "s3", "records-v1", "s3", 32)
                .unwrap_err()
                .code,
            "capacity"
        );
        let invite = a.invite(&selected, "member").unwrap();
        a.accept_invitation_for_principal(&guest, &invite.secret)
            .unwrap();
        let guest_org = a.select_workspace(&guest, org.id).unwrap();
        a.set_workspace_uncapped(&owner, org.id, true).unwrap();
        r.create_for_principal(&guest_org, "s3", "records-v1", "s3", 32)
            .unwrap();
        assert_eq!(
            a.workspaces_for_principal(&guest_org)
                .unwrap()
                .iter()
                .find(|w| w.id == org.id)
                .unwrap()
                .bog_limit,
            None
        );
        a.set_workspace_uncapped(&owner, org.id, false).unwrap();
        assert_eq!(r.list_scoped(org.id).unwrap().len(), 4);
        assert_eq!(
            r.create_for_principal(&guest_org, "s4", "records-v1", "s4", 32)
                .unwrap_err()
                .code,
            "capacity"
        );
        a.set_account_uncapped(&owner, owner.account_id.as_deref().unwrap(), false)
            .unwrap();
        assert_eq!(r.list_scoped(owner.workspace_id.unwrap()).unwrap().len(), 4);
        assert_eq!(
            r.create_for_principal(&owner, "p4", "records-v1", "p4", 32)
                .unwrap_err()
                .code,
            "capacity"
        );
        a.set_workspace_uncapped(&owner, org.id, true).unwrap();
        for i in 8..32 {
            r.create_for_principal(
                &selected,
                &format!("g{i}"),
                "records-v1",
                &format!("g{i}"),
                1000,
            )
            .unwrap();
        }
        assert_eq!(
            r.create_for_principal(&selected, "overflow", "records-v1", "overflow", 1000)
                .unwrap_err()
                .code,
            "capacity"
        );
        a.set_platform_operator(&operator, owner.account_id.as_deref().unwrap(), false)
            .unwrap();
        assert!(a.set_workspace_uncapped(&owner, org.id, false).is_err());
    }
    #[test]
    fn shared_workspace_requests_are_exact_bounded_and_human_only() {
        let (_d, r, a) = setup();
        let owner = person(&a, "owner");
        let operator = a.authenticate(&"x".repeat(32)).unwrap();
        let org = a.create_workspace(&owner, "Flower", "org").unwrap();
        assert_eq!(
            a.create_workspace(&owner, "Flower", "org").unwrap().id,
            org.id
        );
        assert_eq!(
            a.create_workspace(&owner, "flower", "org")
                .unwrap_err()
                .code,
            "conflict"
        );
        assert!(a.create_workspace(&owner, " Flower", "bad").is_err());
        let bog = r
            .create_for_principal(&owner, "app", "records-v1", "app", 32)
            .unwrap();
        let token = a.issue_app_token(&owner, bog.id, Scope::Write).unwrap();
        let app = a.authenticate(&token.secret).unwrap();
        let mut agent = owner.clone();
        agent.kind = PrincipalKind::Agent;
        a.set_platform_operator(&operator, owner.account_id.as_deref().unwrap(), true)
            .unwrap();
        for denied in [&agent, &app, &operator] {
            assert!(a.create_workspace(denied, "Denied", "denied").is_err());
            assert!(a.set_workspace_uncapped(denied, org.id, true).is_err());
            assert!(
                a.set_account_uncapped(denied, owner.account_id.as_deref().unwrap(), true)
                    .is_err()
            );
            assert!(a.platform_accounts(denied).is_err());
            assert!(a.platform_workspaces(denied).is_err());
        }
        let other = person(&a, "other");
        assert!(a.set_workspace_uncapped(&other, org.id, true).is_err());
        for i in 1..20 {
            a.create_workspace(&owner, &format!("Org {i}"), &format!("org{i}"))
                .unwrap();
        }
        assert_eq!(
            a.create_workspace(&owner, "Overflow", "overflow")
                .unwrap_err()
                .code,
            "capacity"
        );
        assert_eq!(
            a.create_workspace(&owner, "Flower", "org").unwrap().id,
            org.id
        );
        assert!(
            a.bootstrap_workspace(
                &operator,
                other.account_id.as_deref().unwrap(),
                "Bootstrap",
                "boot"
            )
            .is_ok()
        );
        assert!(
            a.bootstrap_workspace(
                &owner,
                other.account_id.as_deref().unwrap(),
                "Denied",
                "boot"
            )
            .is_err()
        );
    }
    #[test]
    fn legacy_public_operator_cannot_cross_workspaces_or_manage_membership() {
        let (_d, r, a) = setup();
        let human = person(&a, "other");
        let foreign = r
            .create_for_principal(&human, "private", "records-v1", "private", 32)
            .unwrap();
        let legacy = r.create("legacy", "records-v1", "legacy").unwrap();
        let mut operator = a.authenticate(&"x".repeat(32)).unwrap();
        operator.workspace_id = Some(WorkspaceId::legacy());
        assert_eq!(
            a.authorize(&operator, Some(foreign.id), false)
                .unwrap_err()
                .code,
            "not_found"
        );
        assert!(a.issue(&operator, foreign.id, Scope::Write).is_err());
        assert!(a.delete_bog(&operator, foreign.id, foreign.id).is_err());
        assert!(
            a.select_workspace(&operator, human.workspace_id.unwrap())
                .is_err()
        );
        assert!(a.invite(&operator, "member").is_err());
        assert!(
            a.remove_member(&operator, human.account_id.as_deref().unwrap())
                .is_err()
        );
        let token = a.issue(&operator, legacy.id, Scope::Write).unwrap();
        assert_eq!(a.list_tokens(&operator).unwrap().len(), 1);
        a.revoke(&operator, legacy.id, &token.id).unwrap();
        assert!(a.authenticate(&token.secret).is_err());
        a.delete_bog(&operator, legacy.id, legacy.id).unwrap();
        a.delete_bog(&operator, legacy.id, legacy.id).unwrap();
        assert!(r.get(legacy.id).is_err());
    }
    #[test]
    fn agents_cannot_mutate_ownership_but_can_manage_app_credentials() {
        let (_d, r, a) = setup();
        let human = person(&a, "owner");
        let guest = person(&a, "guest");
        let mut agent = human.clone();
        agent.kind = PrincipalKind::Agent;
        let w = human.workspace_id.unwrap();
        let bog = r
            .create_for_principal(&agent, "db", "records-v1", "db", 32)
            .unwrap();
        assert_eq!(a.invite(&agent, "owner").err().unwrap().code, "forbidden");
        let revocable = a.invite(&human, "member").unwrap();
        assert_eq!(
            a.revoke_invitation(&agent, &revocable.id).unwrap_err().code,
            "forbidden"
        );
        a.revoke_invitation(&human, &revocable.id).unwrap();
        let invite = a.invite(&human, "member").unwrap();
        let mut guest_agent = guest.clone();
        guest_agent.kind = PrincipalKind::Agent;
        assert_eq!(
            a.accept_invitation_for_principal(&guest_agent, &invite.secret)
                .unwrap_err()
                .code,
            "forbidden"
        );
        assert_eq!(
            a.accept_invitation_for_principal(&guest, &invite.secret)
                .unwrap(),
            w
        );
        assert_eq!(
            a.remove_member(&agent, guest.account_id.as_deref().unwrap())
                .unwrap_err()
                .code,
            "forbidden"
        );
        a.remove_member(&human, guest.account_id.as_deref().unwrap())
            .unwrap();
        let token = a.issue_app_token(&agent, bog.id, Scope::Write).unwrap();
        assert!(
            a.list_tokens(&agent)
                .unwrap()
                .iter()
                .any(|t| t.id == token.id)
        );
        a.revoke_app_token(&agent, &token.id).unwrap();
        assert!(a.authenticate(&token.secret).is_err());
        assert_eq!(
            a.delete_bog(&agent, bog.id, bog.id).unwrap_err().code,
            "forbidden"
        );
        assert!(!r.is_deleted(bog.id).unwrap());
        a.delete_bog(&human, bog.id, bog.id).unwrap();
        assert!(r.is_deleted(bog.id).unwrap());
    }
    #[test]
    fn verified_principal_expiry_is_rechecked() {
        let (_d, _r, a) = setup();
        let mut p = person(&a, "expired");
        p.expires_at = Some(0);
        assert_eq!(
            a.authorize(&p, None, false).unwrap_err().code,
            "unauthorized"
        );
    }
    #[test]
    fn reprovisioning_does_not_restore_removed_owner_membership() {
        let (_d, _r, a) = setup();
        let p = person(&a, "original");
        let q = person(&a, "newowner");
        let invite = a.invite(&p, "owner").unwrap();
        a.accept_invitation_for_principal(&q, &invite.secret)
            .unwrap();
        let selected = a.select_workspace(&q, p.workspace_id.unwrap()).unwrap();
        a.remove_member(&selected, p.account_id.as_deref().unwrap())
            .unwrap();
        let (account, w) = a.provision_subject("issuer", "original").unwrap();
        assert_eq!(account.id, p.account_id.unwrap());
        assert!(w.role.is_empty());
        assert!(a.check_member(&account.id, w.id, false).is_err());
    }
    #[test]
    fn pending_cleanup_keeps_global_slot_and_removed_member_cannot_create() {
        let (_d, r, a) = setup();
        let p = person(&a, "owner");
        let q = person(&a, "other");
        let b = r
            .create_for_principal(&p, "one", "records-v1", "1", 1)
            .unwrap();
        a.delete_bog(&p, b.id, b.id).unwrap();
        assert_eq!(
            r.create_for_principal(&q, "two", "records-v1", "2", 1)
                .unwrap_err()
                .code,
            "capacity"
        );
        r.finish_delete(b.id).unwrap();
        assert!(
            r.create_for_principal(&q, "two", "records-v1", "2", 1)
                .is_ok()
        );
        let invite = a.invite(&p, "member").unwrap();
        a.accept_invitation_for_principal(&q, &invite.secret)
            .unwrap();
        let selected = a.select_workspace(&q, p.workspace_id.unwrap()).unwrap();
        a.remove_member(&p, q.account_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            r.create_for_principal(&selected, "denied", "records-v1", "3", 10)
                .unwrap_err()
                .code,
            "forbidden"
        );
    }
    #[test]
    fn legacy_claim_is_explicit_exact_and_idempotent() {
        let (_d, _r, a) = setup();
        let owner = person(&a, "owner");
        let operator = a.authenticate(&"x".repeat(32)).unwrap();
        assert!(
            a.claim_legacy_subject(&owner, "issuer", "owner", "issuer", "owner")
                .is_err()
        );
        assert!(
            a.claim_legacy_subject(&operator, "issuer", "stranger", "issuer", "owner")
                .is_err()
        );
        assert_eq!(
            a.claim_legacy_subject(&operator, "issuer", "owner", "issuer", "owner")
                .unwrap(),
            WorkspaceId::legacy()
        );
        assert_eq!(
            a.claim_legacy_subject(&operator, "issuer", "owner", "issuer", "owner")
                .unwrap(),
            WorkspaceId::legacy()
        );
        assert!(
            a.claim_legacy_subject(&operator, "issuer", "other", "issuer", "other")
                .is_err()
        );
    }
    #[test]
    fn bog_deletion_revokes_before_cleanup_and_preserves_nonce() {
        let (_d, r, a) = setup();
        let p = person(&a, "owner");
        let q = person(&a, "other");
        let w = p.workspace_id.unwrap();
        let b = r.create_scoped(w, "db", "records-v1", "1").unwrap();
        r.start_generation(b.id, "nonce").unwrap();
        let token = a.issue_app_token(&p, b.id, Scope::Write).unwrap();
        let app = a.authenticate(&token.secret).unwrap();
        assert!(a.delete_bog(&q, b.id, b.id).is_err());
        assert!(a.delete_bog(&p, b.id, BogId(Uuid::new_v4())).is_err());
        a.delete_bog(&p, b.id, b.id).unwrap();
        assert!(a.authorize(&app, Some(b.id), false).is_err());
        assert!(r.get(b.id).is_err());
        assert!(r.list_scoped(w).unwrap().is_empty());
        assert_eq!(r.startup_nonce(b.id).unwrap().as_deref(), Some("nonce"));
        assert_eq!(r.pending_deletions().unwrap(), vec![b.id]);
        r.finish_delete(b.id).unwrap();
        assert!(r.pending_deletions().unwrap().is_empty());
        assert!(r.is_deleted(b.id).unwrap());
    }
    #[test]
    fn scoped_identity_and_membership() {
        let (_d, r, a) = setup();
        let p = person(&a, "a");
        let q = person(&a, "b");
        let w = p.workspace_id.unwrap();
        assert_eq!(person(&a, "a").account_id, p.account_id);
        assert_eq!(person(&a, "a").workspace_id, p.workspace_id);
        let b = r.create_scoped(w, "same", "records-v1", "same").unwrap();
        let other = r
            .create_scoped(q.workspace_id.unwrap(), "same", "records-v1", "same")
            .unwrap();
        assert_ne!(b.id, other.id);
        assert!(a.authorize(&q, Some(b.id), false).is_err());
        assert_eq!(
            r.create_scoped(w, "same", "records-v1", "same").unwrap().id,
            b.id
        );
        assert!(r.get_scoped(q.workspace_id.unwrap(), b.id).is_err());
    }
    #[test]
    fn invitations_single_use_and_removal_revokes_tokens() {
        let (_d, r, a) = setup();
        let p = person(&a, "a");
        let mut q = person(&a, "b");
        let invite = a.invite(&p, "member").unwrap();
        let w = a
            .accept_for_account(q.account_id.as_deref().unwrap(), &invite.secret)
            .unwrap();
        assert!(
            a.accept_for_account(q.account_id.as_deref().unwrap(), &invite.secret)
                .is_err()
        );
        q.workspace_id = Some(w);
        let b = r.create_scoped(w, "db", "records-v1", "1").unwrap();
        let token = a.issue_app_token(&q, b.id, Scope::Write).unwrap();
        let app = a.authenticate(&token.secret).unwrap();
        assert!(a.issue_app_token(&app, b.id, Scope::Read).is_err());
        assert!(a.invite(&q, "owner").is_err());
        a.remove_member(&p, q.account_id.as_deref().unwrap())
            .unwrap();
        assert!(a.authorize(&app, Some(b.id), false).is_err());
        assert!(a.authenticate(&token.secret).is_err());
        assert!(a.authorize(&q, Some(b.id), false).is_err());
        assert!(
            a.remove_member(&p, p.account_id.as_deref().unwrap())
                .is_err()
        );
    }
    #[test]
    fn expiry_suspension_and_hash_storage() {
        let (_d, r, a) = setup();
        let p = person(&a, "a");
        let q = person(&a, "b");
        let i = a.invite(&p, "member").unwrap();
        r.connection()
            .unwrap()
            .execute("UPDATE invitations SET expires_at=0", [])
            .unwrap();
        assert!(
            a.accept_for_account(q.account_id.as_deref().unwrap(), &i.secret)
                .is_err()
        );
        let b = r
            .create_scoped(p.workspace_id.unwrap(), "db", "records-v1", "1")
            .unwrap();
        let token = a.issue_app_token(&p, b.id, Scope::Read).unwrap();
        let app = a.authenticate(&token.secret).unwrap();
        assert!(a.authorize(&app, Some(b.id), true).is_err());
        r.connection()
            .unwrap()
            .execute("UPDATE tokens SET expires_at=0", [])
            .unwrap();
        assert!(a.authenticate(&token.secret).is_err());
        assert!(a.authorize(&app, Some(b.id), false).is_err());
        let operator = a.authenticate(&"x".repeat(32)).unwrap();
        a.suspend_account(&operator, p.account_id.as_deref().unwrap())
            .unwrap();
        assert!(a.authorize(&p, Some(b.id), false).is_err());
        let events: String = r
            .connection()
            .unwrap()
            .query_row("SELECT group_concat(action) FROM audit_events", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(!events.contains(&token.secret));
        assert!(!events.contains(&i.secret));
    }
    #[test]
    fn scoped_quota_serializes_concurrent_connections() {
        let (d, r, a) = setup();
        let p = person(&a, "a");
        let w = p.workspace_id.unwrap();
        let path = d.path().join("registry.sqlite");
        let joins: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    Registry::open(&path).unwrap().create_scoped(
                        w,
                        &format!("db-{i}"),
                        "records-v1",
                        &format!("{i}"),
                    )
                })
            })
            .collect();
        let successes = joins
            .into_iter()
            .map(|j| j.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(successes, 3);
        assert_eq!(r.list_scoped(w).unwrap().len(), 3);
    }
    #[test]
    fn global_retained_limit_counts_stopped_bogs_across_accounts() {
        let (_d, r, a) = setup();
        for i in 0..32 {
            let p = person(&a, &format!("account-{}", i / 3));
            let b = r
                .create_for_principal(&p, &format!("db-{i}"), "records-v1", &format!("{i}"), 1000)
                .unwrap();
            r.set_desired(b.id, false).unwrap();
        }
        let p = person(&a, "overflow");
        assert_eq!(
            r.create_for_principal(&p, "overflow", "records-v1", "overflow", 1000)
                .unwrap_err()
                .code,
            "capacity"
        );
    }
    #[test]
    fn workspace_quota_counts_stopped_bogs() {
        let (_d, r, a) = setup();
        let w = person(&a, "a").workspace_id.unwrap();
        for i in 0..3 {
            let b = r
                .create_scoped(w, &format!("db-{i}"), "records-v1", &format!("{i}"))
                .unwrap();
            r.set_desired(b.id, false).unwrap();
        }
        assert_eq!(
            r.create_scoped(w, "overflow", "records-v1", "overflow")
                .unwrap_err()
                .code,
            "capacity"
        );
    }
}

impl Auth {
    pub fn authorize_delete_bog(&self, p: &Principal, bog: BogId) -> Result<(), CloudError> {
        if p.kind() == PrincipalKind::Agent {
            return self.sandbox_agent_delete(p, bog);
        }
        let workspace = if p.legacy_public_operator() {
            self.authorize(p, None, true)?;
            WorkspaceId::legacy()
        } else {
            self.human_owner(p)?.0
        };
        let exists: bool = self
            .registry
            .connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bogs WHERE id=?1 AND workspace_id=?2)",
                params![bog.to_string(), workspace.to_string()],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if exists {
            Ok(())
        } else {
            Err(CloudError::new("not_found", "database not found"))
        }
    }
    pub fn delete_bog(
        &self,
        p: &Principal,
        bog: BogId,
        confirmation: BogId,
    ) -> Result<(), CloudError> {
        self.authorize_delete_bog(p, bog)?;
        if bog != confirmation {
            return Err(CloudError::new(
                "invalid_request",
                "database confirmation does not match",
            ));
        }
        let (w, a) = if p.kind() == PrincipalKind::Agent {
            (p.workspace_id().ok_or_else(forbidden)?, p.account_id())
        } else if p.legacy_public_operator() {
            (WorkspaceId::legacy(), None)
        } else {
            let (w, a) = self.human_owner(p)?;
            (w, Some(a))
        };
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        if let Some(a) = a {
            member(&tx, a, w, p.kind() != PrincipalKind::Agent)?;
            crate::sandboxes::check_live_principal(&tx, p, now())?;
            if p.kind() == PrincipalKind::Agent {
                crate::sandboxes::sandbox_creator(&tx, p, bog)?;
            }
        }
        let building:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM definition_jobs WHERE bog_id=?1 AND status IN ('building','activating','recovery_required'))",[bog.to_string()],|r|r.get(0)).map_err(db_error)?;
        if building {
            return Err(CloudError::new(
                "conflict",
                "definition build is in progress",
            ));
        }
        let n=tx.execute("UPDATE bogs SET deleted_at=COALESCE(deleted_at,?3),desired_state='stopped' WHERE id=?1 AND workspace_id=?2",params![bog.to_string(),w.to_string(),now()]).map_err(db_error)?;
        if n == 0 {
            return Err(CloudError::new("not_found", "database not found"));
        }
        tx.execute(
            "UPDATE tokens SET revoked_at=?2 WHERE bog_id=?1 AND revoked_at IS NULL",
            params![bog.to_string(), now()],
        )
        .map_err(db_error)?;
        audit(&tx, w, a, "bog.deleted", &bog.to_string())?;
        tx.commit().map_err(db_error)
    }
}
impl Registry {
    pub fn is_deleted(&self, bog: BogId) -> Result<bool, CloudError> {
        self.connection()?
            .query_row(
                "SELECT deleted_at IS NOT NULL FROM bogs WHERE id=?1",
                [bog.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| CloudError::new("not_found", "database not found"))
    }
    pub fn pending_deletions(&self) -> Result<Vec<BogId>, CloudError> {
        let db = self.connection()?;
        let mut s = db
            .prepare(
                "SELECT id FROM bogs WHERE deleted_at IS NOT NULL AND cleanup_completed_at IS NULL",
            )
            .map_err(db_error)?;
        s.query_map([], |r| r.get::<_, String>(0))
            .map_err(db_error)?
            .map(|v| {
                let id = v.map_err(db_error)?;
                Uuid::parse_str(&id).map(BogId).map_err(|_| forbidden())
            })
            .collect()
    }
    pub fn finish_delete(&self, bog: BogId) -> Result<(), CloudError> {
        let n = self
            .connection()?
            .execute(
                "UPDATE bogs SET cleanup_completed_at=?2 WHERE id=?1 AND deleted_at IS NOT NULL",
                params![bog.to_string(), now()],
            )
            .map_err(db_error)?;
        if n == 0 {
            return Err(CloudError::new(
                "conflict",
                "database must be deleted before cleanup",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct InvitationInfo {
    pub id: String,
    pub workspace_id: WorkspaceId,
    pub workspace_name: String,
    pub role: String,
    pub expires_at: i64,
    pub accepted_at: Option<i64>,
    pub revoked_at: Option<i64>,
}
impl Auth {
    pub fn select_workspace(
        &self,
        p: &Principal,
        workspace: WorkspaceId,
    ) -> Result<Principal, CloudError> {
        if p.expires_at
            .is_some_and(|expiry| expiry <= now().max(0) as u64)
        {
            return Err(CloudError::new("unauthorized", "invalid credentials"));
        }
        match p.kind {
            PrincipalKind::Human | PrincipalKind::Agent => {
                let a = p.account_id.as_deref().ok_or_else(forbidden)?;
                self.check_member(a, workspace, false)?;
                let mut selected = p.clone();
                selected.workspace_id = Some(workspace);
                Ok(selected)
            }
            PrincipalKind::Operator | PrincipalKind::App => {
                self.authorize(p, p.bog_id, false)?;
                if p.workspace_id != Some(workspace) {
                    return Err(forbidden());
                }
                Ok(p.clone())
            }
        }
    }
    pub fn accept_invitation_for_principal(
        &self,
        p: &Principal,
        secret: &str,
    ) -> Result<WorkspaceId, CloudError> {
        if p.kind != PrincipalKind::Human {
            return Err(forbidden());
        }
        self.authorize(p, None, false)?;
        self.accept_for_account(p.account_id.as_deref().ok_or_else(forbidden)?, secret)
    }
    pub fn preview_invitation(
        &self,
        p: &Principal,
        secret: &str,
    ) -> Result<InvitationInfo, CloudError> {
        if p.kind != PrincipalKind::Human {
            return Err(forbidden());
        }
        self.authorize(p, None, false)?;
        let db = self.registry.connection()?;
        db.query_row("SELECT i.id,i.workspace_id,w.name,i.role,i.expires_at,i.accepted_at,i.revoked_at FROM invitations i JOIN workspaces w ON w.id=i.workspace_id JOIN memberships m ON m.workspace_id=i.workspace_id AND m.account_id=i.issuer_account_id JOIN accounts a ON a.id=i.issuer_account_id WHERE i.secret_hash=?1 AND i.accepted_at IS NULL AND i.revoked_at IS NULL AND i.expires_at>?2 AND w.suspended_at IS NULL AND w.deleted_at IS NULL AND a.suspended_at IS NULL AND m.role='owner'",params![hash(secret.as_bytes()),now()],invitation_row).optional().map_err(db_error)?.ok_or_else(forbidden)
    }
    pub fn list_invitations(&self, p: &Principal) -> Result<Vec<InvitationInfo>, CloudError> {
        let (w, _) = self.managed(p, true)?;
        let db = self.registry.connection()?;
        let mut s=db.prepare("SELECT i.id,i.workspace_id,w.name,i.role,i.expires_at,i.accepted_at,i.revoked_at FROM invitations i JOIN workspaces w ON w.id=i.workspace_id WHERE i.workspace_id=?1 ORDER BY i.created_at,i.id").map_err(db_error)?;
        s.query_map([w.to_string()], invitation_row)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }
}
fn invitation_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<InvitationInfo> {
    let w: String = r.get(1)?;
    Ok(InvitationInfo {
        id: r.get(0)?,
        workspace_id: WorkspaceId(Uuid::parse_str(&w).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?),
        workspace_name: r.get(2)?,
        role: r.get(3)?,
        expires_at: r.get(4)?,
        accepted_at: r.get(5)?,
        revoked_at: r.get(6)?,
    })
}

fn platform_access(db: &Connection, p: &Principal) -> Result<(), CloudError> {
    if p.kind != PrincipalKind::Human || p.expires_at.is_some_and(|e| e <= now().max(0) as u64) {
        return Err(forbidden());
    }
    let allowed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 AND platform_operator=1 AND suspended_at IS NULL)", [p.account_id.as_deref().ok_or_else(forbidden)?], |r| r.get(0)).map_err(db_error)?;
    if allowed { Ok(()) } else { Err(forbidden()) }
}
impl Auth {
    pub fn is_platform_operator(&self, p: &Principal) -> Result<bool, CloudError> {
        if p.kind != PrincipalKind::Human {
            return Ok(false);
        }
        self.authorize(p, None, false)?;
        let db = self.registry.connection()?;
        Ok(platform_access(&db, p).is_ok())
    }
    pub fn set_platform_operator(
        &self,
        operator: &Principal,
        target: &str,
        enabled: bool,
    ) -> Result<(), CloudError> {
        if operator.kind != PrincipalKind::Operator {
            return Err(forbidden());
        }
        self.authorize(operator, None, true)?;
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        if tx
            .execute(
                "UPDATE accounts SET platform_operator=?2 WHERE id=?1",
                params![target, enabled],
            )
            .map_err(db_error)?
            == 0
        {
            return Err(CloudError::new("not_found", "account not found"));
        }
        audit(
            &tx,
            WorkspaceId::legacy(),
            None,
            if enabled {
                "platform_operator.enabled"
            } else {
                "platform_operator.disabled"
            },
            target,
        )?;
        tx.commit().map_err(db_error)
    }
    pub fn set_account_uncapped(
        &self,
        p: &Principal,
        target: &str,
        enabled: bool,
    ) -> Result<(), CloudError> {
        self.authorize(p, None, false)?;
        self.change_uncapped(p, target, enabled, false, false)
    }
    pub fn set_workspace_uncapped(
        &self,
        p: &Principal,
        target: WorkspaceId,
        enabled: bool,
    ) -> Result<(), CloudError> {
        self.authorize(p, None, false)?;
        self.change_uncapped(p, &target.to_string(), enabled, true, false)
    }
    pub fn operator_set_account_uncapped(
        &self,
        p: &Principal,
        target: &str,
        enabled: bool,
    ) -> Result<(), CloudError> {
        if p.kind != PrincipalKind::Operator {
            return Err(forbidden());
        }
        self.authorize(p, None, true)?;
        self.change_uncapped(p, target, enabled, false, true)
    }
    pub fn operator_set_workspace_uncapped(
        &self,
        p: &Principal,
        target: WorkspaceId,
        enabled: bool,
    ) -> Result<(), CloudError> {
        if p.kind != PrincipalKind::Operator {
            return Err(forbidden());
        }
        self.authorize(p, None, true)?;
        self.change_uncapped(p, &target.to_string(), enabled, true, true)
    }
    fn change_uncapped(
        &self,
        p: &Principal,
        target: &str,
        enabled: bool,
        workspace: bool,
        bootstrap: bool,
    ) -> Result<(), CloudError> {
        if workspace && target == WorkspaceId::legacy().to_string() {
            return Err(CloudError::new(
                "invalid_request",
                "legacy workspace uses the server-configured limit",
            ));
        }
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        if !bootstrap {
            platform_access(&tx, p)?;
        }
        let sql = if workspace {
            "UPDATE workspaces SET uncapped_bogs=?2 WHERE id=?1 AND deleted_at IS NULL"
        } else {
            "UPDATE accounts SET uncapped_bogs=?2 WHERE id=?1"
        };
        if tx
            .execute(sql, params![target, enabled])
            .map_err(db_error)?
            == 0
        {
            return Err(CloudError::new("not_found", "target not found"));
        }
        tx.execute("INSERT INTO audit_events(workspace_id,account_id,action,resource_id,created_at) VALUES(?1,?2,?3,?4,?5)",params![if workspace {Some(target)}else{None},p.account_id,format!("{}.uncapped_{}",if workspace{"workspace"}else{"account"},if enabled{"enabled"}else{"disabled"}),target,now()]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    pub fn platform_accounts(&self, p: &Principal) -> Result<Vec<serde_json::Value>, CloudError> {
        self.authorize(p, None, false)?;
        let db = self.registry.connection()?;
        platform_access(&db, p)?;
        let mut s=db.prepare("SELECT id,issuer,subject,uncapped_bogs,platform_operator,suspended_at FROM accounts ORDER BY created_at,id").map_err(db_error)?;
        s.query_map([],|r| Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"issuer":r.get::<_,String>(1)?,"subject":r.get::<_,String>(2)?,"uncapped_bogs":r.get::<_,bool>(3)?,"platform_operator":r.get::<_,bool>(4)?,"suspended_at":r.get::<_,Option<i64>>(5)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)
    }
    pub fn platform_workspaces(&self, p: &Principal) -> Result<Vec<serde_json::Value>, CloudError> {
        self.authorize(p, None, false)?;
        let db = self.registry.connection()?;
        platform_access(&db, p)?;
        let mut s=db.prepare("SELECT w.id,w.name,w.personal_account_id,w.uncapped_bogs,w.uncapped_bogs OR COALESCE(a.uncapped_bogs,0),(SELECT COUNT(*) FROM bogs b WHERE b.workspace_id=w.id AND (b.deleted_at IS NULL OR b.cleanup_completed_at IS NULL)) FROM workspaces w LEFT JOIN accounts a ON a.id=w.personal_account_id WHERE w.deleted_at IS NULL AND w.id NOT IN ('00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000002') ORDER BY w.created_at,w.id").map_err(db_error)?;
        s.query_map([],|r| Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"personal_account_id":r.get::<_,Option<String>>(2)?,"personal":r.get::<_,Option<String>>(2)?.is_some(),"uncapped_bogs":r.get::<_,bool>(3)?,"effective_uncapped_bogs":r.get::<_,bool>(4)?,"bog_limit_source":allowance_source(r.get(3)?,r.get(4)?),"bog_limit":if r.get::<_,bool>(4)? {None} else {Some(3)},"bog_count":r.get::<_,i64>(5)?}))).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)
    }
    pub fn bootstrap_workspace(
        &self,
        operator: &Principal,
        owner_account: &str,
        name: &str,
        key: &str,
    ) -> Result<Workspace, CloudError> {
        if operator.kind != PrincipalKind::Operator {
            return Err(forbidden());
        }
        self.authorize(operator, None, true)?;
        let workspace: String=self.registry.connection()?.query_row("SELECT w.id FROM workspaces w JOIN accounts a ON a.id=w.personal_account_id WHERE a.id=?1 AND a.suspended_at IS NULL AND w.deleted_at IS NULL AND w.suspended_at IS NULL",[owner_account],|r|r.get(0)).optional().map_err(db_error)?.ok_or_else(forbidden)?;
        let human = Principal {
            read_only: false,
            kind: PrincipalKind::Human,
            account_id: Some(owner_account.into()),
            workspace_id: Some(WorkspaceId(
                Uuid::parse_str(&workspace).map_err(|_| forbidden())?,
            )),
            expires_at: None,
            token_id: None,
            bog_id: None,
        };
        self.create_workspace(&human, name, key)
    }
    pub fn create_workspace(
        &self,
        p: &Principal,
        name: &str,
        key: &str,
    ) -> Result<Workspace, CloudError> {
        if p.kind != PrincipalKind::Human {
            return Err(forbidden());
        }
        self.authorize(p, None, false)?;
        if name.is_empty()
            || name.len() > 80
            || name.trim() != name
            || name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
            || name == "."
            || name == ".."
            || key.is_empty()
            || key.len() > 128
            || key.chars().any(char::is_control)
        {
            return Err(CloudError::new(
                "invalid_request",
                "invalid workspace name or idempotency key",
            ));
        }
        let account = p.account_id.as_deref().ok_or_else(forbidden)?;
        let mut db = self.registry.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        member(&tx, account, p.workspace_id.ok_or_else(forbidden)?, false)?;
        let prior:Option<(String,String)>=tx.query_row("SELECT request_name,workspace_id FROM workspace_create_requests WHERE account_id=?1 AND request_key=?2",params![account,key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
        let id = if let Some((old_name, id)) = prior {
            if old_name != name {
                return Err(CloudError::new(
                    "conflict",
                    "idempotency key was used for a different request",
                ));
            }
            id
        } else {
            let count: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM workspace_create_requests WHERE account_id=?1",
                    [account],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if count >= 20 {
                return Err(CloudError::new(
                    "capacity",
                    "shared workspace limit reached",
                ));
            }
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO workspaces(id,name,created_at) VALUES(?1,?2,?3)",
                params![id, name, now()],
            )
            .map_err(db_error)?;
            tx.execute(
                "INSERT INTO memberships VALUES(?1,?2,'owner')",
                params![id, account],
            )
            .map_err(db_error)?;
            tx.execute(
                "INSERT INTO workspace_create_requests VALUES(?1,?2,?3,?4)",
                params![account, key, name, id],
            )
            .map_err(db_error)?;
            audit(
                &tx,
                WorkspaceId(Uuid::parse_str(&id).map_err(|_| forbidden())?),
                Some(account),
                "workspace.created",
                &id,
            )?;
            id
        };
        let w = WorkspaceId(Uuid::parse_str(&id).map_err(|_| forbidden())?);
        member(&tx, account, w, false)?;
        let (uncapped,role):(bool,String)=tx.query_row("SELECT w.uncapped_bogs,m.role FROM workspaces w JOIN memberships m ON m.workspace_id=w.id WHERE w.id=?1 AND m.account_id=?2",params![id,account],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(Workspace {
            id: w,
            name: name.into(),
            role,
            uncapped_bogs: uncapped,
            effective_uncapped_bogs: uncapped,
            bog_limit_source: allowance_source(uncapped, uncapped).into(),
            bog_limit: if uncapped { None } else { Some(3) },
            personal: false,
        })
    }
}
