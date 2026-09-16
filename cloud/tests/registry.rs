use bog_cloud::Registry;
#[test]
fn creation_retry_survives_reopen_and_conflicting_payload_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let registry = Registry::open(&path).unwrap();
    let created = registry
        .create(" Notes ", "records-v1", "request-1")
        .unwrap();
    assert_eq!(created.name, "notes");
    drop(registry);
    let registry = Registry::open(&path).unwrap();
    let retried = registry.create("notes", "records-v1", "request-1").unwrap();
    assert_eq!(created.id, retried.id);
    assert_eq!(
        registry
            .create("different", "records-v1", "request-1")
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        registry
            .create("notes", "records-v1", "request-2")
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        registry
            .create("notes2", "unknown", "request-3")
            .unwrap_err()
            .code,
        "invalid_request"
    );
}

#[test]
fn concurrent_duplicate_creation_is_one_resource() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let registry = std::sync::Arc::new(Registry::open(&path).unwrap());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let registry = registry.clone();
            std::thread::spawn(move || {
                registry
                    .create("parallel", "records-v1", "same")
                    .unwrap()
                    .id
            })
        })
        .collect();
    let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(ids.iter().all(|id| *id == ids[0]));
}

#[test]
fn names_are_display_metadata_and_invalid_inputs_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::open(&dir.path().join("registry.sqlite")).unwrap();
    for name in ["", "  ", "../escape", "control\nname"] {
        assert_eq!(
            registry
                .create(name, "records-v1", "request")
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
    assert_eq!(
        registry.create("ok", "records-v1", "").unwrap_err().code,
        "invalid_request"
    );
}

#[test]
fn template_version_is_persisted_and_separate_connections_converge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let registry = Registry::open(&path).unwrap();
    let a = registry
        .create("versioned", "records-v1", "version")
        .unwrap();
    let encoded = serde_json::to_value(&a).unwrap();
    assert_eq!(encoded["template_version"], "records-v1");
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                Registry::open(&path)
                    .unwrap()
                    .create("cross connection", "records-v1", "multi")
                    .unwrap()
                    .id
            })
        })
        .collect();
    let ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(ids.iter().all(|id| *id == ids[0]));
}
