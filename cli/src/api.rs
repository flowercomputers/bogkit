use anyhow::Context;

/// Fetch and pretty-print a running server's OpenAPI document.
pub fn run(port: Option<u16>) -> anyhow::Result<()> {
    // same resolution order as the server itself: flag, $PORT, default
    let port = port
        .or_else(|| std::env::var("PORT").ok().and_then(|p| p.parse().ok()))
        .unwrap_or(7877);
    let url = format!("http://localhost:{port}/openapi.json");

    let doc: serde_json::Value = ureq::get(&url)
        .call()
        .with_context(|| format!("fetching {url} — is the server running? (`bogkit dev`)"))?
        .into_json()
        .context("parsing the OpenAPI document")?;

    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}
