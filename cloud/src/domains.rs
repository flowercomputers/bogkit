//! Custom-domain aliases keep the existing authorization issuer and host-only
//! browser sessions stable. Never derive a trusted resource from arbitrary Host.
use crate::CloudService;
use axum::{
    Json,
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use std::sync::Arc;

const CLOUD: &str = "https://cloud.bog.new";
const MCP: &str = "https://mcp.bog.new";
fn public_origin(host: &str) -> Option<&'static str> {
    match host {
        "cloud.bog.new" => Some(CLOUD),
        "mcp.bog.new" => Some(MCP),
        _ => None,
    }
}

pub async fn aliases(
    State(service): State<Arc<CloudService>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(native) = service
        .native_auth
        .as_ref()
        .filter(|a| a.config.custom_domains_enabled())
    else {
        return next.run(request).await;
    };
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let Some(base) = public_origin(host) else {
        return next.run(request).await;
    };
    let path = request.uri().path();
    // The browser login cookie must be set on the registered callback host.
    // Keeping the console there avoids cross-domain cookie/token transfers.
    if path == "/console"
        || path == "/auth/login"
        || path == "/auth/callback"
        || path == "/auth/device/approve"
    {
        let target = format!(
            "{}{}",
            native.config.origin(),
            request
                .uri()
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or(path)
        );
        let mut response = Redirect::temporary(&target).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        return response;
    }
    if base == MCP && path == "/" {
        return Redirect::temporary("/mcp").into_response();
    }
    if matches!(
        path,
        "/.well-known/oauth-protected-resource" | "/.well-known/oauth-protected-resource/mcp"
    ) {
        let mcp = base == MCP || path.ends_with("/mcp");
        let mut metadata = if mcp {
            native.resource_metadata()
        } else {
            native.api_resource_metadata()
        };
        metadata["resource"] = serde_json::json!(if mcp {
            format!("{base}/mcp")
        } else {
            base.to_owned()
        });
        metadata["resource_documentation"] = serde_json::json!(format!("{CLOUD}/connect"));
        return ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], Json(metadata)).into_response();
    }
    let mut response = next.run(request).await;
    if let Some(challenge) = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
    {
        let updated = challenge.replace(
            &format!("resource_metadata=\"{}/", native.config.origin()),
            &format!("resource_metadata=\"{base}/"),
        );
        if let Ok(value) = updated.parse() {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_public_hosts_and_resources_are_trusted() {
        assert_eq!(public_origin("cloud.bog.new"), Some(CLOUD));
        assert_eq!(public_origin("mcp.bog.new"), Some(MCP));
        for host in [
            "evil.example",
            "cloud.bog.new.evil.example",
            "cloud.bog.new:444",
        ] {
            assert_eq!(public_origin(host), None);
        }
        let config = crate::native_auth::GithubConfig::new(
            "test",
            "test",
            "https://flower-bog-cloud.fly.dev/auth/callback",
        )
        .unwrap();
        for resource in [
            "https://flower-bog-cloud.fly.dev/mcp",
            "https://cloud.bog.new/mcp",
            "https://mcp.bog.new/mcp",
        ] {
            assert!(config.accepts_resource(resource, false));
            assert!(config.accepts_resource(resource, true));
        }
        for resource in ["https://flower-bog-cloud.fly.dev", "https://cloud.bog.new"] {
            assert!(config.accepts_resource(resource, true));
            assert!(!config.accepts_resource(resource, false));
        }
        for resource in [
            "https://evil.example/mcp",
            "http://cloud.bog.new",
            "https://cloud.bog.new/",
            "https://mcp.bog.new",
        ] {
            assert!(!config.accepts_resource(resource, true));
        }
    }
}
