use bog_cloud::backup::{ArchiveFile, Manifest, validate_manifest};
#[test]
fn archives_reject_traversal_duplicates_and_incompatible_templates() {
    let mut m = Manifest {
        definition: None,
        format_version: 1,
        template_id: "records-v1".into(),
        template_version: "records-v1".into(),
        source_bog_id: uuid::Uuid::new_v4().to_string(),
        build_commit: "test".into(),
        created_at: 0,
        logical: bog_cloud::backup::LogicalSnapshot {
            count: 0,
            records_sha256: "00".repeat(32),
        },
        files: vec![ArchiveFile {
            path: "data/file".into(),
            size: 3,
            sha256: "00".repeat(32),
        }],
    };
    assert!(validate_manifest(&m).is_ok());
    for path in [
        "../escape",
        "/absolute",
        "data/../escape",
        "data//file",
        "data/./file",
        "secrets",
    ] {
        m.files[0].path = path.into();
        assert!(validate_manifest(&m).is_err(), "{path}");
    }
    m.files[0].path = "data/file".into();
    m.files.push(m.files[0].clone());
    assert!(validate_manifest(&m).is_err());
    m.files.pop();
    m.template_version = "records-v2".into();
    assert!(validate_manifest(&m).is_err());
}
#[test]
fn configured_archives_require_supported_definition_identity() {
    let definition = bog_definition::Definition::records_v1();
    let mut manifest = Manifest {
        format_version: 2,
        definition: Some(bog_cloud::definitions::ActiveDefinition {
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
        logical: bog_cloud::backup::LogicalSnapshot {
            count: 0,
            records_sha256: "00".repeat(32),
        },
        files: vec![ArchiveFile {
            path: "records.json".into(),
            size: 2,
            sha256: "00".repeat(32),
        }],
    };
    assert!(validate_manifest(&manifest).is_ok());
    manifest.definition.as_mut().unwrap().digest = "ff".repeat(32);
    assert!(validate_manifest(&manifest).is_err());
    let active = manifest.definition.as_mut().unwrap();
    active.digest = active.definition.digest().unwrap();
    active.definition.version = 99;
    assert!(validate_manifest(&manifest).is_err());
}
