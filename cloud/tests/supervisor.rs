use bog_cloud::{Registry, config::Config, supervisor::Supervisor};
use std::sync::Arc;
#[test]
fn manager_root_is_exclusive_and_symlink_root_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("service");
    std::fs::create_dir(&root).unwrap();
    let db = Arc::new(Registry::open(&root.join("registry.sqlite")).unwrap());
    let config = Config::new(root.clone(), std::env::current_exe().unwrap());
    let first = Supervisor::open(config.clone(), db.clone()).unwrap();
    assert!(Supervisor::open(config.clone(), db.clone()).is_err());
    drop(first);
    assert!(Supervisor::open(config, db.clone()).is_ok());
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    assert!(Supervisor::open(Config::new(link, std::env::current_exe().unwrap()), db).is_err());
}
