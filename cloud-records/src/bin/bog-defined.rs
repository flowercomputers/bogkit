//! Run a definition locally using the same durable runtime and HTTP contracts as Cloud.
use bog_cloud_records::{ConfiguredService, DEFAULT_LOGICAL_BYTES};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut data = None;
    let mut definition = None;
    let mut port = 7877u16;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing flag value")?;
        match flag.as_str() {
            "--data-dir" => data = Some(std::path::PathBuf::from(value)),
            "--definition-file" => definition = Some(value),
            "--port" => port = value.parse()?,
            _ => return Err("unknown argument".into()),
        }
    }
    let definition: bog_definition::Definition = serde_json::from_slice(&std::fs::read(
        definition.ok_or("--definition-file required")?,
    )?)?;
    let limits: bog_definition::Limits = match std::env::var("BOG_COMPOSABLE_LIMITS") {
        Ok(v) => serde_json::from_str(&v)?,
        Err(std::env::VarError::NotPresent) => Default::default(),
        Err(e) => return Err(e.into()),
    };
    let service = ConfiguredService::open_with_limits(
        &data.ok_or("--data-dir required")?,
        definition,
        1,
        DEFAULT_LOGICAL_BYTES,
        limits,
    )?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    println!(
        "Bog definition server listening on http://{}",
        listener.local_addr()?
    );
    let router = service.local_router();
    let signal_service = service.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            signal_service.begin_shutdown();
        })
        .await?;
    service.begin_shutdown();
    service.shutdown()?;
    Ok(())
}
