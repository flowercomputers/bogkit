use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::archive::{Archive, Entry};

const MAX_FILES: usize = 1_000;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const STREAM_BUFFER_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub files: Vec<FileDeclaration>,
    pub entry_program: String,
    pub required_tool_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileDeclaration {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub tool_ids: Vec<String>,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
    pub archive_members: usize,
    pub streamed_bytes: u64,
    pub stream_buffer_bytes: u64,
    pub total_member_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub bundle: String,
    pub ready: bool,
    pub staged: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
    pub metrics: Metrics,
}

struct LoadedManifest {
    value: Manifest,
    raw_sha256: String,
}

pub fn check(bundle: &Path, inventory_path: &Path, staging: Option<&Path>) -> Report {
    let mut diagnostics = Vec::new();
    let mut metrics = Metrics {
        stream_buffer_bytes: STREAM_BUFFER_BYTES,
        ..Metrics::default()
    };
    let inventory = load_inventory(inventory_path, &mut diagnostics);
    let mut archive = match Archive::open(bundle) {
        Ok(archive) => archive,
        Err(error) => {
            add(&mut diagnostics, "archive_invalid", "", &error.to_string());
            diagnostics.sort();
            return Report {
                bundle: bundle.display().to_string(),
                ready: false,
                staged: None,
                diagnostics,
                metrics,
            };
        }
    };
    metrics.archive_members = archive.entries.len();
    metrics.total_member_bytes = archive.entries.iter().map(|entry| entry.size).sum();

    validate_archive_inventory(&archive.entries, &mut diagnostics);
    if archive.entries.len().saturating_sub(1) > MAX_FILES {
        add(
            &mut diagnostics,
            "archive_too_many_files",
            "",
            &format!("maximum is {MAX_FILES}"),
        );
    }
    if metrics.total_member_bytes > MAX_TOTAL_BYTES {
        add(
            &mut diagnostics,
            "archive_oversized",
            "",
            &format!(
                "{} bytes exceeds {} byte policy",
                metrics.total_member_bytes, MAX_TOTAL_BYTES
            ),
        );
    }

    let manifest_indexes: Vec<usize> = archive
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| (entry.name == "manifest.json").then_some(index))
        .collect();
    if manifest_indexes.is_empty() {
        add(
            &mut diagnostics,
            "manifest_missing",
            "manifest.json",
            "exact root member is required",
        );
    } else if manifest_indexes.len() > 1 {
        add(
            &mut diagnostics,
            "manifest_duplicate",
            "manifest.json",
            "manifest must occur exactly once",
        );
    }

    let manifest = manifest_indexes.first().and_then(|&index| {
        let entry = archive.entries[index].clone();
        read_manifest(&mut archive, &entry, &mut diagnostics, &mut metrics)
    });

    if let Some(manifest) = &manifest {
        validate_manifest(
            &mut archive,
            &manifest.value,
            inventory.as_ref(),
            &mut diagnostics,
            &mut metrics,
        );
    }

    diagnostics.sort();
    diagnostics.dedup();
    let mut staged = None;
    if diagnostics.is_empty()
        && let (Some(staging), Some(manifest)) = (staging, manifest.as_ref())
    {
        match stage(&mut archive, &manifest.value, &manifest.raw_sha256, staging) {
            Ok(path) => staged = Some(path.display().to_string()),
            Err(error) => add(&mut diagnostics, "staging_failed", "", &error.to_string()),
        }
    }
    diagnostics.sort();
    Report {
        bundle: bundle.display().to_string(),
        ready: diagnostics.is_empty(),
        staged,
        diagnostics,
        metrics,
    }
}

fn load_inventory(path: &Path, diagnostics: &mut Vec<Diagnostic>) -> Option<Inventory> {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Inventory>(&bytes) {
            Ok(inventory) => {
                let mut seen = BTreeSet::new();
                for tool in &inventory.tool_ids {
                    if tool.is_empty() {
                        add(
                            diagnostics,
                            "inventory_tool_id_empty",
                            "",
                            "tool IDs cannot be empty",
                        );
                    } else if !seen.insert(tool) {
                        add(
                            diagnostics,
                            "inventory_tool_id_duplicate",
                            tool,
                            "tool ID occurs more than once",
                        );
                    }
                }
                Some(inventory)
            }
            Err(error) => {
                add(diagnostics, "inventory_invalid", "", &error.to_string());
                None
            }
        },
        Err(error) => {
            add(diagnostics, "inventory_unreadable", "", &error.to_string());
            None
        }
    }
}

fn validate_archive_inventory(entries: &[Entry], diagnostics: &mut Vec<Diagnostic>) {
    let mut exact: BTreeMap<&str, usize> = BTreeMap::new();
    let mut folded: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for entry in entries {
        if let Err(reason) = safe_relative_path(&entry.name) {
            add(diagnostics, "archive_path_unsafe", &entry.name, reason);
        }
        *exact.entry(&entry.name).or_default() += 1;
        folded
            .entry(entry.name.to_lowercase())
            .or_default()
            .push(&entry.name);
        if entry.encrypted {
            add(
                diagnostics,
                "archive_member_encrypted",
                &entry.name,
                "encrypted members are unsupported",
            );
        }
        if entry.method != 0 || entry.compressed_size != entry.size {
            add(
                diagnostics,
                "archive_compression_unsupported",
                &entry.name,
                "prototype requires stored members",
            );
        }
    }
    add_path_prefix_collisions(
        exact.keys().copied(),
        diagnostics,
        "archive_path_type_collision",
    );
    for (path, count) in exact {
        if count > 1 {
            add(
                diagnostics,
                "archive_name_duplicate",
                path,
                "member name occurs more than once",
            );
        }
    }
    for names in folded.values_mut() {
        names.sort_unstable();
        names.dedup();
        if names.len() > 1 {
            add(
                diagnostics,
                "archive_name_case_collision",
                &names.join(" | "),
                "member names differ only by case",
            );
        }
    }
}

fn read_manifest(
    archive: &mut Archive,
    entry: &Entry,
    diagnostics: &mut Vec<Diagnostic>,
    metrics: &mut Metrics,
) -> Option<LoadedManifest> {
    if entry.size > MAX_MANIFEST_BYTES {
        add(
            diagnostics,
            "manifest_oversized",
            "manifest.json",
            "manifest exceeds 1 MiB",
        );
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size).unwrap_or(0));
    match archive.stream_entry(entry, Some(&mut bytes)) {
        Ok(result) => {
            metrics.streamed_bytes += result.bytes;
            if result.crc32 != entry.crc32 {
                add(
                    diagnostics,
                    "archive_crc_mismatch",
                    "manifest.json",
                    "ZIP CRC does not match content",
                );
            }
            match serde_json::from_slice(&bytes) {
                Ok(value) => Some(LoadedManifest {
                    value,
                    raw_sha256: result.sha256,
                }),
                Err(error) => {
                    add(
                        diagnostics,
                        "manifest_invalid",
                        "manifest.json",
                        &error.to_string(),
                    );
                    None
                }
            }
        }
        Err(error) => {
            add(
                diagnostics,
                "manifest_unreadable",
                "manifest.json",
                &error.to_string(),
            );
            None
        }
    }
}

#[allow(clippy::too_many_lines)]
fn validate_manifest(
    archive: &mut Archive,
    manifest: &Manifest,
    inventory: Option<&Inventory>,
    diagnostics: &mut Vec<Diagnostic>,
    metrics: &mut Metrics,
) {
    if manifest.version != 1 {
        add(
            diagnostics,
            "manifest_version_unsupported",
            "manifest.json",
            "only version 1 is supported",
        );
    }
    if manifest.files.len() > MAX_FILES {
        add(
            diagnostics,
            "manifest_too_many_files",
            "manifest.json",
            &format!("maximum is {MAX_FILES}"),
        );
    }

    let mut declarations: BTreeMap<&str, &FileDeclaration> = BTreeMap::new();
    let mut folded: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for declaration in &manifest.files {
        if let Err(reason) = safe_relative_path(&declaration.path) {
            add(
                diagnostics,
                "manifest_path_unsafe",
                &declaration.path,
                reason,
            );
        }
        let allowed_extension = Path::new(&declaration.path)
            .extension()
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("nc") || extension.eq_ignore_ascii_case("gcode")
            });
        if !allowed_extension {
            add(
                diagnostics,
                "manifest_extension_disallowed",
                &declaration.path,
                "program must end in .nc or .gcode",
            );
        }
        if !valid_sha256(&declaration.sha256) {
            add(
                diagnostics,
                "manifest_checksum_invalid",
                &declaration.path,
                "SHA-256 must be 64 lowercase hexadecimal characters",
            );
        }
        if declarations
            .insert(&declaration.path, declaration)
            .is_some()
        {
            add(
                diagnostics,
                "manifest_path_duplicate",
                &declaration.path,
                "path is declared more than once",
            );
        }
        folded
            .entry(declaration.path.to_lowercase())
            .or_default()
            .push(&declaration.path);
    }
    for names in folded.values_mut() {
        names.sort_unstable();
        names.dedup();
        if names.len() > 1 {
            add(
                diagnostics,
                "manifest_path_case_collision",
                &names.join(" | "),
                "declared paths differ only by case",
            );
        }
    }
    add_path_prefix_collisions(
        declarations.keys().copied(),
        diagnostics,
        "manifest_path_type_collision",
    );

    if safe_relative_path(&manifest.entry_program).is_err() {
        add(
            diagnostics,
            "entry_program_path_unsafe",
            &manifest.entry_program,
            "entry program must be a safe relative path",
        );
    }
    if !declarations.contains_key(manifest.entry_program.as_str()) {
        add(
            diagnostics,
            "entry_program_undeclared",
            &manifest.entry_program,
            "entry program must be declared",
        );
    }

    let archive_by_name: BTreeMap<&str, &Entry> = archive
        .entries
        .iter()
        .filter(|entry| entry.name != "manifest.json")
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    for path in declarations.keys() {
        if !archive_by_name.contains_key(path) {
            add(
                diagnostics,
                "declared_file_missing",
                path,
                "manifest path is absent from archive",
            );
        }
    }
    for path in archive_by_name.keys() {
        if !declarations.contains_key(path) {
            add(
                diagnostics,
                "archive_file_undeclared",
                path,
                "archive member is absent from manifest",
            );
        }
    }

    if let Some(inventory) = inventory {
        let available: BTreeSet<&str> = inventory.tool_ids.iter().map(String::as_str).collect();
        let mut requested = BTreeSet::new();
        for tool in &manifest.required_tool_ids {
            if tool.is_empty() {
                add(
                    diagnostics,
                    "manifest_tool_id_empty",
                    "",
                    "required tool IDs cannot be empty",
                );
            } else if !requested.insert(tool) {
                add(
                    diagnostics,
                    "manifest_tool_id_duplicate",
                    tool,
                    "required tool ID occurs more than once",
                );
            }
            if !available.contains(tool.as_str()) {
                add(
                    diagnostics,
                    "required_tool_missing",
                    tool,
                    "tool ID is absent from inventory",
                );
            }
        }
    }

    let declared_entries: Vec<(FileDeclaration, Entry)> = manifest
        .files
        .iter()
        .filter_map(|declaration| {
            archive_by_name
                .get(declaration.path.as_str())
                .map(|entry| (declaration.clone(), (*entry).clone()))
        })
        .collect();
    for (declaration, entry) in declared_entries {
        if declaration.bytes != entry.size {
            add(
                diagnostics,
                "declared_size_mismatch",
                &declaration.path,
                &format!(
                    "manifest says {} bytes; ZIP says {}",
                    declaration.bytes, entry.size
                ),
            );
        }
        match archive.stream_entry::<File>(&entry, None) {
            Ok(result) => {
                metrics.streamed_bytes += result.bytes;
                if result.bytes != declaration.bytes {
                    add(
                        diagnostics,
                        "content_size_mismatch",
                        &declaration.path,
                        &format!(
                            "manifest says {} bytes; streamed {}",
                            declaration.bytes, result.bytes
                        ),
                    );
                }
                if result.sha256 != declaration.sha256 {
                    add(
                        diagnostics,
                        "checksum_mismatch",
                        &declaration.path,
                        &format!("expected {}; got {}", declaration.sha256, result.sha256),
                    );
                }
                if result.crc32 != entry.crc32 {
                    add(
                        diagnostics,
                        "archive_crc_mismatch",
                        &declaration.path,
                        "ZIP CRC does not match content",
                    );
                }
            }
            Err(error) => add(
                diagnostics,
                "member_unreadable",
                &declaration.path,
                &error.to_string(),
            ),
        }
    }
}

fn stage(
    archive: &mut Archive,
    manifest: &Manifest,
    manifest_sha256: &str,
    staging: &Path,
) -> std::io::Result<PathBuf> {
    if staging.exists() {
        let metadata = fs::symlink_metadata(staging)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(std::io::Error::other(
                "selected staging root must be a real directory",
            ));
        }
    } else {
        fs::create_dir(staging)?;
    }
    let ready = staging.join("ready");
    match fs::symlink_metadata(&ready) {
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "ready destination already exists",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let pending = create_pending_directory(staging)?;
    let copy_result = stage_into(archive, manifest, manifest_sha256, &pending);
    if let Err(error) = copy_result {
        let _ = fs::remove_dir_all(&pending);
        return Err(error);
    }
    if let Err(error) = fs::rename(&pending, &ready) {
        let _ = fs::remove_dir_all(&pending);
        return Err(error);
    }
    Ok(ready)
}

fn stage_into(
    archive: &mut Archive,
    manifest: &Manifest,
    manifest_sha256: &str,
    pending: &Path,
) -> std::io::Result<()> {
    let declarations: BTreeMap<&str, &FileDeclaration> = manifest
        .files
        .iter()
        .map(|declaration| (declaration.path.as_str(), declaration))
        .collect();
    let entries = archive.entries.clone();
    for entry in entries {
        let relative = safe_relative_path(&entry.name).map_err(std::io::Error::other)?;
        let destination = pending.join(&relative);
        if !destination.starts_with(pending) {
            return Err(std::io::Error::other("destination escaped staging"));
        }
        if let Some(parent) = destination.parent() {
            create_safe_directories(pending, parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        let copied = archive.stream_entry(&entry, Some(&mut file))?;
        file.flush()?;
        file.sync_all()?;

        if copied.bytes != entry.size || copied.crc32 != entry.crc32 {
            return Err(std::io::Error::other(format!(
                "copied content changed for {:?}",
                entry.name
            )));
        }
        let expected_sha256 = if entry.name == "manifest.json" {
            manifest_sha256
        } else {
            declarations
                .get(entry.name.as_str())
                .ok_or_else(|| std::io::Error::other("staging member was not declared"))?
                .sha256
                .as_str()
        };
        if copied.sha256 != expected_sha256 {
            return Err(std::io::Error::other(format!(
                "copied content changed for {:?}",
                entry.name
            )));
        }
    }
    Ok(())
}

fn create_pending_directory(staging: &Path) -> std::io::Result<PathBuf> {
    static NEXT_PENDING: AtomicU64 = AtomicU64::new(0);
    for _ in 0..100 {
        let sequence = NEXT_PENDING.fetch_add(1, Ordering::Relaxed);
        let pending = staging.join(format!(".ready.pending-{}-{sequence}", std::process::id()));
        match fs::create_dir(&pending) {
            Ok(()) => return Ok(pending),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::other(
        "could not allocate a temporary staging directory",
    ))
}

fn create_safe_directories(root: &Path, parent: &Path) -> std::io::Result<()> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| std::io::Error::other("parent escaped staging"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(std::io::Error::other(
                        "staging path contains a non-directory or symlink",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub fn safe_relative_path(path: &str) -> Result<PathBuf, &'static str> {
    if path.is_empty() {
        return Err("path is empty");
    }
    if path.contains('\\') {
        return Err("backslash separators are forbidden");
    }
    if path.contains('\0') {
        return Err("NUL bytes are forbidden");
    }
    if path.starts_with('/') || path.starts_with("//") {
        return Err("absolute paths are forbidden");
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err("drive-prefixed paths are forbidden");
    }
    let candidate = Path::new(path);
    let mut output = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(value) => output.push(value),
            Component::ParentDir => return Err("parent traversal is forbidden"),
            Component::CurDir => return Err("dot segments are forbidden"),
            Component::RootDir | Component::Prefix(_) => {
                return Err("absolute paths are forbidden");
            }
        }
    }
    if path.split('/').any(str::is_empty) {
        return Err("empty path segments are forbidden");
    }
    Ok(output)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn add_path_prefix_collisions<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    diagnostics: &mut Vec<Diagnostic>,
    code: &str,
) {
    let paths: BTreeSet<&str> = paths.into_iter().collect();
    for path in &paths {
        for (index, byte) in path.bytes().enumerate() {
            if byte == b'/' {
                let parent = &path[..index];
                if paths.contains(parent) {
                    add(
                        diagnostics,
                        code,
                        &format!("{parent} | {path}"),
                        "one file path is also the parent of another file path",
                    );
                }
            }
        }
    }
}

fn add(diagnostics: &mut Vec<Diagnostic>, code: &str, path: &str, message: &str) {
    diagnostics.push(Diagnostic {
        code: code.to_owned(),
        path: path.to_owned(),
        message: message.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::{Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{FileDeclaration, Manifest, add_path_prefix_collisions, safe_relative_path, stage};
    use crate::archive::{Archive, Writer};
    use crate::sha256::{Sha256, hex};

    #[test]
    fn accepts_safe_program_path() {
        assert!(safe_relative_path("programs/job.nc").is_ok());
    }

    #[test]
    fn rejects_escape_forms() {
        for path in [
            "/etc/passwd",
            "../escape.nc",
            "a/../../escape.nc",
            "C:/escape.nc",
            r"..\escape.nc",
            "a//b.nc",
            "./job.nc",
        ] {
            assert!(safe_relative_path(path).is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn reports_file_and_parent_path_collisions() {
        let mut diagnostics = Vec::new();
        add_path_prefix_collisions(
            ["programs/job.nc", "programs/job.nc/child.nc"],
            &mut diagnostics,
            "path_type_collision",
        );
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "path_type_collision");
    }

    #[test]
    fn late_copy_failure_leaves_no_ready_or_pending_directory() {
        let root = temporary_directory("late-copy");
        let bundle = root.join("bundle.zip");
        let staging = root.join("staging");
        let first = b"%\nM30\n";
        let second = b"%\nM31\n";
        let manifest = Manifest {
            version: 1,
            files: vec![
                declaration("programs/job.nc", first),
                declaration("programs/job.nc/child.nc", second),
            ],
            entry_program: "programs/job.nc".to_owned(),
            required_tool_ids: Vec::new(),
        };
        let manifest_sha256 = write_bundle(
            &bundle,
            &manifest,
            &[
                ("programs/job.nc", first.as_slice()),
                ("programs/job.nc/child.nc", second.as_slice()),
            ],
        );
        let mut archive = Archive::open(&bundle).expect("open generated bundle");

        let error = stage(&mut archive, &manifest, &manifest_sha256, &staging)
            .expect_err("copy conflict must fail");
        assert!(!error.to_string().is_empty());
        assert!(!staging.join("ready").exists());
        assert_eq!(
            fs::read_dir(&staging).expect("read staging root").count(),
            0
        );
        fs::remove_dir_all(&root).expect("remove test directory");
    }

    #[test]
    fn copied_content_is_rechecked_before_ready_is_named() {
        let root = temporary_directory("copy-recheck");
        let bundle = root.join("bundle.zip");
        let staging = root.join("staging");
        let original = b"%\nM30\n";
        let replacement = b"%\nM31\n";
        let manifest = Manifest {
            version: 1,
            files: vec![declaration("programs/job.nc", original)],
            entry_program: "programs/job.nc".to_owned(),
            required_tool_ids: Vec::new(),
        };
        let manifest_sha256 = write_bundle(
            &bundle,
            &manifest,
            &[("programs/job.nc", original.as_slice())],
        );
        let mut archive = Archive::open(&bundle).expect("open generated bundle");
        let data_offset = archive
            .entries
            .iter()
            .find(|entry| entry.name == "programs/job.nc")
            .expect("program entry")
            .data_offset;
        let mut source = OpenOptions::new()
            .write(true)
            .open(&bundle)
            .expect("open bundle for ordinary update");
        source
            .seek(SeekFrom::Start(data_offset))
            .expect("seek to program data");
        source
            .write_all(replacement)
            .expect("replace program bytes");
        source.flush().expect("flush replacement");

        let error = stage(&mut archive, &manifest, &manifest_sha256, &staging)
            .expect_err("changed copy must fail");
        assert!(error.to_string().contains("copied content changed"));
        assert!(!staging.join("ready").exists());
        assert_eq!(
            fs::read_dir(&staging).expect("read staging root").count(),
            0
        );
        fs::remove_dir_all(&root).expect("remove test directory");
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cnc-job-bundle-preflight-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    fn declaration(path: &str, data: &[u8]) -> FileDeclaration {
        FileDeclaration {
            path: path.to_owned(),
            bytes: data.len() as u64,
            sha256: digest(data),
        }
    }

    fn digest(data: &[u8]) -> String {
        let mut sha = Sha256::new();
        sha.update(data);
        hex(&sha.finish())
    }

    fn write_bundle(path: &Path, manifest: &Manifest, members: &[(&str, &[u8])]) -> String {
        let manifest_bytes = serde_json::to_vec(manifest).expect("serialize manifest");
        let manifest_sha256 = digest(&manifest_bytes);
        let mut writer = Writer::create(path).expect("create bundle");
        writer
            .member("manifest.json", &manifest_bytes)
            .expect("write manifest");
        for (name, data) in members {
            writer.member(name, data).expect("write member");
        }
        writer.finish().expect("finish bundle");
        manifest_sha256
    }
}
