use bog_cloud::{CloudService, config::Config};
#[tokio::test]
async fn admin_socket_requires_owner_and_refuses_unrelated_path() {
    let dir = tempfile::Builder::new()
        .prefix("bc-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = dir.path().join("r");
    let token = "owner-secret-thirty-two-bytes-for-admin";
    let svc = CloudService::open(
        Config::new(root.clone(), std::env::current_exe().unwrap()),
        token,
    )
    .unwrap();
    let path = root.join("admin.sock");
    std::fs::write(&path, b"unrelated").unwrap();
    assert!(
        bog_cloud::admin::bind(svc.clone(), path.clone())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"unrelated");
    std::fs::remove_file(&path).unwrap();
    let admin = bog_cloud::admin::bind(svc.clone(), path.clone())
        .await
        .unwrap();
    let client = reqwest::Client::builder()
        .unix_socket(path.clone())
        .no_proxy()
        .build()
        .unwrap();
    let url = format!("http://admin/backup/{}", uuid::Uuid::new_v4());
    assert_eq!(client.post(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    admin.shutdown().await.unwrap();
    assert!(!path.exists());
}
