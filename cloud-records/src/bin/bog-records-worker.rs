//! Operator-owned records process. Identity and shutdown exist only on its Unix socket.
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use bog_cloud_records::{TEMPLATE_ID, records_service};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Serialize, Deserialize)]
struct SocketOwner {
    data_dir: PathBuf,
    instance_id: String,
    dev: u64,
    ino: u64,
}
struct OwnedSocket {
    path: PathBuf,
    owner_path: PathBuf,
    dev: u64,
    ino: u64,
    owner_identity: Option<(u64, u64)>,
    clean_exit: bool,
}
impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if self.clean_exit
            && std::fs::symlink_metadata(&self.path).is_ok_and(|m| {
                m.file_type().is_socket() && m.dev() == self.dev && m.ino() == self.ino
            })
        {
            let _ = std::fs::remove_file(&self.path);
            if std::fs::symlink_metadata(&self.owner_path)
                .is_ok_and(|m| Some((m.dev(), m.ino())) == self.owner_identity)
            {
                let _ = std::fs::remove_file(&self.owner_path);
            }
        }
    }
}
fn bind_socket(
    path: &Path,
    data_dir: &Path,
    instance_id: &str,
) -> Result<(tokio::net::UnixListener, OwnedSocket), Box<dyn std::error::Error>> {
    let owner_path = PathBuf::from(format!("{}.owner", path.display()));
    if let Ok(meta) = std::fs::symlink_metadata(&owner_path) {
        if !meta.file_type().is_file() {
            return Err("unsafe socket owner marker".into());
        }
        let owner: SocketOwner = serde_json::from_slice(&std::fs::read(&owner_path)?)?;
        if owner.data_dir != data_dir || owner.instance_id != instance_id {
            return Err("socket marker belongs to another resource".into());
        }
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let owner: SocketOwner = serde_json::from_slice(&std::fs::read(&owner_path)?)?;
        if !meta.file_type().is_socket()
            || owner.data_dir != data_dir
            || owner.instance_id != instance_id
            || owner.dev != meta.dev()
            || owner.ino != meta.ino()
        {
            return Err("socket path belongs to another resource".into());
        }
        match std::os::unix::net::UnixStream::connect(path) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            _ => return Err("socket is active or cannot be verified stale".into()),
        }
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    let mut guard = OwnedSocket {
        path: path.into(),
        owner_path: owner_path.clone(),
        dev: meta.dev(),
        ino: meta.ino(),
        owner_identity: None,
        clean_exit: true,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    // Do not follow an unrelated sidecar symlink.
    if std::fs::symlink_metadata(&owner_path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err("unsafe socket owner marker".into());
    }
    std::fs::write(
        &owner_path,
        serde_json::to_vec(&SocketOwner {
            data_dir: data_dir.into(),
            instance_id: instance_id.into(),
            dev: meta.dev(),
            ino: meta.ino(),
        })?,
    )?;
    let owner_meta = std::fs::symlink_metadata(&owner_path)?;
    guard.owner_identity = Some((owner_meta.dev(), owner_meta.ino()));
    guard.clean_exit = false;
    Ok((listener, guard))
}
#[derive(Deserialize)]
struct Shutdown {
    nonce: String,
}
#[tokio::main]
async fn main() {
    if let Err(_error) = run().await {
        // Deliberately omit arbitrary underlying error text (it could contain private data).
        eprintln!("bog-records-worker: startup or shutdown failed");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut data = None;
    let mut socket = None;
    let mut version = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing flag value")?;
        match flag.as_str() {
            "--data-dir" if data.is_none() => data = Some(PathBuf::from(value)),
            "--socket" if socket.is_none() => socket = Some(PathBuf::from(value)),
            "--template-version" if version.is_none() => version = Some(value),
            _ => return Err("unknown or duplicate flag".into()),
        }
    }
    let data = data.ok_or("missing data-dir")?;
    let socket = socket.ok_or("missing socket")?;
    if !data.is_absolute() || !socket.is_absolute() || version.as_deref() != Some(TEMPLATE_ID) {
        return Err("invalid worker configuration".into());
    }
    let instance_id = std::env::var("BOG_INSTANCE_ID")?;
    let nonce = std::env::var("BOG_STARTUP_NONCE")?;
    if instance_id.is_empty() || nonce.is_empty() {
        return Err("missing identity".into());
    }
    // Acquire store lock before touching any old socket. A competing worker cannot unlink it.
    let service = records_service(&data)?;
    let data = std::fs::canonicalize(data)?;
    let (listener, mut socket_guard) = bind_socket(&socket, &data, &instance_id)?;
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let identity = json!({"instance_id":instance_id,"nonce":nonce,"template_version":TEMPLATE_ID,"pid":std::process::id()});
    let control = Router::new()
        .route(
            "/_cloud/identity",
            get(move || {
                let identity = identity.clone();
                async move { Json(identity) }
            }),
        )
        .route(
            "/_cloud/shutdown",
            post(move |Json(body): Json<Shutdown>| {
                let nonce = nonce.clone();
                let stop = stop_tx.clone();
                async move {
                    if body.nonce != nonce {
                        return (
                            StatusCode::FORBIDDEN,
                            Json(json!({"error":"identity mismatch"})),
                        );
                    }
                    stop.send_replace(true);
                    (StatusCode::OK, Json(json!({"stopping":true})))
                }
            }),
        );
    let router = service.router().merge(control);
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = drain_rx.await;
            })
            .await
    });
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _=terminate.recv()=>{},
        _=tokio::signal::ctrl_c()=>{},
        _=stop_rx.wait_for(|stop|*stop)=>{},
        result=&mut server=>{result??;return Err("listener exited unexpectedly".into())}
    }
    service.begin_shutdown();
    let _ = drain_tx.send(());
    match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
        Ok(result) => {
            result??;
        }
        Err(_) => {
            server.abort();
            let _ = server.await;
            service.shutdown()?;
            return Err("request drain timed out".into());
        }
    }
    service.shutdown()?;
    socket_guard.clean_exit = true;
    Ok(())
}
