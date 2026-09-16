use crate::{
    BogId, CloudError, DesiredState, ObservedState, Registry, auth::random_secret, config::Config,
    worker_client::WorkerClient,
};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::{
    process::{Child, Command},
    sync::{Mutex as AsyncMutex, OwnedRwLockReadGuard, OwnedSemaphorePermit, RwLock, Semaphore},
};

struct Running {
    client: WorkerClient,
    child: Option<Child>,
    _slot: OwnedSemaphorePermit,
}
pub struct WorkerLease {
    pub client: WorkerClient,
    _guard: OwnedRwLockReadGuard<()>,
}
pub struct Supervisor {
    pub(crate) config: Config,
    pub(crate) registry: Arc<Registry>,
    _lock: File,
    running: AsyncMutex<HashMap<BogId, Running>>,
    gates: Mutex<HashMap<BogId, Arc<RwLock<()>>>>,
    starts: Mutex<HashMap<BogId, Arc<AsyncMutex<()>>>>,
    start_slots: Arc<Semaphore>,
    active_slots: Arc<Semaphore>,
}
fn unavailable(message: &str) -> CloudError {
    CloudError::new("unavailable", message)
}
pub(crate) fn private_directory(path: &Path) -> Result<(), CloudError> {
    if let Ok(meta) = std::fs::symlink_metadata(path)
        && (meta.file_type().is_symlink() || !meta.is_dir())
    {
        return Err(unavailable("service directory must be a real directory"));
    }
    std::fs::create_dir_all(path).map_err(|_| unavailable("cannot create service directory"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| unavailable("cannot protect service directory"))
}
impl Supervisor {
    pub fn open(mut config: Config, registry: Arc<Registry>) -> Result<Self, CloudError> {
        if !config.root.is_absolute()
            || !config.worker_binary.is_absolute()
            || config.max_active == 0
            || config.max_starts == 0
        {
            return Err(CloudError::new(
                "invalid_config",
                "absolute paths and positive limits required",
            ));
        }
        private_directory(&config.root)?;
        config.root = std::fs::canonicalize(&config.root)
            .map_err(|_| unavailable("cannot resolve service directory"))?;
        private_directory(&config.root.join("instances"))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(config.root.join("manager.lock"))
            .map_err(|_| unavailable("cannot open manager lock"))?;
        lock.try_lock()
            .map_err(|_| unavailable("another manager owns this service root"))?;
        Ok(Self {
            start_slots: Arc::new(Semaphore::new(config.max_starts)),
            active_slots: Arc::new(Semaphore::new(config.max_active)),
            starts: Mutex::new(HashMap::new()),
            config,
            registry,
            _lock: lock,
            running: AsyncMutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
        })
    }
    pub fn max_active(&self) -> usize {
        self.config.max_active
    }
    pub fn instance_dir(&self, id: BogId) -> PathBuf {
        self.config.root.join("instances").join(id.to_string())
    }
    pub(crate) fn gate(&self, id: BogId) -> Result<Arc<RwLock<()>>, CloudError> {
        Ok(self
            .gates
            .lock()
            .map_err(|_| unavailable("instance lock failed"))?
            .entry(id)
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone())
    }
    pub async fn lease(&self, id: BogId) -> Result<WorkerLease, CloudError> {
        let guard = self.gate(id)?.read_owned().await;
        let bog = self.registry.get(id)?;
        if bog.desired_state != DesiredState::Running
            || matches!(
                bog.status,
                ObservedState::Maintenance | ObservedState::Restoring
            )
        {
            return Err(unavailable("database is not available"));
        }
        let client = self.start_locked(id).await?;
        Ok(WorkerLease {
            client,
            _guard: guard,
        })
    }
    pub async fn ensure_running(&self, id: BogId) -> Result<WorkerClient, CloudError> {
        let _guard = self.gate(id)?.read_owned().await;
        self.start_locked(id).await
    }
    pub(crate) async fn start_locked(&self, id: BogId) -> Result<WorkerClient, CloudError> {
        let gate = self
            .starts
            .lock()
            .map_err(|_| unavailable("startup lock failed"))?
            .entry(id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        let _start_guard = gate.lock().await;
        let snapshot = {
            let mut running = self.running.lock().await;
            if let Some(worker) = running.get_mut(&id) {
                let exited = match worker.child.as_mut() {
                    Some(child) => child
                        .try_wait()
                        .map_err(|_| unavailable("cannot inspect worker"))?
                        .is_some(),
                    None => false,
                };
                if exited {
                    running.remove(&id);
                    None
                } else {
                    Some((worker.client.clone(), worker.child.is_none()))
                }
            } else {
                None
            }
        };
        if let Some((client, adopted)) = snapshot {
            if !adopted || self.verify_worker(id, &client).await.is_ok() {
                return Ok(client);
            }
            self.running.lock().await.remove(&id);
        }
        let bog = self.registry.get(id)?;
        if bog.desired_state != DesiredState::Running
            || matches!(
                bog.status,
                ObservedState::Maintenance | ObservedState::Restoring
            )
        {
            return Err(unavailable("database is not available"));
        }
        let slot = self
            .active_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| CloudError::new("capacity", "active database limit reached"))?;
        let _start_slot = self
            .start_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| unavailable("supervisor is stopping"))?;
        let dir = self.instance_dir(id);
        private_directory(&dir)?;
        let socket = dir.join("worker.sock");
        if socket.as_os_str().len() > 100 {
            return Err(CloudError::new(
                "invalid_config",
                "service root is too long for Unix sockets",
            ));
        }
        let client = WorkerClient::new(&socket)?;
        if socket.exists()
            && let Ok((200, identity)) = client
                .request(reqwest::Method::GET, "/_cloud/identity", None)
                .await
        {
            let nonce = self
                .registry
                .startup_nonce(id)?
                .ok_or_else(|| unavailable("worker identity is not registered"))?;
            if identity["instance_id"] != id.to_string()
                || identity["nonce"] != nonce
                || identity["template_version"] != "records-v1"
            {
                return Err(unavailable("worker identity mismatch"));
            }
            self.verify_worker(id, &client).await?;
            self.registry.set_status(id, ObservedState::Ready, None)?;
            self.running.lock().await.insert(
                id,
                Running {
                    client: client.clone(),
                    child: None,
                    _slot: slot,
                },
            );
            return Ok(client);
        }
        // Acquire this before recording a new generation, and pass the open
        // file description to the child. It survives a manager crash even if
        // the worker has not opened its store or bound its socket yet. A probe
        // timeout alone must never destroy the surviving worker's identity.
        let generation_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join("worker.lock"))
            .map_err(|_| unavailable("cannot open worker ownership lock"))?;
        generation_lock
            .try_lock()
            .map_err(|_| unavailable("existing worker is not yet responsive"))?;
        // Also protect adoption of a worker launched before the lifetime lock
        // was introduced (or directly by an administrator).
        match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join("data/lock"))
        {
            Ok(store_lock) => store_lock
                .try_lock()
                .map_err(|_| unavailable("existing worker still owns the store"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unavailable("cannot inspect store ownership")),
        }
        for attempt in 0..3 {
            let nonce = random_secret()?;
            self.registry.start_generation(id, &nonce)?;
            let mut command = Command::new(&self.config.worker_binary);
            command
                .args([
                    "--data-dir",
                    dir.join("data")
                        .to_str()
                        .ok_or_else(|| unavailable("invalid data path"))?,
                    "--socket",
                    socket
                        .to_str()
                        .ok_or_else(|| unavailable("invalid socket path"))?,
                    "--template-version",
                    "records-v1",
                ])
                .env_clear()
                .env("BOG_INSTANCE_ID", id.to_string())
                .env("BOG_STARTUP_NONCE", &nonce)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let lock_fd = generation_lock.as_raw_fd();
            // Only the child clears CLOEXEC; unrelated concurrently spawned
            // workers cannot inherit this instance's ownership lock. fcntl is
            // async-signal-safe and the file remains live throughout spawn.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(lock_fd, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let spawned = command.spawn();
            let mut child = match spawned {
                Ok(child) => child,
                Err(_) => {
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(100 * (attempt + 1)))
                            .await;
                    }
                    continue;
                }
            };
            let deadline = tokio::time::Instant::now() + self.config.readiness_timeout;
            while tokio::time::Instant::now() < deadline {
                if child
                    .try_wait()
                    .map_err(|_| unavailable("cannot inspect worker"))?
                    .is_some()
                {
                    break;
                }
                if let Ok(Ok((200, identity))) = tokio::time::timeout(
                    std::time::Duration::from_millis(300),
                    client.request(reqwest::Method::GET, "/_cloud/identity", None),
                )
                .await
                    && identity["instance_id"] == id.to_string()
                    && identity["nonce"] == nonce
                    && identity["template_version"] == "records-v1"
                {
                    if self.verify_worker(id, &client).await.is_err() {
                        break;
                    }
                    self.registry.set_status(id, ObservedState::Ready, None)?;
                    self.running.lock().await.insert(
                        id,
                        Running {
                            client: client.clone(),
                            child: Some(child),
                            _slot: slot,
                        },
                    );
                    return Ok(client);
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            child
                .kill()
                .await
                .map_err(|_| unavailable("worker exit could not be confirmed"))?;
            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(100 * (attempt + 1))).await;
            }
        }
        self.registry
            .set_status(id, ObservedState::Failed, Some("worker_start_failed"))?;
        Err(unavailable("database worker failed readiness checks"))
    }
    async fn verify_worker(&self, id: BogId, client: &WorkerClient) -> Result<(), CloudError> {
        let nonce = self.registry.startup_nonce(id)?;
        let (status, identity) = client
            .request(reqwest::Method::GET, "/_cloud/identity", None)
            .await?;
        if status != 200
            || identity["instance_id"] != id.to_string()
            || identity["nonce"].as_str() != nonce.as_deref()
            || identity["template_version"] != "records-v1"
        {
            return Err(unavailable("worker identity mismatch"));
        }
        let (status, schema) = client
            .request(reqwest::Method::GET, "/schema", None)
            .await?;
        let names: Vec<_> = schema["views"]
            .as_array()
            .map(|views| views.iter().filter_map(|v| v["name"].as_str()).collect())
            .unwrap_or_default();
        if status != 200 || schema["input"]["type"] != "object" || names != ["docs", "total"] {
            return Err(unavailable("worker schema mismatch"));
        }
        Ok(())
    }
    pub async fn stop(&self, id: BogId) -> Result<(), CloudError> {
        let _guard = self.gate(id)?.write_owned().await;
        self.stop_locked(id, true).await
    }
    pub(crate) async fn stop_locked(
        &self,
        id: BogId,
        change_desired: bool,
    ) -> Result<(), CloudError> {
        if change_desired {
            self.registry.set_desired(id, false)?;
        }
        let worker = self.running.lock().await.remove(&id);
        if let Some(mut worker) = worker {
            let response = worker
                .client
                .request(
                    reqwest::Method::POST,
                    "/_cloud/shutdown",
                    Some(serde_json::json!({"nonce":self.registry.startup_nonce(id)?})),
                )
                .await;
            if response.as_ref().is_ok_and(|(s, _)| *s != 200) {
                self.running.lock().await.insert(id, worker);
                return Err(unavailable("worker refused shutdown"));
            }
            if let Some(child) = worker.child.as_mut() {
                match tokio::time::timeout(std::time::Duration::from_secs(20), child.wait()).await {
                    Ok(Ok(status)) if status.success() => {}
                    Ok(Ok(_)) => {
                        self.registry.set_status(
                            id,
                            ObservedState::Failed,
                            Some("unclean_shutdown"),
                        )?;
                        return Err(unavailable("worker did not shut down cleanly"));
                    }
                    _ => {
                        self.running.lock().await.insert(id, worker);
                        return Err(unavailable("worker shutdown could not be confirmed"));
                    }
                }
            } else {
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
                while self.instance_dir(id).join("worker.sock").exists() {
                    if tokio::time::Instant::now() > deadline {
                        self.running.lock().await.insert(id, worker);
                        return Err(unavailable("adopted worker shutdown timed out"));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
        self.registry.set_status(id, ObservedState::Stopped, None)
    }
    pub async fn reconcile(&self) -> Result<Vec<(BogId, String)>, CloudError> {
        crate::backup::recover_staging(&self.config.root.join("backups"))?;
        let mut failures = Vec::new();
        for bog in self.registry.list()? {
            if bog.status == ObservedState::Maintenance {
                // An interrupted closed-store backup never changes source data.
                self.registry
                    .set_status(bog.id, ObservedState::Stopped, None)?;
            }
            if bog.status == ObservedState::Restoring {
                self.registry.set_status(
                    bog.id,
                    ObservedState::Failed,
                    Some("restore_interrupted"),
                )?;
                self.registry.set_desired(bog.id, false)?;
                continue;
            }
            if bog.desired_state == DesiredState::Running
                && let Err(e) = self.ensure_running(bog.id).await
            {
                failures.push((bog.id, e.code));
            }
        }
        Ok(failures)
    }
    /// Stop workers while retaining intent, so the next manager restores them.
    pub async fn shutdown(&self) -> Result<(), CloudError> {
        let ids: Vec<_> = self.running.lock().await.keys().copied().collect();
        for id in ids {
            let _guard = self.gate(id)?.write_owned().await;
            self.stop_locked(id, false).await?;
        }
        Ok(())
    }
    pub async fn stop_all(&self) -> Result<(), CloudError> {
        let ids: Vec<_> = self.running.lock().await.keys().copied().collect();
        for id in ids {
            self.stop(id).await?;
        }
        Ok(())
    }
}
