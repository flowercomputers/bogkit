//! Standalone REST gateway. The combined MCP binary uses the same service.
use bog_cloud::{CloudService, config::Config, http::build_rest_router};
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bog-cloud: {error}");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var("BOG_CLOUD_ROOT").map_err(|_| "BOG_CLOUD_ROOT is required")?;
    let binary = std::env::var("BOG_WORKER_BINARY").map_err(|_| "BOG_WORKER_BINARY is required")?;
    let token =
        std::env::var("BOG_CLOUD_OWNER_TOKEN").map_err(|_| "BOG_CLOUD_OWNER_TOKEN is required")?;
    let bind = std::env::var("BOG_CLOUD_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let svc = CloudService::open(
        Config::from_env(root.clone().into(), binary.into())?,
        &token,
    )?;
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    for (_, code) in svc.supervisor.reconcile().await? {
        eprintln!("{{\"event\":\"reconcile_failed\",\"code\":\"{code}\"}}");
    }
    let admin =
        bog_cloud::admin::bind(svc.clone(), std::path::Path::new(&root).join("admin.sock")).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    axum::serve(listener, build_rest_router(svc.clone()))
        .with_graceful_shutdown(async move {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        })
        .await?;
    admin.shutdown().await?;
    svc.supervisor.shutdown().await?;
    Ok(())
}
