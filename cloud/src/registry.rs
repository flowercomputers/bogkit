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
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_error)?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )
        .map_err(db_error)?;
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_error)?;
        if version > 1 {
            return Err(CloudError::new(
                "incompatible_registry",
                "registry version is newer than this server",
            ));
        }
        if version == 0 {
            db.execute_batch(concat!(
                "BEGIN IMMEDIATE;",
                include_str!("../migrations/001_registry.sql"),
                "COMMIT;"
            ))
            .map_err(db_error)?;
        }
        Ok(Self { db: Mutex::new(db) })
    }
    pub(crate) fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CloudError> {
        self.db
            .lock()
            .map_err(|_| CloudError::new("unavailable", "registry lock failed"))
    }
    pub fn create(&self, name: &str, template: &str, request_key: &str) -> Result<Bog, CloudError> {
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
        let digest = hash(
            serde_json::json!({"name":name,"template":template})
                .to_string()
                .as_bytes(),
        );
        let mut db = self.connection()?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let prior: Option<(Vec<u8>, String)> = tx
            .query_row(
                "SELECT body_hash,bog_id FROM create_requests WHERE request_key=?1",
                [request_key],
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
                    &format!("SELECT {COLUMNS} FROM bogs WHERE id=?1"),
                    [id],
                    read_bog,
                )
                .map_err(db_error);
        }
        if tx
            .query_row("SELECT 1 FROM bogs WHERE name=?1", [&name], |_| Ok(()))
            .optional()
            .map_err(db_error)?
            .is_some()
        {
            return Err(CloudError::new("conflict", "database name already exists"));
        }
        let bog = Bog {
            id: BogId(Uuid::new_v4()),
            name,
            template: TemplateId::RecordsV1,
            template_version: "records-v1".into(),
            status: ObservedState::Creating,
            desired_state: DesiredState::Running,
            generation: 0,
            failure_code: None,
            created_at: now(),
        };
        tx.execute("INSERT INTO bogs(id,name,template,desired_state,observed_state,created_at,template_version) VALUES (?1,?2,?3,'running','creating',?4,'records-v1')",params![bog.id.to_string(),bog.name,bog.template.as_str(),bog.created_at]).map_err(db_error)?;
        tx.execute(
            "INSERT INTO create_requests(request_key,body_hash,bog_id) VALUES (?1,?2,?3)",
            params![request_key, digest, bog.id.to_string()],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(bog)
    }
    pub fn get(&self, id: BogId) -> Result<Bog, CloudError> {
        self.connection()?
            .query_row(
                &format!("SELECT {COLUMNS} FROM bogs WHERE id=?1"),
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
                "SELECT {COLUMNS} FROM bogs ORDER BY created_at,id"
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
