//! One bounded public guide shared by Markdown discovery surfaces.
use crate::{CloudService, definitions::ActiveDefinition};

pub const NOTES: &str = include_str!("../../docs/examples/composable/notes.json");

fn authentication(service: &CloudService) -> &'static str {
    if service.native_auth.is_some() {
        "GitHub approval is enabled. Follow /auth.md for device approval or your MCP client's supported sign-in flow. Show the human only the approval link and user code; keep device codes and bearer tokens private."
    } else if service.public_auth.is_some() {
        "External identity provider: obtain credentials privately using /auth.md and protected-resource metadata."
    } else {
        "Operator-only deployment: obtain management credentials privately from the operator; no public signup/device approval."
    }
}

pub fn index(service: &CloudService) -> String {
    let base = crate::public_discovery::origin(service);
    let claimable = if service.supervisor.config.claimable_enabled && service.native_auth.is_some()
    {
        format!(
            "- [Try a temporary Bog]({base}/#try-bog): no sign-in, one hour, claim before expiry. Agents: POST {base}/v1/claimable-bogs or use bog-cloud try.\n"
        )
    } else {
        String::new()
    };
    format!(
        "# Bog Cloud\n\n> Hosted JSON records with maintained views for small apps and agents. Free prototype; keep a separate copy of important data.\n\n{}\n\n{claimable}- [Agent guide]({base}/agent.md): start here for authorization, composition, queries and private app access.\n- [Markdown docs]({base}/docs.md)\n- [Private helper]({base}/bog-app-access.py)\n- [Authentication]({base}/auth.md): active deployment instructions.\n- [API operations]({base}/v1): machine-readable service discovery.\n- [OpenAPI]({base}/openapi.json): exact HTTP contracts.\n- [Notes definition]({base}/examples/notes.json): one composable example.\n- [Agent skills]({base}/.well-known/agent-skills/index.json)\n- [MCP server card]({base}/mcp/server-card)\n- [Resource catalog]({base}/.well-known/ard.json)\n",
        authentication(service)
    )
}

pub fn guide(service: &CloudService) -> String {
    let base = crate::public_discovery::origin(service);
    let definition = crate::definitions::parse(serde_json::from_str(NOTES).expect("notes JSON"))
        .expect("valid notes definition");
    let active = ActiveDefinition {
        digest: definition.digest().expect("notes digest"),
        definition,
        revision: 1,
        storage_dir: "data".into(),
        configured: true,
    };
    let routes = crate::definitions::compact_routes(&active, "BOG_ID");
    let mut route_lines = String::new();
    for route in routes.as_array().expect("routes array") {
        let op = route["op"].as_str().unwrap_or("");
        if !["put", "list", "recent", "text", "semantic", "wait"].contains(&op) {
            continue;
        }
        let method = route["method"].as_str().unwrap_or("POST");
        let path = route["path"]
            .as_str()
            .unwrap_or("")
            .replace("/v1/bogs/BOG_ID", "B");
        route_lines.push_str(&format!("{method} {path} ({op})\n"));
    }
    let notes = serde_json::from_str::<serde_json::Value>(NOTES)
        .unwrap()
        .to_string();
    let auth = if service.native_auth.is_some() {
        format!(
            "GitHub approval is enabled. Download /bog-app-access.py; `python3 bog-app-access.py --origin {base} --connect --auth-file ./bog-auth.json`. Show approval link/code; helper polls and privately saves token."
        )
    } else {
        authentication(service).to_owned()
    };
    let workspace = if service.authentication_configured() {
        "workspace_id defaults personal; shared must be explicit."
    } else {
        "Management defaults to the legacy workspace; public accounts/sharing are disabled."
    };
    let claimable = if service.supervisor.config.claimable_enabled && service.native_auth.is_some()
    {
        format!(
            "## Try before sign-in (one hour)\n\n`bog-cloud --origin {base} try --name notes --output ./temporary-bog.json` creates a real Bog and saves its credential to an owned private file. No account or approval is needed. Add `--definition ./notes.json` to create with a bounded custom definition. Use `bog-cloud --config ./temporary-bog.json claim-link` when ready; show only the returned short-lived claim URL to the human. The human signs in with GitHub, chooses a workspace with room, and claims the same Bog before `expires_at`. Poll `GET /claim/CODE/status` for active, claimed, or expired; do not put the credential in chat. Old credentials stop working on claim. If the output file says pending after a network failure, retry the same command and file. Raw API: `POST /v1/claimable-bogs` with Idempotency-Key and JSON {{\"name\":\"notes\",\"recovery_secret\":\"64 random hex characters\"}}; response includes a private credential, routes, and expiry. A retry with the same key and recovery secret rotates the old temporary credential. Do not print the raw response. MCP can use the temporary bearer after HTTP creation; neither MCP sign-in nor a browser login creates a temporary Bog.\n\n"
        )
    } else {
        String::new()
    };
    format!(
        r#"# Bog Cloud agent guide

Prototype; keep backups. Origin: {base}. Send Authorization: Bearer TOKEN; never expose tokens in chat/URLs/logs.

{auth} See /auth.md. MCP: read /mcp/server-card, initialize, tools/list.

{claimable}

## Three-call start

POST /v1/bogs with Idempotency-Key and {{"name":"notes","wait":true}}; PUT /v1/bogs/ID/docs/n1 with {{"title":"Hello","body":"Remember","updated_at":1}}; GET /v1/bogs/ID/views/docs?limit=10. Use returned ID; require ready. Retry identical key/body; capacity means retry same ID after capacity frees.

{workspace} GET /v1/workspaces: actual bog_limit (null=uncapped). Defaults 3 Bogs/workspace, 16 MiB/Bog; existing allowances differ; host/storage limits still apply.

## Composable notes

Instead create with {{"name":"notes","wait":true,"definition":DEFINITION}}. Use:
```json
{notes}
```
B=/v1/bogs/ID. Generated routes:
```text
{route_lines}```
POST query bodies: list {{"action":"list","limit":10}}, recent {{"action":"top","limit":10,"include_fields":["/title"]}}, wait {{"action":"wait","cursor":"CURSOR","timeout":25}}. Both searches: {{"query":"remember","limit":5,"include_fields":["/title"]}}. PUT the note above.

Table batch_get: {{"action":"batch_get","keys":["n1"]}}. List after/before: exclusive key bounds, no offset. include_fields results use pointer keys, e.g. "/title". Semantic max_distance 0–2; score=1-distance, no universal cutoff.

Keep cursor; wait returns seq,cursor,changed,reset. Refetch on changed/reset; no replay/automatic agent wake. Page max1000, offset max10000; wait 0–25s. 401: authenticate; 403: check scope/membership. writes_paused: await update. GET B/resources: schemas; /v1/components: enabled/limits. Updates: plan/apply/poll; only succeeded activates.

## Private app

prepare_app_access returns nonsecret handoff/command. Helper --handoff --output installs privately; --auth-file reuses only your approval. Never read another client's credentials. App tokens access one Bog only. Load installed config privately.

More: /docs.md /openapi.json /.well-known/agent-skills/index.json /.well-known/ard.json
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notes_is_one_valid_definition_with_all_guide_resources() {
        let d = crate::definitions::parse(serde_json::from_str(NOTES).unwrap()).unwrap();
        for name in ["notes", "recent", "text", "semantic"] {
            assert!(d.resources.contains_key(name));
        }
        assert_eq!(d.expose["wait"].target, "notes");
        let temp = tempfile::tempdir().unwrap();
        bog_runtime::Runtime::open_with_limit(temp.path().join("notes"), d, 16 * 1024 * 1024)
            .unwrap();
    }
    #[test]
    fn operator_documents_are_bounded_and_mode_accurate() {
        let temp = tempfile::tempdir().unwrap();
        let service = CloudService::open(
            crate::config::Config::new(temp.path().into(), std::env::current_exe().unwrap()),
            "test-owner-secret-at-least-thirty-two-bytes",
        )
        .unwrap();
        let index = index(&service);
        let guide = guide(&service);
        assert!(index.len() < 2048, "{}", index.len());
        assert!(guide.len() <= 3300, "{}", guide.len());
        assert!(guide.contains("Operator-only"));
        assert!(guide.contains("POST B/resources/notes/query"));
        assert!(!guide.contains("https://mcp.bog.new"));
    }
}
