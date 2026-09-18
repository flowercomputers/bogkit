//! These tests kill the real manager, leaving its worker alive across restart.
use bog_cloud::{BogId, Registry, worker_client::WorkerClient};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const OWNER: &str = "owner-manager-restart-test-thirty-two-bytes";

struct Processes {
    manager: Option<Child>,
    worker: Option<i32>,
    root: PathBuf,
    binary: PathBuf,
    address: String,
    client: Client,
}
impl Processes {
    fn new(root: PathBuf, binary: PathBuf) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        Self {
            manager: None,
            worker: None,
            root,
            binary,
            address,
            client: Client::builder()
                .timeout(Duration::from_secs(40))
                .build()
                .unwrap(),
        }
    }
    fn start(&mut self) {
        self.manager = Some(
            Command::new(env!("CARGO_BIN_EXE_bog-cloud"))
                .env("BOG_ALLOW_LEGACY_PUBLIC_OPERATOR", "true")
                .env("BOG_CLOUD_COMPOSABLE", "true")
                .env("BOG_CLOUD_ROOT", &self.root)
                .env("BOG_WORKER_BINARY", &self.binary)
                .env("BOG_CLOUD_OWNER_TOKEN", OWNER)
                .env("BOG_CLOUD_BIND", &self.address)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
    }
    fn crash(&mut self) {
        let mut manager = self.manager.take().unwrap();
        manager.kill().unwrap();
        manager.wait().unwrap();
    }
    async fn healthy(&self) {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if self
                .client
                .get(format!("http://{}/healthz", self.address))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return;
            }
            assert!(Instant::now() < deadline, "manager never became healthy");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .client
            .request(method, format!("http://{}{path}", self.address))
            .bearer_auth(OWNER)
            .header("Idempotency-Key", "restart-test");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap())
    }
    async fn identity(&mut self, id: BogId) -> Value {
        let socket = self
            .root
            .join("instances")
            .join(id.to_string())
            .join("worker.sock");
        let (_, identity) = WorkerClient::new(&socket)
            .unwrap()
            .request(Method::GET, "/_cloud/identity", None)
            .await
            .unwrap();
        self.worker = Some(identity["pid"].as_i64().unwrap() as i32);
        identity
    }
    fn signal_worker(&self, signal: i32) {
        assert_eq!(unsafe { libc::kill(self.worker.unwrap(), signal) }, 0);
    }
}
impl Drop for Processes {
    fn drop(&mut self) {
        // Stop the manager first so cleanup cannot accidentally launch another worker.
        if let Some(mut manager) = self.manager.take() {
            let _ = manager.kill();
            let _ = manager.wait();
        }
        if let Some(pid) = self.worker {
            unsafe {
                libc::kill(pid, libc::SIGCONT);
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}
fn worker() -> PathBuf {
    std::env::var_os("BOG_TEST_WORKER")
        .map(Into::into)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/bog-records-worker")
        })
}

#[tokio::test]
async fn paused_worker_recovers_after_manager_crash_without_changing_identity_or_data() {
    let temp = tempfile::Builder::new()
        .prefix("bcr-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut processes = Processes::new(temp.path().join("r"), worker());
    processes.start();
    processes.healthy().await;
    let (status, bog) = processes
        .request(
            Method::POST,
            "/v1/bogs",
            Some(json!({"name":"restart","template":"records-v1"})),
        )
        .await;
    assert_eq!(status, 202);
    let id: BogId = serde_json::from_value(bog["id"].clone()).unwrap();
    let path = format!("/v1/bogs/{id}/docs/saved");
    let data = json!({"nested":{"survives":true}, "revision":7});
    assert!(
        processes
            .request(Method::PUT, &path, Some(data.clone()))
            .await
            .0
            < 300
    );
    let identity = processes.identity(id).await;
    let registry = Registry::open(&processes.root.join("registry.sqlite")).unwrap();
    let generation = registry.get(id).unwrap().generation;
    processes.signal_worker(libc::SIGSTOP);
    processes.crash();
    processes.start();
    // Health is served only after reconciliation's full identity timeout.
    processes.healthy().await;
    assert_eq!(registry.get(id).unwrap().generation, generation);
    assert_eq!(
        registry.startup_nonce(id).unwrap().as_deref(),
        identity["nonce"].as_str()
    );
    processes.signal_worker(libc::SIGCONT);
    let (status, record) = processes.request(Method::GET, &path, None).await;
    assert_eq!(status, 200, "{record}");
    assert_eq!(record["data"], data);
    assert_eq!(processes.identity(id).await, identity);
    assert_eq!(registry.get(id).unwrap().generation, generation);
}

#[tokio::test]
async fn pre_socket_worker_keeps_ownership_across_manager_crash() {
    let temp = tempfile::Builder::new()
        .prefix("bcp-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = temp.path().join("r");
    std::fs::create_dir(&root).unwrap();
    let registry = Registry::open(&root.join("registry.sqlite")).unwrap();
    let bog = registry.create("prebind", "records-v1", "prebind").unwrap();
    let wrapper = temp.path().join("worker");
    let pid_file = temp.path().join("pid");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho $$ >> '{}'\nkill -STOP $$\nexec '{}' \"$@\"\n",
            pid_file.display(),
            worker().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut processes = Processes::new(root, wrapper);
    processes.start();
    processes.healthy().await;
    // Retained databases now wake on demand, so initiate a request before
    // killing the manager while its worker is still before socket startup.
    let request = processes
        .client
        .get(format!(
            "http://{}/v1/bogs/{}/docs/saved",
            processes.address, bog.id
        ))
        .bearer_auth(OWNER);
    let pending = tokio::spawn(async move { request.send().await });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists()
        || std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .is_empty()
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    processes.worker = Some(
        std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    );
    let nonce = registry.startup_nonce(bog.id).unwrap();
    let generation = registry.get(bog.id).unwrap().generation;
    assert!(
        !processes
            .root
            .join("instances")
            .join(bog.id.to_string())
            .join("worker.sock")
            .exists()
    );
    processes.crash();
    pending.abort();
    processes.start();
    processes.healthy().await;
    assert_eq!(registry.startup_nonce(bog.id).unwrap(), nonce);
    assert_eq!(registry.get(bog.id).unwrap().generation, generation);
    assert_eq!(
        std::fs::read_to_string(&pid_file).unwrap().lines().count(),
        1
    );
    processes.signal_worker(libc::SIGCONT);
    let path = format!("/v1/bogs/{}/docs/saved", bog.id);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, _) = processes
            .request(Method::PUT, &path, Some(json!({"survives":true})))
            .await;
        if status < 300 {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        processes.request(Method::GET, &path, None).await.1["data"],
        json!({"survives":true})
    );
    let identity = processes.identity(bog.id).await;
    assert_eq!(identity["nonce"].as_str(), nonce.as_deref());
    assert_eq!(registry.get(bog.id).unwrap().generation, generation);
    assert_eq!(
        std::fs::read_to_string(&pid_file).unwrap().lines().count(),
        1
    );
}

#[tokio::test]
async fn interrupted_definition_freeze_resumes_surviving_workers_before_writes() {
    for configured in [false, true] {
        let temp = tempfile::Builder::new()
            .prefix("bcf-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut processes = Processes::new(temp.path().join("r"), worker());
        processes.start();
        processes.healthy().await;
        // The test manager is explicitly opted into structured definitions below.
        let body = if configured {
            json!({"name":"frozen","definition":bog_definition::Definition::records_v1()})
        } else {
            json!({"name":"frozen","template":"records-v1"})
        };
        let (status, bog) = processes
            .request(Method::POST, "/v1/bogs", Some(body))
            .await;
        assert_eq!(status, 202, "{bog}");
        let id: BogId = serde_json::from_value(bog["id"].clone()).unwrap();
        let path = format!("/v1/bogs/{id}/docs/saved");
        assert_eq!(
            processes
                .request(Method::PUT, &path, Some(json!({"value":1})))
                .await
                .0,
            200
        );
        let identity = processes.identity(id).await;
        let db = rusqlite::Connection::open(processes.root.join("registry.sqlite")).unwrap();
        db.execute("INSERT INTO definition_jobs(id,bog_id,status,payload,created_at) VALUES ('crash-build',?1,'building','{}',0)",[id.to_string()]).unwrap();
        drop(db);
        let socket = processes
            .root
            .join("instances")
            .join(id.to_string())
            .join("worker.sock");
        let client = WorkerClient::new(&socket).unwrap();
        assert_eq!(
            client
                .request(Method::POST, "/_cloud/pause_writes", None)
                .await
                .unwrap()
                .0,
            200
        );
        assert_eq!(
            client
                .request(Method::PUT, "/docs/late", Some(json!({"value":2})))
                .await
                .unwrap()
                .0,
            503
        );
        assert_eq!(processes.request(Method::GET, &path, None).await.0, 200);
        processes.crash();
        processes.start();
        processes.healthy().await;
        let (status, body) = processes
            .request(Method::PUT, &path, Some(json!({"value":3})))
            .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            processes.identity(id).await,
            identity,
            "surviving worker should be resumed, not replaced"
        );
        assert_eq!(
            processes.request(Method::GET, &path, None).await.1["data"]["value"],
            3
        );
        let db = rusqlite::Connection::open(processes.root.join("registry.sqlite")).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT status FROM definition_jobs WHERE id='crash-build'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "failed"
        );
    }
}
