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
    generation: i64,
    last_use: Arc<Mutex<std::time::Instant>>,
    client: WorkerClient,
    child: Option<Child>,
    _slot: OwnedSemaphorePermit,
}
#[derive(Clone)]
struct WorkerSnapshot {
    client: WorkerClient,
    generation: i64,
    last_use: Arc<Mutex<std::time::Instant>>,
}
impl Running {
    fn snapshot(&self) -> WorkerSnapshot {
        WorkerSnapshot {
            client: self.client.clone(),
            generation: self.generation,
            last_use: self.last_use.clone(),
        }
    }
}
pub struct WorkerLease {
    pub client: WorkerClient,
    /// Generation captured with this worker client under startup serialization.
    pub generation: i64,
    last_use: Arc<Mutex<std::time::Instant>>,
    _guard: OwnedRwLockReadGuard<()>,
}
impl Drop for WorkerLease {
    fn drop(&mut self) {
        if let Ok(mut last_use) = self.last_use.lock() {
            *last_use = std::time::Instant::now();
        }
    }
}
pub struct Supervisor {
    pub(crate) config: Config,
    observability: Option<Arc<crate::observability::Observability>>,
    pub(crate) registry: Arc<Registry>,
    _lock: File,
    running: AsyncMutex<HashMap<BogId, Running>>,
    gates: Mutex<HashMap<BogId, Arc<RwLock<()>>>>,
    starts: Mutex<HashMap<BogId, Arc<AsyncMutex<()>>>>,
    start_slots: Arc<Semaphore>,
    active_slots: Arc<Semaphore>,
    admission: AsyncMutex<()>,
    build_slots: Arc<Semaphore>,
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
        config
            .composable_limits
            .validate()
            .map_err(|_| CloudError::new("invalid_config", "invalid composable limits"))?;
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
        registry.recover_definition_jobs()?;
        crate::definitions::recover_candidates(&config.root.join("instances"), &registry)?;
        Ok(Self {
            observability: None,
            start_slots: Arc::new(Semaphore::new(config.max_starts)),
            active_slots: Arc::new(Semaphore::new(config.max_active)),
            admission: AsyncMutex::new(()),
            build_slots: Arc::new(Semaphore::new(1)),
            starts: Mutex::new(HashMap::new()),
            config,
            registry,
            _lock: lock,
            running: AsyncMutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
        })
    }
    pub fn with_observability(mut self, obs: Arc<crate::observability::Observability>) -> Self {
        self.observability = Some(obs);
        self
    }
    fn event(&self, id: BogId, kind: &str, reason: Option<&str>) {
        if let Some(obs) = &self.observability {
            obs.event(id, kind, None, reason);
        }
    }
    /// Read cached manager state only; never obtains a lease or touches idle time.
    pub async fn diagnostic_snapshot(&self, id: BogId) -> Result<serde_json::Value, CloudError> {
        let bog = self.registry.get(id)?;
        let running = self.running.lock().await;
        let state = if running.contains_key(&id) {
            "running"
        } else {
            match bog.status {
                ObservedState::Creating => "starting",
                ObservedState::Failed => "failed",
                ObservedState::Restoring => "restoring",
                ObservedState::Maintenance => "maintenance",
                _ => "sleeping",
            }
        };
        let mut value = serde_json::json!({"state":state,"generation":bog.generation,"desired_state":bog.desired_state,"failure_code":bog.failure_code,"state_source":"manager_cached"});
        if let Some(obs) = &self.observability
            && let Some(fields) = obs.worker_metadata(id).as_object()
        {
            for (key, v) in fields {
                value[key] = v.clone();
            }
        }
        Ok(value)
    }
    pub(crate) fn reserve_build(&self) -> Result<OwnedSemaphorePermit, CloudError> {
        self.build_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| CloudError::new("capacity", "one definition build may run on this host"))
    }
    pub(crate) async fn reserve_candidate(&self) -> Result<OwnedSemaphorePermit, CloudError> {
        let slot = self.reserve_resident_slot(None).await?;
        self.check_physical_capacity(None).await?;
        Ok(slot)
    }
    /// Called with the source's exclusive gate held and source already resident.
    pub(crate) async fn reserve_build_candidate(
        &self,
        source: BogId,
    ) -> Result<OwnedSemaphorePermit, CloudError> {
        let slot = self.reserve_resident_slot(Some(source)).await?;
        self.check_physical_capacity(Some(source)).await?;
        Ok(slot)
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
        let snapshot = self.start_snapshot_locked(id).await?;
        Ok(WorkerLease {
            last_use: snapshot.last_use,
            client: snapshot.client,
            generation: snapshot.generation,
            _guard: guard,
        })
    }
    pub async fn ensure_running(&self, id: BogId) -> Result<WorkerClient, CloudError> {
        let _guard = self.gate(id)?.read_owned().await;
        self.start_locked(id).await
    }
    pub(crate) async fn start_locked(&self, id: BogId) -> Result<WorkerClient, CloudError> {
        Ok(self.start_snapshot_locked(id).await?.client)
    }
    async fn start_snapshot_locked(&self, id: BogId) -> Result<WorkerSnapshot, CloudError> {
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
                    self.event(id, "worker_exited", Some("process_exit"));
                    running.remove(&id);
                    None
                } else {
                    if let Ok(mut last_use) = worker.last_use.lock() {
                        *last_use = std::time::Instant::now();
                    }
                    Some((worker.snapshot(), worker.child.is_none()))
                }
            } else {
                None
            }
        };
        if let Some((snapshot, adopted)) = snapshot {
            if !adopted || self.verify_worker(id, &snapshot.client).await.is_ok() {
                return Ok(snapshot);
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
        let slot = self.reserve_resident_slot(Some(id)).await?;
        self.check_physical_capacity(Some(id)).await?;
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
            let running = Running {
                generation: bog.generation,
                last_use: Arc::new(Mutex::new(std::time::Instant::now())),
                client,
                child: None,
                _slot: slot,
            };
            let snapshot = running.snapshot();
            self.running.lock().await.insert(id, running);
            return Ok(snapshot);
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
            .open(dir.join(self.registry.storage_dir(id)?).join("lock"))
        {
            Ok(store_lock) => store_lock
                .try_lock()
                .map_err(|_| unavailable("existing worker still owns the store"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unavailable("cannot inspect store ownership")),
        }
        for attempt in 0..3 {
            let nonce = random_secret()?;
            let generation = self.registry.start_generation(id, &nonce)?;
            self.event(
                id,
                "worker_starting",
                Some(if attempt == 0 {
                    "access"
                } else {
                    "readiness_retry"
                }),
            );
            let active = self.registry.definition(id)?;
            let definition_file = dir.join(format!("definition-{}.json", active.revision));
            if active.configured {
                crate::definitions::write_json(
                    &definition_file,
                    &serde_json::to_value(&active.definition)
                        .map_err(crate::definitions::invalid)?,
                )?;
            }
            let mut command = Command::new(&self.config.worker_binary);
            command
                .args([
                    "--data-dir",
                    dir.join(&active.storage_dir)
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
                .env(
                    "BOG_COMPOSABLE_LIMITS",
                    serde_json::to_string(&self.config.composable_limits)
                        .map_err(crate::definitions::invalid)?,
                )
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            if active.configured {
                command
                    .arg("--definition-file")
                    .arg(&definition_file)
                    .arg("--definition-revision")
                    .arg(active.revision.to_string());
            }
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
                    self.event(id, "worker_running", Some("readiness_passed"));
                    let running = Running {
                        generation,
                        last_use: Arc::new(Mutex::new(std::time::Instant::now())),
                        client,
                        child: Some(child),
                        _slot: slot,
                    };
                    let snapshot = running.snapshot();
                    self.running.lock().await.insert(id, running);
                    return Ok(snapshot);
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
        self.event(id, "worker_failed", Some("worker_start_failed"));
        Err(unavailable("database worker failed readiness checks"))
    }
    async fn check_physical_capacity(&self, id: Option<BogId>) -> Result<(), CloudError> {
        // Surviving but unresponsive workers still consume physical capacity.
        // Their lifetime/store locks are authoritative even before a socket
        // exists; never launch replacement capacity beside those processes.
        let running = self.running.lock().await;
        let mut untracked = 0;
        for entry in std::fs::read_dir(self.config.root.join("instances"))
            .map_err(|_| unavailable("cannot inspect resident worker capacity"))?
        {
            let entry =
                entry.map_err(|_| unavailable("cannot inspect resident worker capacity"))?;
            let Ok(uuid) = uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let other = BogId(uuid);
            if Some(other) == id || running.contains_key(&other) {
                continue;
            }
            for path in [
                entry.path().join("worker.lock"),
                entry.path().join("data/lock"),
            ] {
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)
                {
                    Ok(lock) if lock.try_lock().is_err() => {
                        untracked += 1;
                        break;
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(unavailable("cannot inspect resident worker ownership")),
                }
            }
        }
        drop(running);
        if self.config.max_active - self.active_slots.available_permits() + untracked
            > self.config.max_active
        {
            return Err(CloudError::new(
                "capacity",
                "surviving workers occupy resident slots; retry when they recover or stop an unused database",
            ));
        }
        Ok(())
    }

    /// Warm workers are a cache, not reservations. Under pressure close the
    /// least recently used worker whose exclusive gate is immediately available.
    /// Never wait for a victim's gate: callers already hold their target's gate.
    async fn reserve_resident_slot(
        &self,
        target: Option<BogId>,
    ) -> Result<OwnedSemaphorePermit, CloudError> {
        let _admission = self.admission.lock().await;
        if let Ok(slot) = self.active_slots.clone().try_acquire_owned() {
            return Ok(slot);
        }
        let mut candidates: Vec<_> = self
            .running
            .lock()
            .await
            .iter()
            .filter(|(id, _)| Some(**id) != target)
            .filter_map(|(id, worker)| worker.last_use.lock().ok().map(|last| (*id, *last)))
            .collect();
        candidates.sort_by_key(|(_, last)| *last);
        for (id, observed_last_use) in candidates {
            let Ok(_guard) = self.gate(id)?.try_write_owned() else {
                continue;
            };
            // The journal protects the source even between export and activation,
            // when no operation lease is held. Check while holding its write gate.
            if self.registry.building(id)? {
                continue;
            }
            // A newer request may have completed since the candidate snapshot.
            let unchanged = self.running.lock().await.get(&id).is_some_and(|worker| {
                worker
                    .last_use
                    .lock()
                    .is_ok_and(|last| *last == observed_last_use)
            });
            if !unchanged {
                continue;
            }
            self.stop_locked(id, false).await?;
            self.event(id, "worker_sleeping", Some("capacity_pressure"));
            if let Ok(slot) = self.active_slots.clone().try_acquire_owned() {
                return Ok(slot);
            }
        }
        Err(CloudError::new(
            "capacity",
            "all resident database slots are busy with active operations or startup; retry the same request shortly",
        ))
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
        let active = self.registry.worker_definition(id)?;
        if !self.registry.building(id)? {
            let (status, _) = client
                .request(reqwest::Method::POST, "/_cloud/resume_writes", None)
                .await?;
            // A pre-composition legacy worker could never have been frozen.
            if status != 200 && (status != 404 || active.configured) {
                return Err(unavailable("worker write recovery pending"));
            }
        }
        if active.configured {
            if identity["definition_digest"] != active.digest
                || identity["definition_revision"] != active.revision
            {
                return Err(unavailable("worker definition identity mismatch"));
            }
            return Ok(());
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
        if self.registry.building(id)? {
            return Err(CloudError::new(
                "conflict",
                "definition build is in progress",
            ));
        }
        self.stop_locked(id, true).await?;
        self.event(id, "worker_stopped", Some("explicit_stop"));
        Ok(())
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
        } else {
            self.stop_survivor_locked(id).await?;
        }
        self.registry.set_status(id, ObservedState::Stopped, None)
    }
    fn survivor_owns_store(&self, id: BogId) -> Result<bool, CloudError> {
        let dir = self.instance_dir(id);
        for path in [
            dir.join("worker.lock"),
            dir.join(self.registry.storage_dir(id)?).join("lock"),
        ] {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
            {
                Ok(lock) if lock.try_lock().is_err() => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(unavailable("cannot inspect surviving worker ownership")),
            }
        }
        Ok(false)
    }
    async fn stop_survivor_locked(&self, id: BogId) -> Result<(), CloudError> {
        if !self.survivor_owns_store(id)? {
            return Ok(());
        }
        let client = WorkerClient::new(&self.instance_dir(id).join("worker.sock"))?;
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.verify_worker(id, &client),
        )
        .await
        .map_err(|_| unavailable("surviving worker is not responsive; retry stop"))??;
        let (status, _) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.request(
                reqwest::Method::POST,
                "/_cloud/shutdown",
                Some(serde_json::json!({"nonce": self.registry.startup_nonce(id)?})),
            ),
        )
        .await
        .map_err(|_| unavailable("surviving worker shutdown pending; retry stop"))??;
        if status != 200 {
            return Err(unavailable("surviving worker refused shutdown"));
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while self.survivor_owns_store(id)? {
            if tokio::time::Instant::now() >= deadline {
                return Err(unavailable("surviving worker shutdown pending; retry stop"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }
    /// Revisit survivors without starting closed stores or resetting active idle clocks.
    pub async fn reconcile_survivors(&self) -> Result<Vec<(BogId, String)>, CloudError> {
        let mut failures = Vec::new();
        for bog in self.registry.list()? {
            let Ok(_guard) = self.gate(bog.id)?.try_write_owned() else {
                continue;
            };
            let bog = match self.registry.get(bog.id) {
                Ok(bog) => bog,
                Err(error) if error.code == "not_found" => continue,
                Err(error) => return Err(error),
            };
            if matches!(
                bog.status,
                ObservedState::Maintenance | ObservedState::Restoring
            ) {
                continue;
            }
            if bog.desired_state == DesiredState::Stopped {
                if !self.running.lock().await.contains_key(&bog.id)
                    && !self.survivor_owns_store(bog.id)?
                {
                    continue;
                }
                if let Err(error) = self.stop_locked(bog.id, false).await {
                    failures.push((bog.id, error.code));
                }
                continue;
            }
            if self.running.lock().await.contains_key(&bog.id)
                || !self.instance_dir(bog.id).join("worker.sock").exists()
            {
                continue;
            }
            let client = WorkerClient::new(&self.instance_dir(bog.id).join("worker.sock"))?;
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                self.verify_worker(bog.id, &client),
            )
            .await
            {
                Ok(Ok(())) => {
                    let Ok(slot) = self.active_slots.clone().try_acquire_owned() else {
                        failures.push((bog.id, "capacity".into()));
                        continue;
                    };
                    self.registry
                        .set_status(bog.id, ObservedState::Ready, None)?;
                    self.running.lock().await.insert(
                        bog.id,
                        Running {
                            generation: bog.generation,
                            client,
                            child: None,
                            _slot: slot,
                            last_use: Arc::new(Mutex::new(std::time::Instant::now())),
                        },
                    );
                }
                Ok(Err(error)) => failures.push((bog.id, error.code)),
                Err(_) => failures.push((bog.id, "unavailable".into())),
            }
        }
        Ok(failures)
    }
    pub async fn reconcile(&self) -> Result<Vec<(BogId, String)>, CloudError> {
        crate::backup::recover_staging(&self.config.root.join("backups"))?;
        let mut failures = Vec::new();
        for id in self.registry.pending_deletions()? {
            if let Err(error) = self.cleanup_deleted(id).await {
                failures.push((id, error.code));
            }
        }
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
        }
        failures.extend(self.reconcile_survivors().await?);
        Ok(failures)
    }
    /// Retry filesystem cleanup only after access has been atomically revoked.
    pub async fn cleanup_deleted(&self, id: BogId) -> Result<(), CloudError> {
        let _guard = self.gate(id)?.write_owned().await;
        if !self.registry.is_deleted(id)? {
            return Err(unavailable("database must be deleted before cleanup"));
        }
        self.stop_locked(id, false).await?;
        let dir = self.instance_dir(id);
        if !dir.exists() {
            return self.registry.finish_delete(id);
        }
        // Hold both ownership locks through removal. Socket disappearance is
        // insufficient evidence that Fjall has released the database.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let mut locks = Vec::new();
            let mut busy = false;
            for path in [
                dir.join("worker.lock"),
                dir.join(self.registry.storage_dir(id)?).join("lock"),
            ] {
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)
                {
                    Ok(lock) => {
                        if lock.try_lock().is_err() {
                            busy = true;
                            break;
                        }
                        locks.push(lock);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(unavailable("cannot inspect deleted database ownership")),
                }
            }
            if !busy {
                std::fs::remove_dir_all(&dir)
                    .map_err(|_| unavailable("database cleanup pending; retry deletion"))?;
                return self.registry.finish_delete(id);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(unavailable(
                    "database worker still owns files; retry deletion",
                ));
            }
            drop(locks);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    /// Evict only workers with no active lease and a full idle interval.
    pub async fn evict_idle(&self) -> Result<usize, CloudError> {
        let ids: Vec<_> = self.running.lock().await.keys().copied().collect();
        let mut evicted = 0;
        for id in ids {
            let Ok(_guard) = self.gate(id)?.try_write_owned() else {
                continue;
            };
            if self.registry.building(id)? {
                continue;
            }
            let idle = self.running.lock().await.get(&id).is_some_and(|worker| {
                worker
                    .last_use
                    .lock()
                    .map(|last| last.elapsed() >= self.config.idle_timeout)
                    .unwrap_or(false)
            });
            if idle {
                self.stop_locked(id, false).await?;
                self.event(id, "worker_sleeping", Some("idle_timeout"));
                evicted += 1;
            }
        }
        Ok(evicted)
    }
    pub async fn resident_count(&self) -> usize {
        self.running.lock().await.len()
    }
    /// The caller owns and aborts this task during manager shutdown.
    pub fn spawn_maintenance(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let supervisor = Arc::downgrade(self);
        let period = self
            .config
            .idle_timeout
            .min(std::time::Duration::from_secs(30))
            .max(std::time::Duration::from_millis(10));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            loop {
                interval.tick().await;
                let Some(supervisor) = supervisor.upgrade() else {
                    break;
                };
                for id in supervisor.registry.pending_deletions().unwrap_or_default() {
                    if let Err(error) = supervisor.cleanup_deleted(id).await {
                        eprintln!("database deletion cleanup pending: {}", error.code);
                    }
                }
                match supervisor.reconcile_survivors().await {
                    Ok(failures) => {
                        for (_, code) in failures {
                            eprintln!("worker survivor recovery pending: {code}");
                        }
                    }
                    Err(error) => eprintln!("worker survivor recovery failed: {}", error.code),
                }
                if let Err(error) = supervisor.evict_idle().await {
                    eprintln!("worker idle maintenance failed: {}", error.code);
                }
            }
        })
    }
    /// Stop workers while retaining intent; subsequent access wakes them.
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
