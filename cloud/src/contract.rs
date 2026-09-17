//! Shared public operation descriptions, used by discovery and clients.
use serde_json::{Value, json};
pub const OPERATIONS: &[(&str, &str, &str, &str)] = &[
    (
        "schema",
        "GET",
        "/v1/bogs/{bog_id}/schema",
        "Read the actual records template schema.",
    ),
    (
        "wait_for_change",
        "GET",
        "/v1/bogs/{bog_id}/changes",
        "Wait up to 25 seconds using an opaque cursor; reset means refetch. Initial state acquisition has a separate 25-second ceiling even with timeout=0. No durable event replay.",
    ),
    (
        "create_bog",
        "POST",
        "/v1/bogs",
        "Create a workspace database. Name and Idempotency-Key required; template defaults to records-v1.",
    ),
    (
        "list_bogs",
        "GET",
        "/v1/bogs",
        "List databases in an explicitly selected workspace, default personal.",
    ),
    (
        "list_workspaces",
        "GET",
        "/v1/workspaces",
        "List current workspace memberships.",
    ),
    (
        "describe_bog",
        "GET",
        "/v1/bogs/{bog_id}",
        "Read database status.",
    ),
    (
        "get_record",
        "GET",
        "/v1/bogs/{bog_id}/docs/{key}",
        "Read one JSON record.",
    ),
    (
        "upsert_record",
        "PUT",
        "/v1/bogs/{bog_id}/docs/{key}",
        "Replace a JSON object.",
    ),
    (
        "delete_record",
        "DELETE",
        "/v1/bogs/{bog_id}/docs/{key}",
        "Delete a record.",
    ),
    (
        "read_view",
        "GET",
        "/v1/bogs/{bog_id}/views/{view}",
        "Read docs or total, limit defaults to 100, maximum 1000; offset maximum 10000.",
    ),
    (
        "batch",
        "POST",
        "/v1/bogs/{bog_id}/batch",
        "Atomically apply up to 100 mutations.",
    ),
    (
        "issue_token",
        "POST",
        "/v1/bogs/{bog_id}/tokens",
        "Issue a single database read or write application credential, shown only once.",
    ),
    (
        "list_tokens",
        "GET",
        "/v1/bogs/{bog_id}/tokens",
        "List credential metadata, never secrets.",
    ),
    (
        "revoke_token",
        "DELETE",
        "/v1/bogs/{bog_id}/tokens/{token_id}",
        "Revoke a database credential.",
    ),
];
pub fn description(name: &str) -> Option<&'static str> {
    OPERATIONS.iter().find(|o| o.0 == name).map(|o| o.3)
}
pub fn overview() -> Value {
    json!({"api":"Bog Cloud","authentication":"/auth.md","openapi":"/openapi.json","mcp":"/mcp","templates":[{"id":"records-v1","default":true,"description":"JSON object records keyed by string; docs and total views."}],"workspace_selection":"workspace_id query parameter (REST) or tool argument (MCP), defaults to personal workspace","create_example":{"method":"POST","path":"/v1/bogs","headers":{"Content-Type":"application/json","Idempotency-Key":"your-stable-request-id"},"body":{"name":"my-records"}},"limits":{"bogs_per_workspace":3,"logical_bytes_per_bog":16777216}})
}
pub fn openapi() -> Value {
    let mut paths = serde_json::Map::new();
    for (name, method, path, description) in OPERATIONS {
        let mut parameters = vec![
            json!({"name":"workspace_id","in":"query","required":false,"schema":{"type":"string","format":"uuid"}}),
        ];
        for segment in path.split('/') {
            if let Some(parameter) = segment.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
                parameters.push(json!({"name":parameter,"in":"path","required":true,"schema":{"type":"string"}}));
            }
        }
        let body = match *name {
            "create_bog" => {
                parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1}}));
                Some(
                    json!({"type":"object","required":["name"],"additionalProperties":false,"properties":{"name":{"type":"string","minLength":1},"template":{"type":"string","enum":["records-v1"],"default":"records-v1"}},"example":{"name":"my-records"}}),
                )
            }
            "upsert_record" => Some(
                json!({"type":"object","additionalProperties":true,"example":{"message":"hello"}}),
            ),
            "issue_token" => Some(
                json!({"type":"object","required":["scope"],"additionalProperties":false,"properties":{"scope":{"type":"string","enum":["read","write"]}}}),
            ),
            "batch" => Some(
                json!({"type":"array","maxItems":100,"items":{"oneOf":[{"type":"object","required":["op","key","data"],"additionalProperties":false,"properties":{"op":{"const":"upsert"},"key":{"type":"string"},"data":{"type":"object"}}},{"type":"object","required":["op","key"],"additionalProperties":false,"properties":{"op":{"const":"remove"},"key":{"type":"string"}}}]}}),
            ),
            "wait_for_change" => {
                parameters.extend([json!({"name":"cursor","in":"query","schema":{"type":"string"}}),json!({"name":"timeout","in":"query","schema":{"type":"integer","minimum":0,"maximum":25,"default":25}})]);
                None
            }
            "read_view" => {
                parameters.extend([json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":0,"maximum":1000,"default":100}}),json!({"name":"offset","in":"query","schema":{"type":"integer","minimum":0,"maximum":10000,"default":0}})]);
                None
            }
            _ => None,
        };
        let mut op = json!({"operationId":name,"description":description,"security":[{"bearer":[]}],"parameters":parameters,"responses":{"200":{"description":"Operation result"},"202":{"description":"Provisioning or deletion accepted"},"204":{"description":"Credential revoked"},"400":{"description":"Actionable invalid request"},"401":{"description":"Authentication required"},"403":{"description":"Workspace permission denied"},"404":{"description":"Resource unavailable"},"429":{"description":"Quota or rate limit"}}});
        if let Some(schema) = body {
            op["requestBody"] =
                json!({"required":true,"content":{"application/json":{"schema":schema}}});
        }
        paths.entry(*path).or_insert_with(|| json!({}))[method.to_lowercase()] = op;
    }
    for (method, path, description, fields) in [
        ("get", "/v1/me", "Current account", vec![]),
        (
            "get",
            "/v1/bogs/{bog_id}/usage",
            "Current logical storage usage",
            vec![],
        ),
        (
            "delete",
            "/v1/bogs/{bog_id}",
            "Owner-confirmed permanent deletion",
            vec!["confirm"],
        ),
        (
            "get",
            "/v1/workspaces/{workspace_id}/members",
            "List members",
            vec![],
        ),
        (
            "delete",
            "/v1/workspaces/{workspace_id}/members/{account_id}",
            "Owner removes member and revokes issued credentials",
            vec![],
        ),
        (
            "get",
            "/v1/workspaces/{workspace_id}/invitations",
            "Owner lists invitation metadata",
            vec![],
        ),
        (
            "post",
            "/v1/workspaces/{workspace_id}/invitations",
            "Owner creates seven-day invitation",
            vec!["role"],
        ),
        (
            "delete",
            "/v1/workspaces/{workspace_id}/invitations/{invitation_id}",
            "Owner revokes invitation",
            vec![],
        ),
        (
            "post",
            "/v1/invitations/preview",
            "Signed-in browser previews invitation",
            vec!["secret"],
        ),
        (
            "post",
            "/v1/invitations/accept",
            "Signed-in browser accepts invitation",
            vec!["secret"],
        ),
    ] {
        let mut parameters = path
            .split('/')
            .filter_map(|v| v.strip_prefix('{').and_then(|v| v.strip_suffix('}')))
            .map(|n| json!({"name":n,"in":"path","required":true,"schema":{"type":"string"}}))
            .collect::<Vec<_>>();
        if path.starts_with("/v1/bogs/") {
            parameters.push(json!({"name":"workspace_id","in":"query","required":false,"schema":{"type":"string","format":"uuid"}}));
        }
        let mut op = json!({"description":description,"security":[{"bearer":[]},{"browserSession":[]}],"parameters":parameters,"responses":{"200":{"description":"Result"},"202":{"description":"Deletion accepted"},"401":{"description":"Authentication required"},"403":{"description":"Permission denied"}}});
        if !fields.is_empty() {
            let properties = fields
                .iter()
                .map(|f| ((*f).to_string(), json!({"type":"string"})))
                .collect::<serde_json::Map<_, _>>();
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"type":"object","properties":properties,"required":fields}}}});
        }
        paths.entry(path).or_insert_with(|| json!({}))[method] = op;
    }
    json!({"openapi":"3.1.0","info":{"title":"Bog Cloud","version":"1","description":"Cookie-authenticated mutations require exact Origin and x-csrf-token from /console-session. View ordering is implementation-defined; no insertion-order or replay guarantee."},"paths":paths,"components":{"securitySchemes":{"bearer":{"type":"http","scheme":"bearer"},"browserSession":{"type":"apiKey","in":"cookie","name":"__Host-bog_session"}}}})
}
pub fn llms() -> String {
    format!(
        "# Bog Cloud\n\nAuthentication: /auth.md\nAPI schema: /openapi.json\nTemplates: /v1/templates\nMCP: /mcp\n\n{}\n\nNever put credentials in URLs. Workspace defaults to personal; pass workspace_id explicitly for teams. Application credentials only access their one Bog and cannot provision or mint credentials.\n",
        OPERATIONS
            .iter()
            .map(|(n, m, p, d)| format!("- {n}: {m} {p}. {d}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Shared creation diagnostics; the transport supplies its own idempotency label.
pub fn validate_creation(
    value: &Value,
    key: Option<&str>,
    key_label: &str,
) -> Result<(String, String, String), crate::CloudError> {
    let object = value.as_object().ok_or_else(|| {
        crate::CloudError::new("invalid_request", "creation body must be a JSON object")
    })?;
    let mut errors = Vec::new();
    for field in object.keys() {
        if !["name", "template"].contains(&field.as_str()) {
            errors.push(format!(
                "unknown field {field}; accepted fields: name, template"
            ));
        }
    }
    let name = object.get("name").and_then(Value::as_str).unwrap_or("");
    if name.trim().is_empty() {
        errors.push("name is required and must be a non-empty string".into());
    } else if name.trim().len() > 128
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
        || [".", ".."].contains(&name.trim())
    {
        errors.push("name must be at most 128 bytes, without slashes, control characters, or dot-only paths".into());
    }
    let template = match object.get("template") {
        None => "records-v1",
        Some(v) => v.as_str().unwrap_or(""),
    };
    if template != "records-v1" {
        errors.push("template must be a string: one of records-v1".into());
    }
    let key = key.unwrap_or("");
    if key.trim().is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
        errors.push(format!("{key_label} is required and must be a non-empty string of at most 128 bytes without control characters"));
    }
    if !errors.is_empty() {
        return Err(crate::CloudError::new(
            "invalid_request",
            &format!("{}. Template defaults to records-v1.", errors.join("; ")),
        ));
    }
    Ok((name.into(), template.into(), key.into()))
}

pub const LEGACY_GUIDANCE: &str = "Public signup, workspace sharing, and invitations are unavailable. Ask the operator privately for a management credential to provision, or a single-Bog app credential to use an existing Bog.";
pub fn llms_for_mode(configured: bool) -> String {
    if configured {
        llms()
    } else {
        format!("{}\n\n{}",LEGACY_GUIDANCE,llms().replace("Workspace defaults to personal; pass workspace_id explicitly for teams.","Management credentials default to the legacy workspace. Account workspaces are not activated.").replace("default personal", "default legacy workspace").replace("List current workspace memberships.","List the legacy workspace accessible to management credentials."))
    }
}
pub fn guide(configured: bool, legacy_limit: usize) -> String {
    let replacements = if configured {
        vec![
            ("intro", "Give your scripts, apps, and agents a database of their own. Sign in with GitHub, create a Bog, and let Fold keep your views up to date.".to_owned()),
            ("allowance", "Three small Bogs per workspace · Free to use · HTTP + MCP".into()),
            ("identity_title", "Your GitHub, your workspace.".into()),
            ("identity_body", "Approve a connection. Your personal workspace is created automatically.".into()),
            ("sharing_summary", "Read Fold views, issue an app credential, or invite someone into your workspace.".into()),
            ("connect", "Give your agent this page. Discover the API, approve GitHub sign-in, and create a Bog without putting credentials in a conversation.".into()),
            ("mcp", "connect an OAuth-capable client to <code>https://flower-bog-cloud.fly.dev/mcp</code> and follow its GitHub authorization prompt.".into()),
            ("http", "follow the <a href=\"/auth.md\">device login instructions</a>, then use <a href=\"/v1\">API discovery</a>.".into()),
            ("workspace_default", "Your personal workspace is the default; choose a workspace explicitly when working with a team.".into()),
            ("revocation", "Owners can revoke access in the console. Removing a member also revokes credentials they issued in that workspace.".into()),
            ("sharing", "Owners can invite people with a single-use link that expires after seven days. Members can create Bogs, use shared records, and issue app credentials. Owners manage membership and deletion.".into()),
            ("console_link", "Your Bogs ↗".into()),
        ]
    } else {
        vec![
            ("intro", format!("Create a Bog, store JSON records, and read Fold views through HTTP or MCP. {LEGACY_GUIDANCE}")),
            ("allowance", format!("Token-access preview · Legacy management allowance: {legacy_limit} Bogs · Account signup pending")),
            ("identity_title", "Connect with a credential.".into()),
            ("identity_body", "Obtain a credential privately from the operator. GitHub sign-in and automatic personal workspaces are not activated.".into()),
            ("sharing_summary", "Read Fold views and issue a credential restricted to one Bog for your server-side app.".into()),
            ("connect", LEGACY_GUIDANCE.into()),
            ("mcp", "connect a bearer-capable client to <code>https://flower-bog-cloud.fly.dev/mcp</code> using your privately supplied credential. OAuth signup is unavailable.".into()),
            ("http", "use your privately supplied bearer credential and follow <a href=\"/v1\">API discovery</a>. <a href=\"/auth.md\">Authentication guidance</a> explains current access.".into()),
            ("workspace_default", "Management credentials default to the legacy workspace. The three-Bog account workspace allowance applies when account signup is activated, not to the legacy operator.".into()),
            ("revocation", "Legacy management credentials can list and revoke credentials through HTTP and MCP. The account console is not activated.".into()),
            ("sharing", "Workspace sharing and invitations are unavailable in token-access preview. Use a single-Bog credential for an application's access.".into()),
            ("console_link", "Sign-in status ↗".into()),
        ]
    };
    let mut page = include_str!("../static/index.html").to_owned();
    for (key, value) in replacements {
        page = page.replace(&format!("{{{{{key}}}}}"), &value);
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guide_modes_have_no_unexpanded_fields_or_false_signup_claims() {
        let enabled = guide(true, 5);
        let legacy = guide(false, 5);
        assert!(enabled.contains("Your GitHub, your workspace."));
        assert!(!enabled.contains(LEGACY_GUIDANCE));
        assert!(legacy.contains(LEGACY_GUIDANCE));
        assert!(legacy.contains("allowance: 5 Bogs"));
        assert!(!legacy.contains("Your GitHub, your workspace."));
        for page in [enabled, legacy] {
            assert!(!page.contains("{{"));
            assert!(page.contains("older credentials may have no expiry"));
        }
    }
    #[test]
    fn contract_paths_and_creation_example_are_complete() {
        let document = openapi();
        let paths = document["paths"].as_object().unwrap();
        for (path, methods) in paths {
            for (_, op) in methods.as_object().unwrap() {
                for p in path
                    .split('/')
                    .filter_map(|p| p.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
                {
                    assert!(
                        op["parameters"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|v| v["name"] == p && v["in"] == "path" && v["required"] == true),
                        "{path}: {p}"
                    );
                }
            }
        }
        let schema = &document["paths"]["/v1/bogs"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"];
        let example = &overview()["create_example"]["body"];
        for field in schema["required"].as_array().unwrap() {
            assert!(example.get(field.as_str().unwrap()).is_some());
        }
        assert_eq!(schema["properties"]["template"]["default"], "records-v1");
        for (path, method) in [
            ("/v1/bogs/{bog_id}/usage", "get"),
            ("/v1/bogs/{bog_id}", "delete"),
            ("/v1/bogs/{bog_id}/schema", "get"),
        ] {
            assert!(
                document["paths"][path][method]["parameters"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == "workspace_id" && p["in"] == "query")
            );
        }
    }
}
