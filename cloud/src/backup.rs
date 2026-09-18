use crate::CloudError;
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<crate::definitions::ActiveDefinition>,
    pub format_version: u32,
    pub template_id: String,
    pub template_version: String,
    pub source_bog_id: String,
    pub build_commit: String,
    pub created_at: i64,
    pub files: Vec<ArchiveFile>,
    pub logical: LogicalSnapshot,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalSnapshot {
    pub count: u64,
    pub records_sha256: String,
}
fn snapshot(path: &std::path::Path) -> Result<LogicalSnapshot, CloudError> {
    let mut stream =
        fold::stream::KeyedStream::<String, bog_cloud_records::JsonDocument, _>::try_new(
            path,
            (
                fold::pipeline::terminal::Table::new("docs"),
                fold::pipeline::terminal::Count::new("total"),
            ),
        )
        .map_err(|_| CloudError::new("unavailable", "cannot verify closed database"))?;
    let result = stream.rtx(|(docs, total)| {
        let mut digest = Sha256::new();
        let mut count = 0u64;
        for (key, value) in docs.iter() {
            if stream.get(&key).as_ref() != Some(&value) {
                return Err(invalid());
            }
            let bytes = serde_json::to_vec(&(key, value)).map_err(|_| invalid())?;
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
            count += 1;
        }
        if total.get() != count as i64 {
            return Err(invalid());
        }
        Ok(LogicalSnapshot {
            count,
            records_sha256: format!("{:x}", digest.finalize()),
        })
    });
    stream
        .try_checkpoint()
        .map_err(|_| CloudError::new("unavailable", "verification checkpoint failed"))?;
    result
}
pub fn validate_manifest(m: &Manifest) -> Result<(), CloudError> {
    let configured = m.definition.is_some();
    if let Some(active) = &m.definition {
        active.definition.validate().map_err(|_| invalid())?;
        if active.revision == 0
            || active.revision >= i64::MAX as u64
            || active.definition.digest().map_err(|_| invalid())? != active.digest
            || !active.configured
        {
            return Err(invalid());
        }
        if m.files.len() != 1 || m.files[0].path != "records.json" {
            return Err(invalid());
        }
    }
    if m.format_version != if configured { 2 } else { 1 }
        || m.template_id != "records-v1"
        || m.template_version != "records-v1"
        || m.files.is_empty()
        || m.files.len() > 100_000
    {
        return Err(invalid());
    }
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for file in &m.files {
        if !(file.path.starts_with("data/")
            || file.path == "data.schema"
            || (configured && file.path == "records.json"))
            || file
                .path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
            || !Path::new(&file.path)
                .components()
                .all(|p| matches!(p, Component::Normal(_)))
            || !seen.insert(&file.path)
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        total = total.checked_add(file.size).ok_or_else(invalid)?;
        if total > MAX_ARCHIVE_BYTES {
            return Err(invalid());
        }
    }
    Ok(())
}

use crate::{
    BogId, DesiredState, ObservedState,
    supervisor::{Supervisor, private_directory},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Component, Path},
    sync::Arc,
};
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
fn invalid() -> CloudError {
    CloudError::new("invalid_archive", "invalid or incompatible backup")
}
fn io_error(_: std::io::Error) -> CloudError {
    CloudError::new("unavailable", "backup filesystem operation failed")
}
fn digest_file(path: &Path) -> Result<(u64, String), CloudError> {
    let mut f = fs::File::open(path).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut size = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = f.read(&mut buffer).map_err(io_error)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > MAX_ARCHIVE_BYTES {
            return Err(invalid());
        }
        digest.update(&buffer[..n]);
    }
    Ok((size, format!("{:x}", digest.finalize())))
}
fn real_file(root: &Path, path: &str) -> Result<std::path::PathBuf, CloudError> {
    let mut current = root.to_path_buf();
    for component in Path::new(path).components() {
        let Component::Normal(part) = component else {
            return Err(invalid());
        };
        current.push(part);
        if fs::symlink_metadata(&current)
            .map_err(io_error)?
            .file_type()
            .is_symlink()
        {
            return Err(invalid());
        }
    }
    if !fs::metadata(&current).map_err(io_error)?.is_file() {
        return Err(invalid());
    }
    Ok(current)
}
fn collect(root: &Path, current: &Path, files: &mut Vec<ArchiveFile>) -> Result<(), CloudError> {
    for entry in fs::read_dir(current).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let meta = entry.file_type().map_err(io_error)?;
        if meta.is_symlink() {
            return Err(invalid());
        }
        if meta.is_dir() {
            collect(root, &entry.path(), files)?;
        } else if meta.is_file() {
            if files.len() >= 100_000 {
                return Err(invalid());
            }
            let path = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| invalid())?
                .to_str()
                .ok_or_else(invalid)?
                .to_owned();
            let (size, sha256) = digest_file(&entry.path())?;
            files.push(ArchiveFile { path, size, sha256 });
        } else {
            return Err(invalid());
        }
    }
    Ok(())
}
fn copy_files(source: &Path, target: &Path, manifest: &Manifest) -> Result<(), CloudError> {
    for entry in &manifest.files {
        let input = real_file(source, &entry.path)?;
        let output = target.join(&entry.path);
        fs::create_dir_all(output.parent().ok_or_else(invalid)?).map_err(io_error)?;
        let mut reader = fs::File::open(input).map_err(io_error)?;
        let mut writer = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)
            .map_err(io_error)?;
        let copied = std::io::copy(
            &mut std::io::Read::by_ref(&mut reader).take(entry.size + 1),
            &mut writer,
        )
        .map_err(io_error)?;
        writer.sync_all().map_err(io_error)?;
        if copied != entry.size || digest_file(&output)? != (entry.size, entry.sha256.clone()) {
            return Err(invalid());
        }
    }
    sync_directories(target)?;
    Ok(())
}
fn sync_directories(root: &Path) -> Result<(), CloudError> {
    for entry in fs::read_dir(root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if entry.file_type().map_err(io_error)?.is_dir() {
            sync_directories(&entry.path())?;
        }
    }
    fs::File::open(root)
        .map_err(io_error)?
        .sync_all()
        .map_err(io_error)
}
pub(crate) fn recover_staging(root: &Path) -> Result<(), CloudError> {
    if !fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        return Ok(());
    }
    for entry in fs::read_dir(root).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name
            .strip_prefix(".staging-")
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            && entry.file_type().map_err(io_error)?.is_dir()
        {
            fs::remove_dir_all(entry.path()).map_err(io_error)?;
        }
    }
    Ok(())
}
fn write_backup(root: &Path, source: &Path, id: BogId) -> Result<String, CloudError> {
    private_directory(root)?;
    let logical = snapshot(&source.join("data"))?;
    // Hold Fjall's actual OS lock during the entire closed-store copy, including
    // when the manager has not yet adopted a surviving worker.
    use std::os::unix::fs::OpenOptionsExt;
    let store_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source.join("data/lock"))
        .map_err(io_error)?;
    store_lock
        .try_lock()
        .map_err(|_| CloudError::new("unavailable", "database is still open"))?;
    let archive_id = uuid::Uuid::new_v4().to_string();
    let stage = root.join(format!(".staging-{archive_id}"));
    fs::create_dir(&stage).map_err(io_error)?;
    let result = (|| {
        let mut files = vec![];
        collect(source, &source.join("data"), &mut files)?;
        let (size, sha256) = digest_file(&source.join("data.schema"))?;
        files.push(ArchiveFile {
            path: "data.schema".into(),
            size,
            sha256,
        });
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let manifest = Manifest {
            definition: None,
            format_version: 1,
            template_id: "records-v1".into(),
            template_version: "records-v1".into(),
            source_bog_id: id.to_string(),
            build_commit: option_env!("BOG_BUILD_COMMIT")
                .unwrap_or("development")
                .into(),
            created_at: crate::registry::now(),
            files,
            logical,
        };
        validate_manifest(&manifest)?;
        crate::config::require_free_space(
            root,
            manifest
                .files
                .iter()
                .map(|f| f.size)
                .sum::<u64>()
                .saturating_add(64 * 1024 * 1024),
        )?;
        copy_files(source, &stage, &manifest)?;
        let mut marker = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage.join("manifest.json"))
            .map_err(io_error)?;
        marker
            .write_all(&serde_json::to_vec(&manifest).map_err(|_| invalid())?)
            .map_err(io_error)?;
        marker.sync_all().map_err(io_error)?;
        fs::File::open(&stage)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
        fs::rename(&stage, root.join(&archive_id)).map_err(io_error)?;
        fs::File::open(root)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
        Ok(archive_id)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(stage);
    }
    result
}
pub(crate) fn load_archive(
    root: &Path,
    id: &str,
) -> Result<(std::path::PathBuf, Manifest), CloudError> {
    let id = uuid::Uuid::parse_str(id)
        .map_err(|_| invalid())?
        .to_string();
    let archive = root.join(id);
    if !fs::symlink_metadata(&archive).map_err(io_error)?.is_dir() {
        return Err(invalid());
    }
    let marker = real_file(&archive, "manifest.json")?;
    if fs::metadata(&marker).map_err(io_error)?.len() > 16 * 1024 * 1024 {
        return Err(invalid());
    }
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(marker).map_err(io_error)?).map_err(|_| invalid())?;
    validate_manifest(&manifest)?;
    for file in &manifest.files {
        if digest_file(&real_file(&archive, &file.path)?)? != (file.size, file.sha256.clone()) {
            return Err(invalid());
        }
    }
    Ok((archive, manifest))
}
impl Supervisor {
    /// Shield maintenance cleanup from client disconnect/cancellation.
    pub async fn backup(self: &Arc<Self>, id: BogId) -> Result<String, CloudError> {
        let supervisor = self.clone();
        tokio::spawn(async move { supervisor.backup_inner(id).await })
            .await
            .map_err(|_| CloudError::new("unavailable", "backup task failed"))?
    }
    async fn backup_inner(self: Arc<Self>, id: BogId) -> Result<String, CloudError> {
        let guard = self.gate(id)?.write_owned().await;
        if self.registry.building(id)? {
            return Err(CloudError::new(
                "conflict",
                "definition build is in progress",
            ));
        }
        let original = self.registry.get(id)?;
        if original.desired_state == DesiredState::Running {
            self.start_locked(id).await?;
        }
        self.registry
            .set_status(id, ObservedState::Maintenance, None)?;
        let stopped = self.stop_locked(id, false).await;
        let result = match stopped {
            Ok(()) => {
                self.registry
                    .set_status(id, ObservedState::Maintenance, None)?;
                let root = self.config.root.join("backups");
                let source = self.instance_dir(id);
                let active = self.registry.definition(id)?;
                match tokio::task::spawn_blocking(move || {
                    if active.configured {
                        write_configured_backup(&root, &source, id, active)
                    } else {
                        write_backup(&root, &source, id)
                    }
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(CloudError::new("unavailable", "backup copy failed")),
                }
            }
            Err(e) => {
                self.registry
                    .set_status(id, ObservedState::Failed, Some("backup_stop_failed"))?;
                return Err(e);
            }
        };
        self.registry.set_status(id, ObservedState::Stopped, None)?;
        drop(guard);
        if original.desired_state == DesiredState::Running {
            self.ensure_running(id).await?;
        }
        result
    }
    pub async fn restore(
        self: &Arc<Self>,
        archive_id: &str,
        name: &str,
    ) -> Result<crate::Bog, CloudError> {
        let supervisor = self.clone();
        let id = archive_id.to_owned();
        let name = name.to_owned();
        tokio::spawn(async move { supervisor.restore_inner(&id, &name).await })
            .await
            .map_err(|_| CloudError::new("unavailable", "restore task failed"))?
    }
    async fn restore_inner(
        self: Arc<Self>,
        archive_id: &str,
        name: &str,
    ) -> Result<crate::Bog, CloudError> {
        let root = self.config.root.join("backups");
        let archive_id = archive_id.to_owned();
        let (archive, manifest) =
            tokio::task::spawn_blocking(move || load_archive(&root, &archive_id))
                .await
                .map_err(|_| invalid())??;
        if let Some(active) = &manifest.definition {
            self.config
                .composable_limits
                .validate_definition(&active.definition)
                .map_err(crate::definitions::invalid)?;
        }
        let _build_permit = if manifest.definition.is_some() {
            Some(self.reserve_build()?)
        } else {
            None
        };
        let candidate_slot = if manifest.definition.is_some() {
            Some(self.reserve_candidate()?)
        } else {
            None
        };
        crate::config::require_free_space(
            &self.config.root,
            manifest
                .files
                .iter()
                .map(|f| f.size)
                .sum::<u64>()
                .saturating_add(self.config.min_free_bytes),
        )?;
        let bog = self.registry.create_restoring(
            name,
            &format!("restore-{}", uuid::Uuid::new_v4()),
            self.config.max_active,
        )?;
        self.registry
            .set_status(bog.id, ObservedState::Restoring, None)?;
        let destination = self.instance_dir(bog.id);
        let restored_definition = manifest.definition.clone();
        let reserve = self.config.min_free_bytes;
        let limits = self.config.composable_limits.clone();
        let timeout = limits.build_timeout_seconds;
        let result = tokio::task::spawn_blocking(move || {
            fs::create_dir(&destination).map_err(io_error)?;
            private_directory(&destination)?;
            if let Some(active) = &manifest.definition {
                let records: Vec<bog_runtime::Record> = serde_json::from_slice(
                    &fs::read(real_file(&archive, "records.json")?).map_err(io_error)?,
                )
                .map_err(|_| invalid())?;
                crate::definitions::rebuild(
                    &destination.join("data"),
                    active.definition.clone(),
                    &records,
                    &(
                        manifest.logical.count as usize,
                        manifest.logical.records_sha256.clone(),
                    ),
                    std::time::Instant::now() + std::time::Duration::from_secs(timeout),
                    reserve,
                    limits,
                )?;
            } else {
                copy_files(&archive, &destination, &manifest)?;
                if snapshot(&destination.join("data"))? != manifest.logical {
                    return Err(invalid());
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| invalid())?;
        if let Err(e) = result {
            self.registry
                .set_status(bog.id, ObservedState::Failed, Some("restore_failed"))?;
            self.registry.set_desired(bog.id, false)?;
            return Err(e);
        }
        drop(candidate_slot);
        if let Some(active) = restored_definition {
            self.registry.connection()?.execute("INSERT INTO bog_definitions(bog_id,definition,digest,revision,storage_dir) VALUES (?1,?2,?3,?4,'data')",rusqlite::params![bog.id.to_string(),active.definition.normalized_json().map_err(|_|invalid())?,active.digest,active.revision as i64]).map_err(crate::registry::db_error)?;
        }
        self.registry
            .set_status(bog.id, ObservedState::Creating, None)?;
        match self.ensure_running(bog.id).await {
            Ok(_) => self.registry.get(bog.id),
            Err(e) => {
                self.registry
                    .set_status(bog.id, ObservedState::Failed, Some("restore_failed"))?;
                self.registry.set_desired(bog.id, false)?;
                Err(e)
            }
        }
    }
}

fn write_configured_backup(
    root: &Path,
    source: &Path,
    id: BogId,
    active: crate::definitions::ActiveDefinition,
) -> Result<String, CloudError> {
    private_directory(root)?;
    let mut runtime =
        bog_runtime::Runtime::open(source.join(&active.storage_dir), active.definition.clone())
            .map_err(crate::definitions::invalid)?;
    let first = runtime.export(0);
    let logical = LogicalSnapshot {
        count: first.record_count as u64,
        records_sha256: first.source_digest.clone(),
    };
    let mut records = first.records;
    let mut next = first.next_offset;
    while let Some(offset) = next {
        let page = runtime.export(offset);
        if page.records.is_empty() {
            return Err(invalid());
        }
        records.extend(page.records);
        next = page.next_offset;
    }
    runtime.checkpoint().map_err(crate::definitions::invalid)?;
    if records.len() as u64 != logical.count {
        return Err(invalid());
    }
    crate::config::require_free_space(root, 64 * 1024 * 1024 + 16 * 1024 * 1024)?;
    let archive_id = uuid::Uuid::new_v4().to_string();
    let stage = root.join(format!(".staging-{archive_id}"));
    private_directory(&stage)?;
    let result = (|| {
        crate::definitions::write_json(
            &stage.join("records.json"),
            &serde_json::to_value(&records).map_err(|_| invalid())?,
        )?;
        let (size, sha256) = digest_file(&stage.join("records.json"))?;
        let manifest = Manifest {
            format_version: 2,
            definition: Some(active),
            template_id: "records-v1".into(),
            template_version: "records-v1".into(),
            source_bog_id: id.to_string(),
            build_commit: option_env!("BOG_BUILD_COMMIT")
                .unwrap_or("development")
                .into(),
            created_at: crate::registry::now(),
            files: vec![ArchiveFile {
                path: "records.json".into(),
                size,
                sha256,
            }],
            logical,
        };
        validate_manifest(&manifest)?;
        crate::definitions::write_json(
            &stage.join("manifest.json"),
            &serde_json::to_value(manifest).map_err(|_| invalid())?,
        )?;
        fs::rename(&stage, root.join(&archive_id)).map_err(io_error)?;
        fs::File::open(root)
            .and_then(|f| f.sync_all())
            .map_err(io_error)?;
        Ok(archive_id)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(stage);
    }
    result
}
