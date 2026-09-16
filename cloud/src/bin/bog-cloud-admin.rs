#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!("bog-cloud-admin: operation failed");
        std::process::exit(1);
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let root = std::env::var("BOG_CLOUD_ROOT")?;
    let token = std::env::var("BOG_CLOUD_OWNER_TOKEN")?;
    let (path, body) = match args.as_slice() {
        [command, id] if command == "backup" => (
            format!("/backup/{}", uuid::Uuid::parse_str(id)?),
            serde_json::json!({}),
        ),
        [command, id, flag, name] if command == "restore" && flag == "--name" => (
            format!("/restore/{}", uuid::Uuid::parse_str(id)?),
            serde_json::json!({"name":name}),
        ),
        _ => {
            return Err(
                "usage: bog-cloud-admin backup <bog-id> | restore <archive-id> --name <name>"
                    .into(),
            );
        }
    };
    let client = reqwest::Client::builder()
        .unix_socket(std::path::Path::new(&root).join("admin.sock"))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(300))
        .build()?;
    let response = client
        .post(format!("http://admin{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let result: serde_json::Value = response.json().await?;
    if !status.is_success() {
        return Err("admin operation refused".into());
    }
    println!("{result}");
    Ok(())
}
