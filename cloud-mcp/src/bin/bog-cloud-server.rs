//! Combined authenticated REST and MCP gateway.
use bog_cloud::{CloudService, config::Config};
use bog_cloud_mcp::{McpOptions, build_mcp_router_with_options};
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("bog-cloud-server: {error}");
        std::process::exit(1);
    }
}
fn entries(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .collect()
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::var("BOG_CLOUD_ROOT").map_err(|_| "BOG_CLOUD_ROOT is required")?;
    let binary = std::env::var("BOG_WORKER_BINARY").map_err(|_| "BOG_WORKER_BINARY is required")?;
    let token =
        std::env::var("BOG_CLOUD_OWNER_TOKEN").map_err(|_| "BOG_CLOUD_OWNER_TOKEN is required")?;
    let bind = std::env::var("BOG_CLOUD_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let hosts = entries("BOG_CLOUD_ALLOWED_HOSTS");
    let options = McpOptions {
        allowed_origins: entries("BOG_CLOUD_ALLOWED_ORIGINS"),
        allowed_hosts: (!hosts.is_empty()).then_some(hosts),
    };
    let service = CloudService::open(
        Config::from_env(root.clone().into(), binary.into())?,
        &token,
    )?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    for (_, code) in service.supervisor.reconcile().await? {
        eprintln!("{{\"event\":\"reconcile_failed\",\"code\":\"{code}\"}}");
    }
    let admin = bog_cloud::admin::bind(
        service.clone(),
        std::path::Path::new(&root).join("admin.sock"),
    )
    .await?;
    let app = bog_cloud::build_rest_router(service.clone())
        .merge(build_mcp_router_with_options(service.clone(), options));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let serving = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        })
        .await;
    admin.shutdown().await?;
    service.supervisor.shutdown().await?;
    serving?;
    Ok(())
}
