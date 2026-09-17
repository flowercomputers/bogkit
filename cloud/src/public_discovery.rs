//! Public documents have no dependency on private resource existence.
use crate::{CloudService, http::guide_asset};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::Arc;
const PUBLIC_ORIGIN: &str = "https://flower-bog-cloud.fly.dev";
fn origin(service: &CloudService) -> String {
    service
        .native_auth
        .as_ref()
        .map(|a| a.config.origin())
        .or_else(|| {
            service
                .public_auth
                .as_ref()
                .and_then(|a| reqwest::Url::parse(&a.verifier.config.resource).ok())
                .map(|u| u.origin().ascii_serialization())
        })
        .unwrap_or_else(|| PUBLIC_ORIGIN.into())
}
/// Require an explicit, acceptable Markdown representation; wildcards keep HTML.
fn markdown(headers: &HeaderMap) -> bool {
    let ranges: Vec<_> = headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|v| {
            let mut parts = v.trim().split(';');
            let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            let q = parts
                .find_map(|p| p.trim().strip_prefix("q="))
                .map(|q| {
                    q.parse::<f32>()
                        .ok()
                        .filter(|q| q.is_finite() && (0.0..=1.0).contains(q))
                        .unwrap_or(0.0)
                })
                .unwrap_or(1.0);
            (media, q)
        })
        .collect();
    let md = ranges
        .iter()
        .find(|(m, _)| m == "text/markdown")
        .map(|(_, q)| *q)
        .unwrap_or(0.0);
    let html = ranges
        .iter()
        .find(|(m, _)| m == "text/html")
        .or_else(|| ranges.iter().find(|(m, _)| m == "text/*"))
        .or_else(|| ranges.iter().find(|(m, _)| m == "*/*"))
        .map(|(_, q)| *q)
        .unwrap_or(0.0);
    md > 0.0 && md >= html
}
fn linked(mut r: Response, base: &str) -> Response {
    r.headers_mut().insert(header::LINK, HeaderValue::from_str(&format!("<{base}/.well-known/ard.json>; rel=\"ard\", <{base}/.well-known/api-catalog>; rel=\"api-catalog\", <{base}/docs>; rel=\"service-doc\", <{base}/openapi.json>; rel=\"service-desc\"; type=\"application/json\"")).unwrap());
    r
}
fn pricing(service: &CloudService) -> String {
    if service.authentication_configured() {
        "Free tier: this prototype is free to use, with open GitHub signup, self-serve API credentials, no payment card, and no sales contact. Each ordinary workspace defaults to three Bogs, with 16 MiB of logical JSON record storage per Bog. Approved uncapped allowances still obey host capacity and per-Bog storage limits.".into()
    } else {
        format!(
            "This prototype is free to use. This operator-only deployment allows up to {} Bogs, with 16 MiB of logical JSON record storage per Bog. Public signup is unavailable.",
            service.supervisor.max_active()
        )
    }
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn metadata(base: &str, path: &str) -> String {
    let url = escape(&format!("{base}{path}"));
    let data=json!({"@context":"https://schema.org","@type":"SoftwareApplication","name":"Bog Cloud","url":format!("{base}/"),"applicationCategory":"DeveloperApplication","operatingSystem":"Web","description":"A working prototype for storing JSON records through HTTP and MCP.","offers":{"@type":"Offer","price":"0","priceCurrency":"USD","description":"Free prototype; capacity limits apply"},"publisher":{"@type":"Organization","name":"Flower Computer","url":"https://www.flowercomputer.com/","contactPoint":{"@type":"ContactPoint","email":"ed@flowercomputer.com","contactType":"general inquiries"},"address":{"@type":"PostalAddress","addressLocality":"Brooklyn","addressRegion":"New York","addressCountry":"US"}}}).to_string().replace('<',"\\u003c");
    format!(
        "<link rel=\"canonical\" href=\"{url}\"><meta property=\"og:type\" content=\"website\"><meta property=\"og:title\" content=\"Bog Cloud\"><meta property=\"og:url\" content=\"{url}\"><meta property=\"og:image\" content=\"{}/og.svg\"><script type=\"application/ld+json\">{data}</script>",
        escape(base)
    )
}
pub async fn homepage(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    let base = origin(&service);
    let mut response = if markdown(request.headers()) {
        guide_asset(
            "text/markdown; charset=utf-8",
            format!(
                "# Bog Cloud\n\nA working prototype by [Flower Computer](https://flowercomputer.com/) for apps and agents to store JSON records through HTTP or bearer-authenticated MCP.\n\nUse it for a small shared notebook, reading list, or script state. Keep a separate copy of important data. SQL and durable event replay are not supported.\n\n[Connect an agent]({base}/connect) · [Documentation]({base}/docs) · [Authentication]({base}/auth.md) · [API operations]({base}/v1) · [OpenAPI]({base}/openapi.json) · [Console]({base}/console)\n\nRead the authentication guide for this deployment's active mode before connecting.\n\n{}\n",
                pricing(&service)
            ),
        )
    } else {
        let html = crate::native_http::guide(&service);
        let meta = metadata(&base, "/");
        let html = if html.contains("</head>") {
            html.replacen("</head>", &format!("{meta}</head>"), 1)
        } else {
            html.replacen("<meta charset=", &format!("{meta}<meta charset="), 1)
        };
        guide_asset("text/html; charset=utf-8", html)
    };
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));
    linked(response, &base)
}
pub async fn document(State(service): State<Arc<CloudService>>, request: Request) -> Response {
    let base = origin(&service);
    let path = request.uri().path();
    if let Some((content_type, body)) = crate::agent_discovery::public_document(path, &base) {
        let mut response = if request.method() == axum::http::Method::OPTIONS {
            StatusCode::NO_CONTENT.into_response()
        } else {
            guide_asset(content_type, body)
        };
        for (name, value) in [
            ("access-control-allow-origin", "*"),
            ("access-control-allow-methods", "GET, HEAD, OPTIONS"),
            (
                "access-control-allow-headers",
                "Content-Type, If-None-Match",
            ),
            ("cache-control", "public, max-age=3600"),
        ] {
            response
                .headers_mut()
                .insert(name, HeaderValue::from_static(value));
        }
        return response;
    }
    match path {
        "/robots.txt" => {
            let rules="Allow: /\nAllow: /mcp/server-card\nDisallow: /console\nDisallow: /auth/\nDisallow: /oauth/\nDisallow: /console-session\nDisallow: /v1/bogs\nDisallow: /v1/workspaces\nDisallow: /v1/invitations\nDisallow: /v1/agent-tokens\nDisallow: /v1/me\nDisallow: /mcp\n";
            let mut body=String::new();
            for agent in ["*","GPTBot","OAI-SearchBot","Claude-Web","Google-Extended"] {body.push_str(&format!("User-agent: {agent}\nContent-Signal: search=yes, ai-input=yes, ai-train=no\n{rules}\n"));}
            body.push_str(&format!("Sitemap: {base}/sitemap.xml\n"));
            guide_asset("text/plain; charset=utf-8",body)
        },
        "/sitemap.xml" => {
            // Dates describe the checked-in public content, never the request time.
            let entries=["/","/docs","/connect","/about","/contact","/privacy"].map(|p|format!("<url><loc>{}</loc><lastmod>2026-09-17</lastmod></url>",escape(&format!("{base}{p}")))).join("");
            guide_asset("application/xml; charset=utf-8",format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{entries}</urlset>"))
        },
        "/.well-known/api-catalog" => linked(guide_asset("application/linkset+json; profile=\"https://www.rfc-editor.org/info/rfc9727\"",json!({"linkset":[{"anchor":format!("{base}/.well-known/api-catalog"),"item":[{"href":format!("{base}/v1"),"type":"application/json"}]},{"anchor":format!("{base}/v1"),"service-doc":[{"href":format!("{base}/docs"),"type":"text/html"}],"service-desc":[{"href":format!("{base}/openapi.json"),"type":"application/json"}]}]}).to_string()),&base),
        "/og.svg"=>guide_asset("image/svg+xml",include_str!("../static/og.svg")),
        _ => {
            let (title,body)= match path {
                "/connect"=>("Connect an agent", if service.native_auth.is_some() { include_str!("../static/connect.html").replace("{{origin}}", &escape(&base)) } else { "<p>GitHub agent connection is unavailable on this deployment. An operator must supply an appropriate credential privately. App credentials access one existing Bog; provisioning requires management access.</p><p><a href=\"/auth.md\">Read the active authentication instructions</a></p>".into() }),
                "/docs"=>("Documentation",include_str!("../static/public-docs.html").replace("{{pricing}}", &escape(&pricing(&service)))),
                "/about"=>("About Bog Cloud",include_str!("../static/about.html").into()),
                "/contact"=>("Contact Bog Cloud",include_str!("../static/contact.html").into()),
                _=>("Privacy notes","<p>Bog Cloud uses credentials to control access to workspaces and records. When this deployment enables GitHub sign-in, it identifies your account; repository access is not requested. Never include credentials in chat messages or URLs. This prototype is evolving: keep a separate copy of important data.</p><p>For questions about data handling, contact <a href=\"https://flowercomputer.com/\">Flower Computer</a>. These notes do not specify a retention schedule.</p>".into())
            };
            linked(guide_asset("text/html; charset=utf-8",format!("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{title} — Bog Cloud</title>{}<link rel=\"stylesheet\" href=\"/guide.css\"></head><body><header><a href=\"/\">Bog Cloud</a><nav><a href=\"/docs\">Documentation</a> · <a href=\"/auth.md\">Authentication</a></nav></header><main><h1>{title}</h1>{body}</main></body></html>",metadata(&base,path))),&base)
        }
    }
}
pub fn is_public_unknown(path: &str) -> bool {
    !["/v1", "/auth", "/console", "/mcp"]
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}
pub fn not_found(headers: &HeaderMap) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut r = if markdown(headers) {
        (StatusCode::NOT_FOUND,[(header::CONTENT_TYPE,"text/markdown; charset=utf-8")],"# Not found\n\nThis public page does not exist. Start at [Bog Cloud](/), [documentation](/docs), or [API discovery](/v1).\n").into_response()
    } else {
        (StatusCode::NOT_FOUND,Json(json!({"error":{"code":"not_found","message":"Public page not found"},"request_id":request_id,"links":{"home":"/","docs":"/docs","api":"/v1"}}))).into_response()
    };
    r.headers_mut()
        .insert("x-request-id", HeaderValue::from_str(&request_id).unwrap());
    r.headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));
    r
}
