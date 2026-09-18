use crate::{Bog, BogId, CloudError, DesiredState, ObservedState, TemplateId};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub struct Registry {
    pub(crate) db: Mutex<Connection>,
}
pub(crate) fn db_error(_: rusqlite::Error) -> CloudError {
    CloudError::new("unavailable", "registry operation failed")
}
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub(crate) fn hash(value: &[u8]) -> Vec<u8> {
    Sha256::digest(value).to_vec()
}
// Changing journal mode can return SQLITE_BUSY immediately without invoking SQLite's
// busy handler. Retry only lock contention, with a shared deadline across attempts.
// SQLite coordinates these locks across processes; a process-local mutex would not.
fn initialize_connection(db: &Connection) -> rusqlite::Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        db.busy_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100)),
        )?;
        match db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        ) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let contention = matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                );
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !contention || remaining.is_zero() {
                    return Err(error);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
        }
    }
}
fn read_bog(row: &rusqlite::Row<'_>) -> rusqlite::Result<Bog> {
    let raw: String = row.get(0)?;
    let id = Uuid::parse_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Bog {
        id: BogId(id),
        name: row.get(1)?,
        template: parse_column(row, 2)?,
        template_version: row.get(8)?,
        desired_state: parse_column(row, 3)?,
        status: parse_column(row, 4)?,
        generation: row.get(5)?,
        failure_code: row.get(6)?,
        created_at: row.get(7)?,
    })
}
const COLUMNS: &str = "id,name,template,desired_state,observed_state,generation,failure_code,created_at,template_version";
impl Registry {
    pub fn open(path: &Path) -> Result<Self, CloudError> {
        let db = Connection::open(path).map_err(db_error)?;
        initialize_connection(&db).map_err(db_error)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_error)?;
        // Serialize the version check with migration so simultaneous opens cannot both rebuild v1.
        db.execute_batch("PRAGMA foreign_keys=OFF; BEGIN IMMEDIATE;")
            .map_err(db_error)?;
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_error)?;
        if version > 5 {
            return Err(CloudError::new(
                "incompatible_registry",
                "registry version is newer than this server",
            ));
        }
        if version == 0 {
            db.execute_batch(include_str!("../migrations/001_registry.sql"))
                .map_err(db_error)?;
        }
        if version < 2 {
            db.execute_batch(include_str!("../migrations/002_workspaces.sql"))
                .map_err(db_error)?;
        }
        if version < 3 {
            db.execute_batch(include_str!("../migrations/003_agent_tokens.sql"))
                .map_err(db_error)?;
        }
        if version < 4 {
            db.execute_batch(include_str!("../migrations/004_workspace_quotas.sql"))
                .map_err(db_error)?;
        }
        if version < 5 {
            db.execute_batch("CREATE TABLE bog_definitions (bog_id TEXT PRIMARY KEY REFERENCES bogs(id), definition TEXT NOT NULL, digest TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1, storage_dir TEXT NOT NULL DEFAULT 'data'); CREATE TABLE definition_jobs (id TEXT PRIMARY KEY, bog_id TEXT NOT NULL REFERENCES bogs(id), status TEXT NOT NULL, payload TEXT NOT NULL, created_at INTEGER NOT NULL); PRAGMA user_version=5;").map_err(db_error)?;
        }
        let foreign_key_errors: i64 = db
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .map_err(db_error)?;
        if foreign_key_errors != 0 {
            return Err(CloudError::new(
                "unavailable",
                "registry integrity check failed",
            ));
        }
        db.execute_batch("COMMIT; PRAGMA foreign_keys=ON;")
            .map_err(db_error)?;
        Ok(Self { db: Mutex::new(db) })
    }
    pub(crate) fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CloudError> {
        self.db
            .lock()
            .map_err(|_| CloudError::new("unavailable", "registry lock failed"))
    }
    pub fn create(&self, name: &str, template: &str, request_key: &str) -> Result<Bog, CloudError> {
        self.create_limited(name, template, request_key, usize::MAX)
    }
    pub fn create_limited(
        &self,
        name: &str,
        template: &str,
        request_key: &str,
        maximum: usize,
    ) -> Result<Bog, CloudError> {
        self.create_with_state(
            name,
            template,
            request_key,
            maximum,
            ObservedState::Creating,
        )
    }
    pub(crate) fn create_restoring(
        &self,
        name: &str,
        request_key: &str,
        maximum: usize,
    ) -> Result<Bog, CloudError> {
        self.create_with_state(
            name,
            "records-v1",
            request_key,
            maximum,
            ObservedState::Restoring,
        )
    }
    fn create_with_state(
        &self,
        name: &str,
        template: &str,
        request_key: &str,
        maximum: usize,
        initial: ObservedState,
    ) -> Result<Bog, CloudError> {
        self.create_in_workspace(
            crate::WorkspaceId::legacy(),
            name,
            template,
            request_key,
            maximum,
            usize::MAX,
            initial,
            None,
            maximum,
            None,
        )
    }
    pub fn create_scoped(
        &self,
        workspace: crate::WorkspaceId,
        name: &str,
        template: &str,
        key: &str,
    ) -> Result<Bog, CloudError> {
        self.create_in_workspace(
            workspace,
            name,
            template,
            key,
            3,
            3,
            ObservedState::Creating,
            None,
            32,
            None,
        )
    }
    pub fn create_for_principal(
        &self,
        p: &crate::Principal,
        name: &str,
        template: &str,
        key: &str,
        global_maximum: usize,
    ) -> Result<Bog, CloudError> {
        if !matches!(
            p.kind(),
            crate::PrincipalKind::Human | crate::PrincipalKind::Agent
        ) {
            return Err(CloudError::new("forbidden", "workspace identity required"));
        }
        self.create_in_workspace(
            p.workspace_id()
                .ok_or_else(|| CloudError::new("forbidden", "workspace required"))?,
            name,
            template,
            key,
            3,
            3,
            ObservedState::Creating,
            Some(p),
            global_maximum.min(32),
            None,
        )
    }
    #[allow(clippy::too_many_arguments)] // One transaction must cover identity, scope, limits, and initial state.
    fn create_in_workspace(
        &self,
        workspace: crate::WorkspaceId,
        name: &str,
        template: &str,
        request_key: &str,
        maximum: usize,
        retained: usize,
        initial: ObservedState,
        principal: Option<&crate::Principal>,
        global_maximum: usize,
        definition: Option<&bog_definition::Definition>,
    ) -> Result<Bog, CloudError> {
        let name = name.trim().to_lowercase();
        if name.is_empty()
            || name.len() > 128
            || name
                .chars()
                .any(|c| c.is_control() || c == '/' || c == '\\')
            || name == "."
            || name == ".."
            || template != "records-v1"
            || request_key.is_empty()
            || request_key.len() > 128
            || request_key.chars().any(char::is_control)
        {
            return Err(CloudError::new(
                "invalid_request",
                "invalid name, template, or idempotency key",
            ));
        }
        let mut request = serde_json::json!({"name":name,"template":template});
        if let Some(definition) = definition {
            request["definition_digest"] =
                serde_json::json!(definition.digest().map_err(crate::definitions::invalid)?);
        }
        let digest = hash(request.to_string().as_bytes());
        let mut db = self.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        if let Some(p) = principal {
            if p.expires_at
                .is_some_and(|expiry| expiry <= now().max(0) as u64)
            {
                return Err(CloudError::new("unauthorized", "invalid credentials"));
            }
            crate::workspace::member(
                &tx,
                p.account_id()
                    .ok_or_else(|| CloudError::new("forbidden", "account required"))?,
                workspace,
                false,
            )?;
        }
        let prior: Option<(Vec<u8>, String)> = tx
            .query_row(
                "SELECT body_hash,bog_id FROM create_requests WHERE request_key=?1 AND workspace_id=?2",
                params![request_key,workspace.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        if let Some((body, id)) = prior {
            if body != digest {
                return Err(CloudError::new(
                    "conflict",
                    "idempotency key was used for a different request",
                ));
            }
            return tx
                .query_row(
                    &format!("SELECT {COLUMNS} FROM bogs WHERE id=?1 AND deleted_at IS NULL"),
                    [id],
                    read_bog,
                )
                .map_err(db_error);
        }
        if tx
            .query_row(
                "SELECT 1 FROM bogs WHERE name=?1 AND workspace_id=?2",
                params![name, workspace.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(db_error)?
            .is_some()
        {
            return Err(CloudError::new("conflict", "database name already exists"));
        }
        let uncapped: bool = tx.query_row("SELECT w.uncapped_bogs OR COALESCE(a.uncapped_bogs,0) FROM workspaces w LEFT JOIN accounts a ON a.id=w.personal_account_id WHERE w.id=?1", [workspace.to_string()], |r| r.get(0)).map_err(db_error)?;
        if !uncapped {
            let active: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM bogs WHERE (desired_state='running' OR (deleted_at IS NOT NULL AND cleanup_completed_at IS NULL)) AND workspace_id=?1",
                [workspace.to_string()],
                |r| r.get(0),
            )
            .map_err(db_error)?;
            if active as u64 >= maximum as u64 {
                return Err(CloudError::new("capacity", "active database limit reached"));
            }
            let count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM bogs WHERE workspace_id=?1 AND (deleted_at IS NULL OR cleanup_completed_at IS NULL)",
                [workspace.to_string()],
                |r| r.get(0),
            )
            .map_err(db_error)?;
            if count as u64 >= retained as u64 {
                return Err(CloudError::new(
                    "capacity",
                    "retained database limit reached",
                ));
            }
        }
        let global_count:i64=tx.query_row("SELECT COUNT(*) FROM bogs WHERE deleted_at IS NULL OR cleanup_completed_at IS NULL",[],|r|r.get(0)).map_err(db_error)?;
        if global_count as u64 >= global_maximum as u64 {
            return Err(CloudError::new("capacity", "active database limit reached"));
        }
        let bog = Bog {
            id: BogId(Uuid::new_v4()),
            name,
            template: TemplateId::RecordsV1,
            template_version: "records-v1".into(),
            status: initial,
            desired_state: DesiredState::Running,
            generation: 0,
            failure_code: None,
            created_at: now(),
        };
        tx.execute("INSERT INTO bogs(id,name,template,desired_state,observed_state,created_at,template_version,workspace_id) VALUES (?1,?2,?3,'running',?5,?4,'records-v1',?6)",params![bog.id.to_string(),bog.name,bog.template.as_str(),bog.created_at,initial.as_str(),workspace.to_string()]).map_err(db_error)?;
        tx.execute(
            "INSERT INTO create_requests(request_key,body_hash,bog_id,workspace_id) VALUES (?1,?2,?3,?4)",
            params![request_key, digest, bog.id.to_string(),workspace.to_string()],
        )
        .map_err(db_error)?;
        if let Some(definition) = definition {
            tx.execute("INSERT INTO bog_definitions(bog_id,definition,digest,revision) VALUES (?1,?2,?3,1)", params![bog.id.to_string(), serde_json::to_string(definition).map_err(|_| CloudError::new("invalid_request", "invalid definition"))?, definition.digest().map_err(crate::definitions::invalid)?]).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(bog)
    }
    pub fn list_scoped(&self, workspace: crate::WorkspaceId) -> Result<Vec<Bog>, CloudError> {
        let db = self.connection()?;
        let mut stmt = db
            .prepare(&format!(
                "SELECT {COLUMNS} FROM bogs WHERE workspace_id=?1 AND deleted_at IS NULL ORDER BY created_at,id"
            ))
            .map_err(db_error)?;
        stmt.query_map([workspace.to_string()], read_bog)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }
    pub fn get_scoped(&self, workspace: crate::WorkspaceId, id: BogId) -> Result<Bog, CloudError> {
        self.connection()?
            .query_row(
                &format!("SELECT {COLUMNS} FROM bogs WHERE id=?1 AND workspace_id=?2 AND deleted_at IS NULL"),
                params![id.to_string(), workspace.to_string()],
                read_bog,
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| CloudError::new("not_found", "database not found"))
    }
    pub fn get(&self, id: BogId) -> Result<Bog, CloudError> {
        self.connection()?
            .query_row(
                &format!("SELECT {COLUMNS} FROM bogs WHERE id=?1 AND deleted_at IS NULL"),
                [id.to_string()],
                read_bog,
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| CloudError::new("not_found", "database not found"))
    }
    pub fn list(&self) -> Result<Vec<Bog>, CloudError> {
        let db = self.connection()?;
        let mut s = db
            .prepare(&format!(
                "SELECT {COLUMNS} FROM bogs WHERE deleted_at IS NULL ORDER BY created_at,id"
            ))
            .map_err(db_error)?;
        s.query_map([], read_bog)
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)
    }
    pub fn set_status(
        &self,
        id: BogId,
        status: ObservedState,
        failure: Option<&str>,
    ) -> Result<(), CloudError> {
        let n = self
            .connection()?
            .execute(
                "UPDATE bogs SET observed_state=?2,failure_code=?3 WHERE id=?1",
                params![id.to_string(), status.as_str(), failure],
            )
            .map_err(db_error)?;
        if n == 0 {
            Err(CloudError::new("not_found", "database not found"))
        } else {
            Ok(())
        }
    }
    pub fn set_desired(&self, id: BogId, running: bool) -> Result<(), CloudError> {
        let n = self
            .connection()?
            .execute(
                "UPDATE bogs SET desired_state=?2 WHERE id=?1",
                params![id.to_string(), if running { "running" } else { "stopped" }],
            )
            .map_err(db_error)?;
        if n == 0 {
            Err(CloudError::new("not_found", "database not found"))
        } else {
            Ok(())
        }
    }
    pub fn start_generation(&self, id: BogId, nonce: &str) -> Result<i64, CloudError> {
        self.connection()?.query_row("UPDATE bogs SET generation=generation+1,startup_nonce=?2,observed_state='creating',failure_code=NULL WHERE id=?1 RETURNING generation",params![id.to_string(),nonce],|r|r.get(0)).map_err(db_error)
    }
    pub fn startup_nonce(&self, id: BogId) -> Result<Option<String>, CloudError> {
        self.connection()?
            .query_row(
                "SELECT startup_nonce FROM bogs WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .map_err(db_error)
    }
}

fn parse_column<T: std::str::FromStr<Err = CloudError>>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    raw.parse().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

impl Registry {
    pub fn create_defined(
        &self,
        principal: &crate::Principal,
        name: &str,
        key: &str,
        definition: &bog_definition::Definition,
        maximum: usize,
    ) -> Result<Bog, CloudError> {
        let scoped = principal.kind() != crate::PrincipalKind::Operator;
        self.create_in_workspace(
            principal
                .workspace_id()
                .unwrap_or_else(crate::WorkspaceId::legacy),
            name,
            "records-v1",
            key,
            if scoped { 3 } else { maximum },
            if scoped { 3 } else { usize::MAX },
            ObservedState::Creating,
            if scoped { Some(principal) } else { None },
            if scoped { maximum.min(32) } else { maximum },
            Some(definition),
        )
    }
}
