//! Public, advisory discovery documents. These drafts do not grant access.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const SKILL: &str = include_str!("../static/agent-skills/bog-cloud.md");
pub const SKILL_PATH: &str = "/.well-known/agent-skills/bog-cloud/SKILL.md";

pub fn skills_index() -> Value {
    json!({
        "$schema": "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
        "skills": [{"name":"bog-cloud", "type":"skill-md",
            "description":"Create a small hosted JSON database and build server-side prototypes using Bog Cloud HTTP or MCP.",
            "url":SKILL_PATH, "digest":format!("sha256:{:x}", Sha256::digest(SKILL.as_bytes()))}]
    })
}

pub fn server_card(base: &str) -> Value {
    let base = base.trim_end_matches('/');
    let endpoint = if matches!(
        base,
        "https://cloud.bog.new" | "https://mcp.bog.new" | "https://flower-bog-cloud.fly.dev"
    ) {
        "https://mcp.bog.new/mcp".to_owned()
    } else {
        format!("{base}/mcp")
    };
    json!({
        "$schema":"https://static.modelcontextprotocol.io/schemas/v1/server-card.schema.json",
        "name":"io.fly.flower-bog-cloud/bog-cloud", "title":"Bog Cloud",
        "version":env!("CARGO_PKG_VERSION"),
        "description":"Hosted JSON databases for small prototypes, with HTTP and MCP access.",
        "websiteUrl":format!("{base}/agent.md"),
        "remotes":[{"type":"streamable-http", "url":endpoint,
            "supportedProtocolVersions":["2025-11-25"],
            "headers":[{"name":"Authorization", "isRequired":true,"isSecret":true,
                "description":format!("Bearer token. Read {base}/auth.md for approval or temporary access instructions; never paste credentials into chat.")}]}],
        "_meta":{"flowercomputer.com/runtime-name":"bog-cloud",
            "flowercomputer.com/discovery-status":"Experimental server-card draft; the catalog identifier is namespaced, while initialize reports bog-cloud."}
    })
}

pub fn catalog(base: &str) -> Value {
    let base = base.trim_end_matches('/');
    // Use the configured public origin as the publisher authority, never a request Host header.
    let publisher = reqwest::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "flower-bog-cloud.fly.dev".to_owned());
    json!({"specVersion":"1.0", "entries":[{
        "@context":"https://agenticresourcediscovery.org/context/v1",
        "identifier":format!("urn:air:{publisher}:mcp:bog-cloud"),
        "displayName":"Bog Cloud", "type":"application/mcp-server-card+json",
        "url":format!("{base}/mcp/server-card"),
        "description":"Create and use small hosted JSON databases after account authorization.",
        "representativeQueries":["Create a small database for my chat prototype", "Read and update JSON records in my Bog", "Wait for my Bog's records to change"]
    }, {
        "@context":"https://agenticresourcediscovery.org/context/v1",
        "identifier":format!("urn:air:{publisher}:api:bog-cloud"),
        "displayName":"Bog Cloud HTTP API", "type":"application/json",
        "url":format!("{base}/openapi.json"),
        "description":"OpenAPI description of Bog Cloud's authenticated JSON database API.",
        "representativeQueries":["Build a server-side app using the Bog HTTP API", "Find the request format for storing JSON records"]
    }, {
        "@context":"https://agenticresourcediscovery.org/context/v1",
        "identifier":format!("urn:air:{publisher}:guide:bog-cloud"),
        "displayName":"Bog Cloud agent guide", "type":"text/markdown",
        "url":format!("{base}/agent.md"),
        "description":"Start here for active authentication, a composable notes example and private app installation."
    }]})
}

pub fn public_document(path: &str, base: &str) -> Option<(&'static str, String)> {
    let (content_type, value) = match path {
        "/.well-known/agent-skills/index.json" => ("application/json", skills_index()),
        "/.well-known/ard.json" | "/.well-known/ai-catalog.json" => {
            ("application/ai-catalog+json", catalog(base))
        }
        "/mcp/server-card" | "/.well-known/mcp/server-card.json" => {
            ("application/mcp-server-card+json", server_card(base))
        }
        SKILL_PATH => return Some(("text/markdown; charset=utf-8", SKILL.to_owned())),
        _ => return None,
    };
    Some((content_type, value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_alias_cards_advertise_canonical_remote() {
        for origin in [
            "https://cloud.bog.new",
            "https://mcp.bog.new",
            "https://flower-bog-cloud.fly.dev",
        ] {
            assert_eq!(
                server_card(origin)["remotes"][0]["url"],
                "https://mcp.bog.new/mcp"
            );
        }
    }
    #[test]
    fn hosted_skill_awaits_private_helper_and_uses_current_guides() {
        for required in [
            "/agent.md",
            "/docs.md",
            "/llms.txt",
            "--connect --auth-file",
            "await its completion",
            "--handoff HANDOFF_ID",
            "before** the initial view",
        ] {
            assert!(SKILL.contains(required), "missing {required}");
        }
        assert!(!SKILL.contains("via `POST /v1/bogs/{id}/tokens`"));
    }

    #[test]
    fn skill_digest_and_frontmatter_match_the_published_artifact() {
        let index = skills_index();
        let entry = &index["skills"][0];
        let (_, artifact) =
            public_document(entry["url"].as_str().unwrap(), "https://example.test").unwrap();
        assert_eq!(
            entry["digest"],
            format!("sha256:{:x}", Sha256::digest(artifact.as_bytes()))
        );
        assert!(artifact.starts_with("---\nname: bog-cloud\n"));
        assert!(artifact.contains(entry["description"].as_str().unwrap()));
        assert!(
            public_document(
                "/.well-known/agent-skills/missing/SKILL.md",
                "https://example.test"
            )
            .is_none()
        );
    }
    #[test]
    fn catalog_links_resolve_to_an_advisory_card_without_fake_capabilities() {
        let base = "https://example.test/";
        let catalog = catalog(base);
        let entry = &catalog["entries"][0];
        assert_eq!(entry["identifier"], "urn:air:example.test:mcp:bog-cloud");
        assert!(entry.get("data").is_none());
        let path = reqwest::Url::parse(entry["url"].as_str().unwrap())
            .unwrap()
            .path()
            .to_owned();
        let (kind, body) = public_document(&path, base).unwrap();
        assert_eq!(kind, entry["type"].as_str().unwrap());
        let card: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(card["remotes"][0]["url"], "https://example.test/mcp");
        assert_eq!(card["remotes"][0]["headers"][0]["isSecret"], true);
        for absent in ["capabilities", "tools", "authentication", "oauth"] {
            assert!(card.get(absent).is_none());
        }
        assert!(card["description"].as_str().unwrap().len() <= 100);
        assert_eq!(card["name"].as_str().unwrap().matches('/').count(), 1);
        assert_eq!(
            public_document("/.well-known/ard.json", base),
            public_document("/.well-known/ai-catalog.json", base)
        );
    }
}
