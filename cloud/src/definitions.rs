//! Definition contracts, durable revisions and additive build journal.
use crate::{BogId, CloudError, Registry};
use bog_definition::Definition;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) fn invalid(error: impl std::fmt::Display) -> CloudError {
    CloudError::new("invalid_request", &error.to_string())
}
pub fn parse(value: Value) -> Result<Definition, CloudError> {
    if value.to_string().len() > 1024 * 1024 {
        return Err(CloudError::new(
            "payload_too_large",
            "definition exceeds 1 MiB",
        ));
    }
    let d: Definition = serde_json::from_value(value).map_err(invalid)?;
    d.validate().map_err(invalid)?;
    Ok(d)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveDefinition {
    pub definition: Definition,
    pub digest: String,
    pub revision: u64,
    #[serde(skip_serializing, default = "default_storage")]
    pub storage_dir: String,
    pub configured: bool,
}
fn default_storage() -> String {
    "data".into()
}
impl Registry {
    pub(crate) fn storage_dir(&self, id: BogId) -> Result<String, CloudError> {
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT storage_dir FROM bog_definitions WHERE bog_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(crate::registry::db_error)?;
        let value = value.unwrap_or_else(|| "data".into());
        if value != "data"
            && (!value.starts_with("candidate-") || uuid::Uuid::parse_str(&value[10..]).is_err())
        {
            return Err(CloudError::new(
                "unavailable",
                "invalid active storage directory",
            ));
        }
        Ok(value)
    }
    pub fn definition(&self, id: BogId) -> Result<ActiveDefinition, CloudError> {
        self.get(id)?;
        self.worker_definition(id)
    }
    pub(crate) fn worker_definition(&self, id: BogId) -> Result<ActiveDefinition, CloudError> {
        let db = self.connection()?;
        let row: Option<(String,String,i64,String)> = db.query_row("SELECT definition,digest,revision,storage_dir FROM bog_definitions WHERE bog_id=?1", [id.to_string()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(crate::registry::db_error)?;
        if let Some((raw, digest, revision, storage_dir)) = row {
            let revision = u64::try_from(revision)
                .ok()
                .filter(|r| *r > 0)
                .ok_or_else(|| CloudError::new("unavailable", "invalid definition revision"))?;
            let definition = parse(serde_json::from_str(&raw).map_err(invalid)?)?;
            if definition.digest().map_err(invalid)? != digest {
                return Err(CloudError::new("unavailable", "definition digest mismatch"));
            }
            if storage_dir != "data"
                && (!storage_dir.starts_with("candidate-")
                    || uuid::Uuid::parse_str(&storage_dir[10..]).is_err())
            {
                return Err(CloudError::new(
                    "unavailable",
                    "invalid active storage directory",
                ));
            }
            Ok(ActiveDefinition {
                definition,
                digest,
                revision,
                storage_dir,
                configured: true,
            })
        } else {
            let definition = Definition::records_v1();
            Ok(ActiveDefinition {
                digest: definition.digest().map_err(invalid)?,
                definition,
                revision: 1,
                storage_dir: "data".into(),
                configured: false,
            })
        }
    }
    pub fn definition_job(&self, id: BogId, job: &str) -> Result<Value, CloudError> {
        self.get(id)?;
        let db = self.connection()?;
        let row: Option<(String, String)> = db
            .query_row(
                "SELECT status,payload FROM definition_jobs WHERE id=?1 AND bog_id=?2",
                params![job, id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(crate::registry::db_error)?;
        let (status, payload) =
            row.ok_or_else(|| CloudError::new("not_found", "definition job not found"))?;
        let mut value: Value = serde_json::from_str(&payload).map_err(invalid)?;
        value["job_id"] = json!(job);
        value["bog_id"] = json!(id);
        value["status"] = json!(status);
        Ok(value)
    }
    pub(crate) fn building(&self, id: BogId) -> Result<bool, CloudError> {
        self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM definition_jobs WHERE bog_id=?1 AND status IN ('building','activating','recovery_required'))",[id.to_string()],|r|r.get(0)).map_err(crate::registry::db_error)
    }
    pub(crate) fn set_job_status(
        &self,
        job: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), CloudError> {
        let db = self.connection()?;
        db.execute("UPDATE definition_jobs SET status=?2,payload=json_set(payload,'$.error',?3) WHERE id=?1",params![job,status,error]).map_err(crate::registry::db_error)?;
        Ok(())
    }
    pub(crate) fn recover_definition_jobs(&self) -> Result<(), CloudError> {
        // Activation and the active pointer commit together. Everything left in progress
        // therefore never activated; retain the old source and release paused writes.
        self.connection()?.execute("UPDATE definition_jobs SET status='failed',payload=json_set(payload,'$.error','manager_restarted_before_activation') WHERE status IN ('building','activating','recovery_required')",[]).map_err(crate::registry::db_error)?;
        Ok(())
    }
}
pub fn resources(active: &ActiveDefinition) -> Value {
    resources_with_limits(active, &bog_definition::Limits::default())
}
pub fn resources_with_limits(active: &ActiveDefinition, limits: &bog_definition::Limits) -> Value {
    let operations = active.definition.operation_metadata_with_limits(limits);
    let resources:Vec<_>=active.definition.resources.iter().filter_map(|(name,r)|{
        let exposed:Vec<_>=operations.iter().filter(|o|o.target==*name).collect();
        if exposed.is_empty(){None}else{Some(json!({"name":name,"stages":r.stages,"terminal":r.terminal,"operations":exposed}))}
    }).collect();
    json!({"resources":resources,"revision":active.revision,"digest":active.digest})
}
impl crate::CloudService {
    pub(crate) fn plan_definition(
        &self,
        id: BogId,
        value: Value,
        expected: u64,
    ) -> Result<Value, CloudError> {
        let current = self.registry.definition(id)?;
        if expected >= i64::MAX as u64 {
            return Err(invalid("revision exceeds supported range"));
        }
        if current.revision != expected {
            return Err(CloudError::new("conflict", "stale definition revision"));
        }
        let next = parse(value)?;
        self.supervisor
            .config
            .composable_limits
            .validate_definition(&next)
            .map_err(invalid)?;
        let diff = current
            .definition
            .validate_additive(&next)
            .map_err(invalid)?;
        Ok(
            json!({"compatible":true,"expected_revision":expected,"target_digest":next.digest().map_err(invalid)?,"requires_rebuild":true,"changes":diff}),
        )
    }
    pub(crate) async fn resource_request(
        &self,
        id: BogId,
        resource: &str,
        mut query: Value,
        search: bool,
    ) -> Result<Value, CloudError> {
        use bog_definition::Action;
        let active = self.registry.definition(id)?;
        let object = query
            .as_object_mut()
            .ok_or_else(|| invalid("query must be an object"))?;
        let action = if search {
            Action::Search
        } else if let Some(action) = object.remove("action") {
            serde_json::from_value(action).map_err(invalid)?
        } else {
            match active
                .definition
                .resources
                .get(resource)
                .map(|r| &r.terminal)
            {
                Some(bog_definition::Terminal::Table) => Action::List,
                Some(bog_definition::Terminal::Ranked { .. }) => Action::Top,
                _ => Action::Read,
            }
        };
        if !matches!(
            action,
            Action::Get | Action::List | Action::Read | Action::Top | Action::Search
        ) || (search != (action == Action::Search))
        {
            return Err(invalid("unsupported resource query action"));
        }
        let operation = active
            .definition
            .expose
            .iter()
            .find(|(_, op)| op.target == resource && op.action == action)
            .map(|(name, _)| name)
            .ok_or_else(|| CloudError::new("not_found", "resource operation is not exposed"))?;
        if !active.configured {
            let q: bog_runtime::Query = serde_json::from_value(query).map_err(invalid)?;
            let limit = q.limit.unwrap_or(100);
            let offset = q.offset.unwrap_or(0);
            if limit > 1000 || offset > 10_000 {
                return Err(invalid("page exceeds limits"));
            }
            let path = match action {
                Action::Get => format!(
                    "/docs/{}",
                    crate::service::encode_key(
                        q.key.as_deref().ok_or_else(|| invalid("key required"))?
                    )?
                ),
                Action::List => format!("/views/docs?limit={limit}&offset={offset}"),
                Action::Read => "/views/total".into(),
                _ => {
                    return Err(CloudError::new(
                        "not_found",
                        "resource operation is not exposed",
                    ));
                }
            };
            let mut result = self.worker(id, reqwest::Method::GET, path, None).await?;
            if action == Action::Read {
                result["data"] = result["data"]["value"].clone();
            }
            return Ok(result);
        }
        self.worker(
            id,
            reqwest::Method::POST,
            format!("/operations/{operation}"),
            Some(query),
        )
        .await
    }
}
pub(crate) fn write_json(path: &std::path::Path, value: &Value) -> Result<(), CloudError> {
    use std::io::Write;
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|_| CloudError::new("unavailable", "cannot write definition"))?;
    file.write_all(&serde_json::to_vec(value).map_err(invalid)?)
        .and_then(|_| file.sync_all())
        .map_err(|_| CloudError::new("unavailable", "cannot persist definition"))?;
    std::fs::rename(&temporary, path)
        .map_err(|_| CloudError::new("unavailable", "cannot activate definition file"))?;
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| invalid("invalid definition path"))?,
    )
    .and_then(|f| f.sync_all())
    .map_err(|_| CloudError::new("unavailable", "cannot persist definition directory"))
}
impl crate::CloudService {
    pub(crate) async fn apply_definition(
        &self,
        id: BogId,
        value: Value,
        expected: u64,
    ) -> Result<crate::OperationResult, CloudError> {
        let definition = parse(value.clone())?;
        self.plan_definition(id, value, expected)?;
        let permit = self.supervisor.reserve_build()?;
        crate::config::require_free_space(
            &self.supervisor.config.root,
            self.supervisor
                .config
                .min_free_bytes
                .saturating_add(128 * 1024 * 1024),
        )?;
        if self.supervisor.max_active() < 2 {
            return Err(CloudError::new(
                "capacity",
                "definition builds require resident capacity for both source and candidate",
            ));
        }
        let guard = self.supervisor.gate(id)?.write_owned().await;
        let current = self.registry.definition(id)?;
        if current.revision != expected || self.registry.building(id)? {
            return Err(CloudError::new(
                "conflict",
                "definition changed or a build is in progress",
            ));
        }
        // Keep source admission and candidate reservation under the source gate.
        // Waking first ensures a sleeping source cannot lose its needed slot to
        // the candidate, and the gate excludes it from pressure reclamation.
        self.supervisor.start_locked(id).await?;
        let candidate_slot = self.supervisor.reserve_build_candidate(id).await?;
        let job = uuid::Uuid::new_v4().to_string();
        let payload = json!({"expected_revision":expected,"definition":definition,"target_digest":definition.digest().map_err(invalid)?});
        self.registry.connection()?.execute("INSERT INTO definition_jobs(id,bog_id,status,payload,created_at) VALUES (?1,?2,'building',?3,?4)",params![job,id.to_string(),payload.to_string(),crate::registry::now()]).map_err(crate::registry::db_error)?;
        drop(guard);
        let supervisor = self.supervisor.clone();
        let registry = self.registry.clone();
        let job_for_task = job.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _candidate_slot = candidate_slot;
            let result = supervisor
                .build_definition(id, &job_for_task, definition, expected)
                .await;
            if let Err(error) = result {
                let status = if error.code == "recovery_required" {
                    "recovery_required"
                } else {
                    "failed"
                };
                let _ = registry.set_job_status(&job_for_task, status, Some(&error.code));
            }
        });
        Ok(crate::OperationResult {
            status: 202,
            body: json!({"job_id":job,"status":"building","bog_id":id}),
        })
    }
}
impl crate::supervisor::Supervisor {
    pub(crate) async fn build_definition(
        &self,
        id: BogId,
        job: &str,
        definition: Definition,
        expected: u64,
    ) -> Result<(), CloudError> {
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(self.config.composable_limits.build_timeout_seconds);
        let candidate = self.instance_dir(id).join(format!("candidate-{job}"));
        let mut paused_client = None;
        let result=async {
            // The persistent write pause was installed under the exclusive gate;
            // every earlier write drained before this stable export begins.
            let lease=self.lease(id).await?;
            // Freeze within the worker as well: a canceled manager request may
            // have released its lease while its worker mutation is still queued.
            paused_client=Some(lease.client.clone());
            let (status,_)=lease.client.request(reqwest::Method::POST,"/_cloud/pause_writes",None).await?;
            if status==404 {
                // A worker predating freeze support never paused. Do not send a
                // matching unsupported resume or retain a false write block.
                paused_client=None;
                return Err(CloudError::new("unavailable","source worker must be restarted before definition builds are supported"));
            }
            if status!=200 {return Err(CloudError::new("unavailable","worker cannot freeze source writes"));}
            let mut offset=0usize; let mut records=Vec::new(); let mut expected_snapshot=None;
            loop {
                if std::time::Instant::now()>=deadline {return Err(CloudError::new("timeout","definition build exceeded the configured deadline"));}
                let (status,value)=lease.client.request(reqwest::Method::GET,&format!("/_cloud/export?offset={offset}"),None).await?;
                if status!=200 {return Err(CloudError::new("unavailable","source export failed"));}
                let page:bog_runtime::Export=serde_json::from_value(value.get("data").cloned().unwrap_or(value)).map_err(invalid)?;
                let snapshot=(page.record_count,page.source_digest);
                if let Some(expected)=&expected_snapshot {if expected!=&snapshot {return Err(CloudError::new("conflict","source changed during export"));}} else {expected_snapshot=Some(snapshot);}
                records.extend(page.records);
                match page.next_offset {None=>break,Some(next) if next>offset=>offset=next,_=>return Err(CloudError::new("unavailable","source export made no progress"))}
            }
            drop(lease);
            let snapshot=expected_snapshot.ok_or_else(||invalid("missing source snapshot"))?;
            if records.len()!=snapshot.0 {return Err(invalid("source export count mismatch"));}
            let path=candidate.clone(); let target=definition.clone(); let reserve=self.config.min_free_bytes; let limits=self.config.composable_limits.clone();
            tokio::task::spawn_blocking(move || rebuild(&path,target,&records,&snapshot,deadline,reserve,limits)).await.map_err(|_|CloudError::new("unavailable","candidate build task failed"))??;
            if std::time::Instant::now()>=deadline {return Err(CloudError::new("timeout","definition build exceeded the configured deadline"));}
            let _guard=tokio::time::timeout(deadline.saturating_duration_since(std::time::Instant::now()),self.gate(id)?.write_owned()).await.map_err(|_|CloudError::new("timeout","definition activation exceeded the configured deadline"))?;
            check_deadline(deadline)?;
            let active=self.registry.definition(id)?;
            if active.revision!=expected {return Err(CloudError::new("conflict","active definition changed during build"));}
            self.registry.set_job_status(job,"activating",None)?;
            self.stop_locked(id,false).await?;
            check_deadline(deadline)?;
            // A single durable transaction switches data identity, definition and job state.
            // A restart sees either the old source or the complete verified candidate.
            {
                let mut db=self.registry.connection()?;
                let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(crate::registry::db_error)?;
                tx.execute("INSERT INTO bog_definitions(bog_id,definition,digest,revision,storage_dir) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(bog_id) DO UPDATE SET definition=excluded.definition,digest=excluded.digest,revision=excluded.revision,storage_dir=excluded.storage_dir",params![id.to_string(),definition.normalized_json().map_err(invalid)?,definition.digest().map_err(invalid)?,(expected+1) as i64,format!("candidate-{job}")]).map_err(crate::registry::db_error)?;
                tx.execute("UPDATE definition_jobs SET status='succeeded',payload=json_set(payload,'$.revision',?2) WHERE id=?1",params![job,(expected+1) as i64]).map_err(crate::registry::db_error)?;
                tx.commit().map_err(crate::registry::db_error)?;
            }
            // The activation already committed. A failed wake does not undo the
            // revision or mislabel a committed job; normal worker recovery retries.
            let _ = self.start_locked(id).await;
            Ok(())
        }.await;
        // Never remove a directory selected by the durable active pointer, even if
        // startup failed after activation. It must remain recoverable on restart.
        if result.is_err()
            && self
                .registry
                .definition(id)
                .is_ok_and(|a| a.storage_dir != format!("candidate-{job}"))
        {
            let _ = std::fs::remove_dir_all(&candidate);
        }
        if result.is_err()
            && let Some(client) = paused_client
        {
            let resumed = client
                .request(reqwest::Method::POST, "/_cloud/resume_writes", None)
                .await
                .is_ok_and(|(status, _)| status == 200);
            if !resumed {
                // Try normal worker recovery once. If it cannot establish a live,
                // unpaused worker, keep the durable journal write block in place.
                let recovered = match self.ensure_running(id).await {
                    Ok(client) => client
                        .request(reqwest::Method::POST, "/_cloud/resume_writes", None)
                        .await
                        .is_ok_and(|(status, _)| status == 200),
                    Err(_) => false,
                };
                if !recovered {
                    return Err(CloudError::new(
                        "recovery_required",
                        "build failed and writes remain paused until worker recovery",
                    ));
                }
            }
        }
        result
    }
}
pub(crate) fn rebuild(
    path: &std::path::Path,
    definition: Definition,
    records: &[bog_runtime::Record],
    snapshot: &(usize, String),
    deadline: std::time::Instant,
    reserve: u64,
    limits: bog_definition::Limits,
) -> Result<(), CloudError> {
    check_deadline(deadline)?;
    let storage_root = path
        .parent()
        .ok_or_else(|| invalid("invalid candidate path"))?;
    // Recheck before every replay step. Reserve additionally covers one maximum
    // source record expanded into all 16 resources plus checkpoint overhead.
    let step_reserve = reserve.saturating_add(64 * 1024 * 1024);
    crate::config::require_free_space(storage_root, step_reserve)?;
    let mut runtime = bog_runtime::Runtime::open_with_limits(
        path,
        definition,
        bog_runtime::MAX_LOGICAL_BYTES,
        limits,
    )
    .map_err(invalid)?;
    // One-record replay accepts the largest legal source record without envelope
    // expansion pushing a multi-record import over the runtime request bound.
    for record in records {
        if std::time::Instant::now() >= deadline {
            return Err(CloudError::new(
                "timeout",
                "definition build exceeded the configured deadline",
            ));
        }
        crate::config::require_free_space(storage_root, step_reserve)?;
        runtime
            .mutate(&[bog_runtime::Mutation::Upsert {
                key: record.key.clone(),
                data: record.value.clone(),
            }])
            .map_err(invalid)?;
    }
    crate::config::require_free_space(storage_root, reserve)?;
    runtime.checkpoint().map_err(invalid)?;
    let actual = runtime.export(0);
    if actual.record_count != snapshot.0 || actual.source_digest != snapshot.1 {
        return Err(CloudError::new(
            "unavailable",
            "candidate source verification failed",
        ));
    }
    check_deadline(deadline)?;
    drop(runtime);
    std::fs::File::open(
        path.parent()
            .ok_or_else(|| invalid("invalid candidate path"))?,
    )
    .and_then(|f| f.sync_all())
    .map_err(|_| CloudError::new("unavailable", "candidate directory checkpoint failed"))?;
    check_deadline(deadline)?;
    Ok(())
}
fn check_deadline(deadline: std::time::Instant) -> Result<(), CloudError> {
    if std::time::Instant::now() >= deadline {
        Err(CloudError::new(
            "timeout",
            "definition build exceeded the configured deadline",
        ))
    } else {
        Ok(())
    }
}
pub(crate) fn recover_candidates(
    root: &std::path::Path,
    registry: &Registry,
) -> Result<(), CloudError> {
    for instance in std::fs::read_dir(root)
        .map_err(|_| CloudError::new("unavailable", "cannot inspect candidate recovery"))?
    {
        let instance = instance
            .map_err(|_| CloudError::new("unavailable", "cannot inspect candidate recovery"))?;
        if !instance.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(id) = uuid::Uuid::parse_str(&instance.file_name().to_string_lossy()) else {
            continue;
        };
        let active = registry.storage_dir(BogId(id))?;
        for entry in std::fs::read_dir(instance.path())
            .map_err(|_| CloudError::new("unavailable", "cannot inspect candidate recovery"))?
        {
            let entry = entry
                .map_err(|_| CloudError::new("unavailable", "cannot inspect candidate recovery"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != active
                && name
                    .strip_prefix("candidate-")
                    .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
                && entry.file_type().is_ok_and(|t| t.is_dir())
            {
                std::fs::remove_dir_all(entry.path()).map_err(|_| {
                    CloudError::new("unavailable", "cannot remove interrupted candidate")
                })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    const OWNER: &str = "test-definition-owner-token-01234567890123456789";
    fn registry() -> (tempfile::TempDir, Arc<Registry>, crate::Principal) {
        let temp = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(&temp.path().join("registry.sqlite")).unwrap());
        let auth = crate::Auth::new_with_legacy_limit(registry.clone(), OWNER, 8).unwrap();
        let principal = auth.authenticate(OWNER).unwrap();
        (temp, registry, principal)
    }
    #[test]
    fn definitions_and_create_idempotency_are_atomic_and_content_sensitive() {
        let (_temp, registry, p) = registry();
        let d = Definition::records_v1();
        let first = registry
            .create_defined(&p, "configured", "same", &d, 8)
            .unwrap();
        let repeated = registry
            .create_defined(&p, "configured", "same", &d, 8)
            .unwrap();
        assert_eq!(first.id, repeated.id);
        assert_eq!(registry.definition(first.id).unwrap().definition, d);
        let mut changed = d.clone();
        changed.expose.remove("get");
        assert_eq!(
            registry
                .create_defined(&p, "configured", "same", &changed, 8)
                .unwrap_err()
                .code,
            "conflict"
        );
        assert_eq!(
            registry
                .create("configured", "records-v1", "same")
                .unwrap_err()
                .code,
            "conflict"
        );
        assert_eq!(registry.list().unwrap().len(), 1);
    }
    #[test]
    fn interrupted_jobs_fail_without_changing_the_committed_revision() {
        let (temp, registry, p) = registry();
        let d = Definition::records_v1();
        let bog = registry
            .create_defined(&p, "configured", "same", &d, 8)
            .unwrap();
        for state in ["building", "activating"] {
            registry.connection().unwrap().execute("INSERT INTO definition_jobs(id,bog_id,status,payload,created_at) VALUES (?1,?2,?3,'{}',0)",params![state,bog.id.to_string(),state]).unwrap();
        }
        assert!(registry.building(bog.id).unwrap());
        registry.recover_definition_jobs().unwrap();
        assert!(!registry.building(bog.id).unwrap());
        for state in ["building", "activating"] {
            assert_eq!(
                registry.definition_job(bog.id, state).unwrap()["status"],
                "failed"
            );
        }
        let active = registry.definition(bog.id).unwrap();
        assert_eq!(active.revision, 1);
        assert_eq!(active.storage_dir, "data");
        registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE bog_definitions SET revision=2,storage_dir=?2 WHERE bog_id=?1",
                params![
                    bog.id.to_string(),
                    format!("candidate-{}", uuid::Uuid::new_v4())
                ],
            )
            .unwrap();
        registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE definition_jobs SET status='succeeded' WHERE id='activating'",
                [],
            )
            .unwrap();
        registry.recover_definition_jobs().unwrap();
        assert_eq!(registry.definition(bog.id).unwrap().revision, 2);
        assert_eq!(
            registry.definition_job(bog.id, "activating").unwrap()["status"],
            "succeeded"
        );
        let instances = temp.path().join("instances");
        let directory = instances.join(bog.id.to_string());
        let active = directory.join(registry.storage_dir(bog.id).unwrap());
        let orphan = directory.join(format!("candidate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&active).unwrap();
        std::fs::create_dir_all(&orphan).unwrap();
        recover_candidates(&instances, &registry).unwrap();
        assert!(active.exists());
        assert!(!orphan.exists());
    }
    #[test]
    fn expired_candidate_build_never_changes_source() {
        let temp = tempfile::tempdir().unwrap();
        let record = bog_runtime::Record {
            key: "one".into(),
            value: bog_runtime::JsonDocument::try_from_value(json!({"value":1})).unwrap(),
        };
        let result = rebuild(
            &temp.path().join("candidate"),
            Definition::records_v1(),
            &[record],
            &(1, "wrong".into()),
            std::time::Instant::now(),
            0,
            bog_definition::Limits::default(),
        );
        assert_eq!(result.unwrap_err().code, "timeout");
    }
    #[test]
    fn revoked_bog_identity_remains_available_only_for_worker_cleanup() {
        let (_temp, registry, p) = registry();
        let bog = registry
            .create_defined(&p, "removed", "removed", &Definition::records_v1(), 8)
            .unwrap();
        registry
            .connection()
            .unwrap()
            .execute(
                "UPDATE bogs SET deleted_at=1 WHERE id=?1",
                [bog.id.to_string()],
            )
            .unwrap();
        assert_eq!(registry.definition(bog.id).unwrap_err().code, "not_found");
        assert!(registry.worker_definition(bog.id).unwrap().configured);
    }
    #[test]
    fn expired_empty_candidate_never_opens_store() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("candidate");
        let result = rebuild(
            &path,
            Definition::records_v1(),
            &[],
            &(0, String::new()),
            std::time::Instant::now(),
            0,
            bog_definition::Limits::default(),
        );
        assert_eq!(result.unwrap_err().code, "timeout");
        assert!(!path.exists());
    }
    #[test]
    fn capacity_rejection_leaves_candidate_uncreated() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("candidate");
        let result = rebuild(
            &path,
            Definition::records_v1(),
            &[],
            &(0, String::new()),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            u64::MAX,
            bog_definition::Limits::default(),
        );
        assert_eq!(result.unwrap_err().code, "capacity");
        assert!(!path.exists());
    }
    #[test]
    fn candidate_digest_mismatch_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let result = rebuild(
            &temp.path().join("candidate"),
            Definition::records_v1(),
            &[],
            &(0, "wrong".into()),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
            0,
            bog_definition::Limits::default(),
        );
        assert_eq!(result.unwrap_err().code, "unavailable");
    }
    #[test]
    fn private_resource_is_omitted_from_discovery() {
        let mut definition = Definition::records_v1();
        definition.resources.insert(
            "private".into(),
            bog_definition::Resource {
                stages: vec![],
                terminal: bog_definition::Terminal::Count,
            },
        );
        let active = ActiveDefinition {
            digest: definition.digest().unwrap(),
            definition,
            revision: 1,
            storage_dir: "data".into(),
            configured: true,
        };
        assert!(!resources(&active).to_string().contains("private"));
    }

    #[test]
    fn scoped_definition_creation_preserves_workspace_limit_independently_of_resident_capacity() {
        let (_temp, registry, _) = registry();
        let auth = crate::Auth::new_with_legacy_limit(registry.clone(), OWNER, 8).unwrap();
        let identity = crate::oauth::VerifiedIdentity::native(
            "definition-quota-test",
            crate::registry::now() as u64 + 3600,
        );
        let (_, workspace) = auth.provision_identity(&identity).unwrap();
        let principal = auth
            .principal_from_verified(&identity, workspace.id)
            .unwrap();
        // The supplied legacy/resident allowance does not replace this
        // ordinary workspace's three-Bog quota or the platform's 32-Bog cap.
        for name in ["first", "second", "third"] {
            registry
                .create_defined(&principal, name, name, &Definition::records_v1(), 1)
                .unwrap();
        }
        assert_eq!(
            registry
                .create_defined(&principal, "fourth", "fourth", &Definition::records_v1(), 1)
                .unwrap_err()
                .code,
            "capacity"
        );
    }

    #[tokio::test]
    async fn lowered_definition_limits_gate_create_validate_plan_and_discovery() {
        let temp = tempfile::Builder::new()
            .prefix("bdl-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut config = crate::config::Config::new(
            temp.path().join("r"),
            std::path::PathBuf::from("/unneeded-worker"),
        );
        config.composable_enabled = true;
        config.composable_limits.resources = 1;
        config.composable_limits.hits = 3;
        let service = crate::CloudService::open(config, OWNER).unwrap();
        let owner = service.auth.authenticate(OWNER).unwrap();
        let definition = serde_json::to_value(Definition::records_v1()).unwrap();
        for operation in [
            crate::Operation::ValidateDefinition {
                definition: definition.clone(),
            },
            crate::Operation::CreateDefinedBog {
                name: "too-many".into(),
                definition: definition.clone(),
                idempotency_key: "one".into(),
            },
        ] {
            assert_eq!(
                service.execute(&owner, operation).await.err().unwrap().code,
                "invalid_request"
            );
        }
        assert!(service.registry.list().unwrap().is_empty());
        let legacy = service
            .registry
            .create("legacy", "records-v1", "legacy")
            .unwrap();
        assert_eq!(
            service
                .execute(
                    &owner,
                    crate::Operation::PlanDefinitionUpdate {
                        bog_id: legacy.id,
                        definition,
                        expected_revision: 1
                    }
                )
                .await
                .err()
                .unwrap()
                .code,
            "invalid_request"
        );
        let catalog = service
            .execute(&owner, crate::Operation::ListComponents)
            .await
            .unwrap();
        assert_eq!(catalog.body["limits"]["resources"], 1);
        assert_eq!(catalog.body["limits"]["hits"], 3);
        assert!(
            service
                .execute(
                    &owner,
                    crate::Operation::DescribeDefinition { bog_id: legacy.id }
                )
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn configured_restore_obeys_shared_build_admission() {
        use sha2::{Digest, Sha256};
        let temp = tempfile::Builder::new()
            .prefix("bdq-")
            .tempdir_in("/tmp")
            .unwrap();
        let config = crate::config::Config::new(
            temp.path().join("r"),
            std::path::PathBuf::from("/unneeded-worker"),
        );
        let service = crate::CloudService::open(config, OWNER).unwrap();
        let archive_id = uuid::Uuid::new_v4().to_string();
        let archive = temp.path().join("r/backups").join(&archive_id);
        std::fs::create_dir_all(&archive).unwrap();
        let records = b"[]";
        std::fs::write(archive.join("records.json"), records).unwrap();
        let definition = Definition::records_v1();
        let manifest = crate::backup::Manifest {
            format_version: 2,
            definition: Some(ActiveDefinition {
                digest: definition.digest().unwrap(),
                definition,
                revision: 1,
                storage_dir: "data".into(),
                configured: true,
            }),
            template_id: "records-v1".into(),
            template_version: "records-v1".into(),
            source_bog_id: uuid::Uuid::new_v4().to_string(),
            build_commit: "test".into(),
            created_at: 0,
            files: vec![crate::backup::ArchiveFile {
                path: "records.json".into(),
                size: 2,
                sha256: format!("{:x}", Sha256::digest(records)),
            }],
            logical: crate::backup::LogicalSnapshot {
                count: 0,
                records_sha256: format!("{:x}", Sha256::digest(b"")),
            },
        };
        write_json(
            &archive.join("manifest.json"),
            &serde_json::to_value(manifest).unwrap(),
        )
        .unwrap();
        let permit = service.supervisor.reserve_build().unwrap();
        assert_eq!(
            service
                .supervisor
                .restore(&archive_id, "restored")
                .await
                .unwrap_err()
                .code,
            "capacity"
        );
        assert!(service.registry.list().unwrap().is_empty());
        drop(permit);
        assert!(service.supervisor.reserve_build().is_ok());
    }
    #[tokio::test]
    async fn journaled_build_pauses_writes_preserves_reads_and_excludes_maintenance() {
        let temp = tempfile::Builder::new()
            .prefix("bdj-")
            .tempdir_in("/tmp")
            .unwrap();
        let binary = std::env::var_os("BOG_TEST_WORKER")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../target/debug/bog-records-worker")
            });
        let mut config = crate::config::Config::new(temp.path().join("r"), binary);
        config.composable_enabled = true;
        let service = crate::CloudService::open(config, OWNER).unwrap();
        let owner = service.auth.authenticate(OWNER).unwrap();
        let bog = service
            .registry
            .create("records", "records-v1", "one")
            .unwrap();
        service
            .execute(
                &owner,
                crate::Operation::UpsertRecord {
                    bog_id: bog.id,
                    key: "one".into(),
                    data: json!({"value":1}),
                },
            )
            .await
            .unwrap();
        let listed = service
            .execute(&owner, crate::Operation::ListResources { bog_id: bog.id })
            .await
            .unwrap();
        assert_eq!(listed.body["resources"].as_array().unwrap().len(), 2);
        for (resource, query, expected) in [
            (
                "docs",
                json!({"action":"get","key":"one"}),
                json!({"value":1}),
            ),
            ("total", json!({}), json!(1)),
        ] {
            let response = service
                .execute(
                    &owner,
                    crate::Operation::QueryResource {
                        bog_id: bog.id,
                        resource: resource.into(),
                        query,
                    },
                )
                .await
                .unwrap();
            assert_eq!(response.body["data"], expected);
        }
        service.registry.connection().unwrap().execute("INSERT INTO definition_jobs(id,bog_id,status,payload,created_at) VALUES ('test',?1,'building','{}',0)",[bog.id.to_string()]).unwrap();
        let read = service
            .execute(
                &owner,
                crate::Operation::GetRecord {
                    bog_id: bog.id,
                    key: "one".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(read.body["data"]["value"], 1);
        let error = service
            .execute(
                &owner,
                crate::Operation::UpsertRecord {
                    bog_id: bog.id,
                    key: "one".into(),
                    data: json!({"value":2}),
                },
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "writes_paused");
        assert_eq!(
            service.supervisor.backup(bog.id).await.unwrap_err().code,
            "conflict"
        );
        assert_eq!(
            service.supervisor.stop(bog.id).await.unwrap_err().code,
            "conflict"
        );
        assert!(service.auth.delete_bog(&owner, bog.id, bog.id).is_err());
        assert!(!service.registry.is_deleted(bog.id).unwrap());
        service.registry.recover_definition_jobs().unwrap();
        service
            .execute(
                &owner,
                crate::Operation::UpsertRecord {
                    bog_id: bog.id,
                    key: "one".into(),
                    data: json!({"value":2}),
                },
            )
            .await
            .unwrap();
        service.supervisor.shutdown().await.unwrap();
    }
}
