use crate::registry::{db_error, hash, now};
use crate::{BogId, CloudError, Registry, Scope};
use rand::TryRngCore;
use rusqlite::{OptionalExtension, params};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Principal {
    pub(crate) token_id: Option<String>,
    pub(crate) kind: PrincipalKind,
    pub(crate) expires_at: Option<u64>,
    pub(crate) account_id: Option<String>,
    pub(crate) workspace_id: Option<crate::WorkspaceId>,
    pub(crate) bog_id: Option<BogId>,
}
// Deliberately no Debug or Serialize: a raw token is returned only at issuance.
pub struct IssuedToken {
    pub id: String,
    pub secret: String,
}
pub struct Auth {
    pub(crate) registry: Arc<Registry>,
    owner_hash: Vec<u8>,
}
fn denied() -> CloudError {
    CloudError::new("unauthorized", "invalid credentials")
}
pub fn random_secret() -> Result<String, CloudError> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| CloudError::new("unavailable", "secure randomness unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
impl Auth {
    pub fn new(registry: Arc<Registry>, owner: &str) -> Result<Self, CloudError> {
        if owner.len() < 32 || owner.len() > 4096 || owner.chars().any(char::is_control) {
            return Err(CloudError::new(
                "invalid_config",
                "owner credential must be 32 to 4096 bytes without control characters",
            ));
        }
        Ok(Self {
            registry,
            owner_hash: hash(owner.as_bytes()),
        })
    }
    pub fn authenticate(&self, secret: &str) -> Result<Principal, CloudError> {
        if secret.len() > 4096 || secret.is_empty() {
            return Err(denied());
        }
        let digest = hash(secret.as_bytes());
        if bool::from(digest.ct_eq(&self.owner_hash)) {
            return Ok(Principal {
                token_id: None,
                kind: PrincipalKind::Operator,
                expires_at: None,
                account_id: None,
                workspace_id: None,
                bog_id: None,
            });
        }
        let (public_id, _) = secret.split_once('.').ok_or_else(denied)?;
        Uuid::parse_str(public_id).map_err(|_| denied())?;
        let db = self.registry.connection()?;
        let row: Option<(String, String, Vec<u8>)> = db
            .query_row(
                "SELECT id,bog_id,secret_hash FROM tokens WHERE id=?1 AND revoked_at IS NULL",
                [public_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(db_error)?;
        let (token_id, bog_id, stored) = row.ok_or_else(denied)?;
        if !bool::from(digest.ct_eq(&stored)) {
            return Err(denied());
        }
        let (account_id, workspace, expires): (Option<String>, Option<String>, Option<i64>) = db
            .query_row(
                "SELECT account_id,workspace_id,expires_at FROM tokens WHERE id=?1",
                [&token_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(db_error)?;
        if expires.is_some_and(|v| v <= now()) {
            return Err(denied());
        }
        let principal = Principal {
            kind: PrincipalKind::App,
            expires_at: None,
            account_id,
            workspace_id: workspace
                .and_then(|v| Uuid::parse_str(&v).ok())
                .map(crate::WorkspaceId),
            token_id: Some(token_id),
            bog_id: Some(BogId(Uuid::parse_str(&bog_id).map_err(|_| denied())?)),
        };
        drop(db);
        self.authorize(&principal, principal.bog_id, false)?;
        Ok(principal)
    }
    pub fn authorize(
        &self,
        principal: &Principal,
        bog: Option<BogId>,
        write: bool,
    ) -> Result<(), CloudError> {
        if principal
            .expires_at
            .is_some_and(|expiry| expiry <= now().max(0) as u64)
        {
            return Err(denied());
        }
        if principal.kind != PrincipalKind::Operator || principal.workspace_id.is_some() {
            if let Some(account) = &principal.account_id {
                self.check_member(account, principal.workspace_id.ok_or_else(denied)?, false)?;
            }
            if let Some(workspace) = principal.workspace_id {
                let db = self.registry.connection()?;
                let active:bool=db.query_row("SELECT suspended_at IS NULL AND deleted_at IS NULL FROM workspaces WHERE id=?1",[workspace.to_string()],|r|r.get(0)).map_err(db_error)?;
                if !active {
                    return Err(denied());
                }
                drop(db);
                if let Some(bog) = bog {
                    self.registry.get_scoped(workspace, bog)?;
                }
            }
        }
        let Some(id) = &principal.token_id else {
            return Ok(());
        };
        let db = self.registry.connection()?;
        let scope: Option<String> = db
            .query_row(
                "SELECT scope FROM tokens WHERE id=?1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?2)",
                params![id,now()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        let scope = scope.ok_or_else(denied)?;
        let Some(bog) = bog else {
            return Err(CloudError::new("forbidden", "owner credentials required"));
        };
        if principal.bog_id != Some(bog) {
            return Err(CloudError::new("not_found", "database not found"));
        }
        if write && scope != "write" {
            return Err(CloudError::new("forbidden", "write credentials required"));
        }
        Ok(())
    }
    pub fn issue(
        &self,
        principal: &Principal,
        bog: BogId,
        scope: Scope,
    ) -> Result<IssuedToken, CloudError> {
        self.authorize(principal, None, true)?;
        self.authorize(principal, Some(bog), true)?;
        self.registry.get(bog)?;
        if principal.kind != PrincipalKind::Operator {
            return self.issue_app_token(principal, bog, scope);
        }
        let id = Uuid::new_v4().to_string();
        let secret = format!("{id}.{}", random_secret()?);
        self.registry.connection()?.execute("INSERT INTO tokens(id,bog_id,secret_hash,scope,created_at,workspace_id,expires_at) VALUES (?1,?2,?3,?4,?5,(SELECT workspace_id FROM bogs WHERE id=?2),?6)",params![id,bog.to_string(),hash(secret.as_bytes()),match scope{Scope::Read=>"read",Scope::Write=>"write"},now(),now()+90*86400]).map_err(db_error)?;
        Ok(IssuedToken { id, secret })
    }
    pub fn revoke(&self, principal: &Principal, bog: BogId, id: &str) -> Result<(), CloudError> {
        self.authorize(principal, None, true)?;
        self.authorize(principal, Some(bog), true)?;
        if principal.kind != PrincipalKind::Operator {
            self.registry
                .get_scoped(principal.workspace_id.ok_or_else(denied)?, bog)?;
            return self.revoke_app_token(principal, id);
        }
        let n = self
            .registry
            .connection()?
            .execute(
                "UPDATE tokens SET revoked_at=?3 WHERE id=?1 AND bog_id=?2",
                params![id, bog.to_string(), now()],
            )
            .map_err(db_error)?;
        if n == 0 {
            Err(CloudError::new("not_found", "token not found"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    Operator,
    Human,
    Agent,
    App,
}
impl Principal {
    pub(crate) fn legacy_public_operator(&self) -> bool {
        self.kind == PrincipalKind::Operator
            && self.workspace_id == Some(crate::WorkspaceId::legacy())
    }
    pub fn kind(&self) -> PrincipalKind {
        self.kind
    }
    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }
    pub fn workspace_id(&self) -> Option<crate::WorkspaceId> {
        self.workspace_id
    }
}
