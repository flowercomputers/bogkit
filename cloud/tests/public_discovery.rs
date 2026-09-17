use bog_cloud::{CloudService, build_rest_router, config::Config};
use serde_json::Value;
const OWNER: &str = "test-owner-secret-at-least-thirty-two-bytes";
#[tokio::test]
async fn public_discovery_over_real_http_preserves_auth_boundaries() {
    let tmp = tempfile::tempdir().unwrap();
    let service = CloudService::open(
        Config::new(tmp.path().join("service"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, build_rest_router(service))
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let html = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(html.status(), 200);
    assert_eq!(html.headers()["vary"], "Accept");
    assert!(
        html.headers()["link"]
            .to_str()
            .unwrap()
            .contains("rel=\"api-catalog\"")
    );
    let html = html.text().await.unwrap();
    for needle in [
        "A small home",
        "/guide.js",
        "/guide.css",
        "lang=\"en\"",
        "rel=\"canonical\"",
        "og:type",
        "og:image",
        "SoftwareApplication",
        "href=\"/docs\"",
    ] {
        assert!(html.contains(needle), "missing {needle}");
    }
    for (accept, want_markdown) in [
        ("text/markdown", true),
        ("text/markdown;q=0", false),
        ("text/html,text/markdown;q=0.5", false),
        ("text/markdown;q=0.9,text/html;q=0.2", true),
        ("*/*", false),
        ("text/markdown;q=invalid", false),
        ("text/markdown;q=0, */*;q=1", false),
    ] {
        let r = client
            .get(format!("{base}/"))
            .header("Accept", accept)
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/markdown"),
            want_markdown,
            "{accept}"
        );
        let body = r.text().await.unwrap();
        if !want_markdown {
            assert_eq!(body, html)
        } else {
            assert!(body.starts_with("# Bog Cloud"));
            assert!(body.contains("/auth.md"));
        }
    }
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        let r = client
            .request(method.clone(), format!("{base}/.well-known/api-catalog"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(
            r.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("application/linkset+json")
        );
        assert!(
            r.headers()["link"]
                .to_str()
                .unwrap()
                .contains("service-desc")
        );
        let body = r.text().await.unwrap();
        if method == reqwest::Method::HEAD {
            assert!(body.is_empty())
        } else {
            let v: Value = serde_json::from_str(&body).unwrap();
            assert!(
                v["linkset"][0]["item"][0]["href"]
                    .as_str()
                    .unwrap()
                    .ends_with("/v1")
            );
            assert!(
                v["linkset"][1]["service-doc"][0]["href"]
                    .as_str()
                    .unwrap()
                    .ends_with("/docs")
            );
        }
    }
    let r = client
        .get(format!("{base}/robots.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/plain; charset=utf-8");
    let robots = r.text().await.unwrap();
    let groups: Vec<_> = robots.split("User-agent: ").skip(1).collect();
    assert_eq!(groups.len(), 5);
    for group in groups {
        assert!(group.contains("Allow: /\n"));
        for private in [
            "/console",
            "/auth/",
            "/v1/bogs",
            "/v1/workspaces",
            "/v1/me",
            "/mcp",
        ] {
            assert!(group.contains(&format!("Disallow: {private}\n")));
        }
    }
    assert!(robots.contains("Content-Signal: search=yes, ai-input=yes, ai-train=no"));
    assert!(robots.contains("Sitemap: https://flower-bog-cloud.fly.dev/sitemap.xml"));
    let r = client
        .get(format!("{base}/sitemap.xml"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.headers()["content-type"],
        "application/xml; charset=utf-8"
    );
    let sitemap = r.text().await.unwrap();
    assert!(sitemap.contains("http://www.sitemaps.org/schemas/sitemap/0.9"));
    assert_eq!(sitemap.matches("<loc>").count(), 5);
    assert!(!sitemap.contains("/console"));
    assert!(!sitemap.contains("/v1"));
    assert!(sitemap.contains("<lastmod>2026-09-17</lastmod>"));
    for path in ["/docs", "/about", "/contact", "/privacy", "/og.svg"] {
        let r = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        let body = r.text().await.unwrap();
        if path != "/og.svg" {
            assert!(body.contains("href=\"/\""));
        }
        if path == "/docs" {
            assert!(body.contains("This prototype is free to use."));
            assert!(!body.contains("{{pricing}}"));
            for example in [
                "Idempotency-Key",
                "-X PUT",
                "/docs/dune",
                "/changes?timeout=0",
                "/auth.md",
            ] {
                assert!(body.contains(example));
            }
        }
    }
    for accept in ["application/json", "text/markdown"] {
        let r = client
            .get(format!("{base}/unknown-public-page"))
            .header("accept", accept)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        assert_eq!(r.headers()["vary"], "Accept");
        assert!(
            r.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with(accept)
        );
    }
    for path in [
        "/v1/bogs",
        "/v1/bogs/unknown/docs/key",
        "/v1/workspaces",
        "/v1/agent-tokens",
    ] {
        let r = client
            .get(format!("{base}{path}"))
            .header("Accept", "text/markdown")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "{path}");
        assert!(
            r.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("application/json")
        );
    }
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/openid-configuration",
        "/.well-known/oauth-protected-resource",
    ] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "{path}"
        );
    }
    server.abort();
}

#[tokio::test]
async fn native_documents_redirect_and_actual_rate_limit_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut service = CloudService::open(
        Config::new(tmp.path().join("service"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let native = bog_cloud::native_auth::NativeAuth::new(
        bog_cloud::native_auth::GithubConfig::new(
            "test-client",
            "test-secret",
            "https://flower-bog-cloud.fly.dev/auth/callback",
        )
        .unwrap(),
        &tmp.path().join("native"),
    )
    .unwrap();
    std::sync::Arc::get_mut(&mut service).unwrap().native_auth = Some(native);
    let principal = service.auth.authenticate(OWNER).unwrap();
    for _ in 0..599 {
        service.rate_limit(&principal).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, build_rest_router(service))
            .await
            .unwrap()
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = client
        .get(format!("{base}/auth/login"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 303);
    assert!(
        r.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with("https://github.com/login/oauth/authorize?")
    );
    let r = client.get(format!("{base}/")).send().await.unwrap();
    let body = r.text().await.unwrap();
    for needle in [
        "Sign in with GitHub",
        "href=\"/docs\"",
        "SoftwareApplication",
        "rel=\"canonical\"",
    ] {
        assert!(body.contains(needle), "{needle}");
    }
    for (path, status) in [
        ("/.well-known/oauth-authorization-server", 200),
        ("/.well-known/openid-configuration", 404),
        ("/.well-known/oauth-protected-resource", 200),
    ] {
        assert_eq!(client.get(format!("{base}{path}")).send().await.unwrap().status(), status);
    }
    server.abort();
    let service = CloudService::open(
        Config::new(tmp.path().join("legacy"), std::env::current_exe().unwrap()),
        OWNER,
    )
    .unwrap();
    let principal = service.auth.authenticate(OWNER).unwrap();
    for _ in 0..599 {
        service.rate_limit(&principal).unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, build_rest_router(service))
            .await
            .unwrap()
    });
    let r = client
        .get(format!("{base}/v1/workspaces"))
        .bearer_auth(OWNER)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["ratelimit-limit"], "600");
    assert_eq!(r.headers()["ratelimit-remaining"], "0");
    let reset: u64 = r.headers()["ratelimit-reset"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&reset));
    let r = client
        .get(format!("{base}/v1/workspaces"))
        .bearer_auth(OWNER)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(r.headers()["ratelimit-remaining"], "0");
    assert_eq!(r.headers()["retry-after"], r.headers()["ratelimit-reset"]);
    let r = client
        .get(format!("{base}/v1/workspaces"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(!r.headers().contains_key("ratelimit-remaining"));
    server.abort();
}
