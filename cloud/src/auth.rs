use crate::registry::{db_error, hash, now};
use crate::{BogId, CloudError, Registry, Scope};
use rand::TryRngCore;
use rusqlite::{OptionalExtension, params};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Principal {
    token_id: Option<String>,
    bog_id: Option<BogId>,
}
// Deliberately no Debug or Serialize: a raw token is returned only at issuance.
pub struct IssuedToken {
    pub id: String,
    pub secret: String,
}
pub struct Auth {
    registry: Arc<Registry>,
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
                bog_id: None,
            });
        }
        let db = self.registry.connection()?;
        let row:Option<(String,String,Vec<u8>)>=db.query_row("SELECT id,bog_id,secret_hash FROM tokens WHERE secret_hash=?1 AND revoked_at IS NULL",[&digest],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db_error)?;
        let (token_id, bog_id, stored) = row.ok_or_else(denied)?;
        if !bool::from(digest.ct_eq(&stored)) {
            return Err(denied());
        }
        Ok(Principal {
            token_id: Some(token_id),
            bog_id: Some(BogId(Uuid::parse_str(&bog_id).map_err(|_| denied())?)),
        })
    }
    pub fn authorize(
        &self,
        principal: &Principal,
        bog: Option<BogId>,
        write: bool,
    ) -> Result<(), CloudError> {
        let Some(id) = &principal.token_id else {
            return Ok(());
        };
        let db = self.registry.connection()?;
        let scope: Option<String> = db
            .query_row(
                "SELECT scope FROM tokens WHERE id=?1 AND revoked_at IS NULL",
                [id],
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
        self.registry.get(bog)?;
        let secret = random_secret()?;
        let id = Uuid::new_v4().to_string();
        self.registry.connection()?.execute("INSERT INTO tokens(id,bog_id,secret_hash,scope,created_at) VALUES (?1,?2,?3,?4,?5)",params![id,bog.to_string(),hash(secret.as_bytes()),match scope{Scope::Read=>"read",Scope::Write=>"write"},now()]).map_err(db_error)?;
        Ok(IssuedToken { id, secret })
    }
    pub fn revoke(&self, principal: &Principal, bog: BogId, id: &str) -> Result<(), CloudError> {
        self.authorize(principal, None, true)?;
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
