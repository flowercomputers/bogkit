//! Small read-only context resources using the same authorized operations as tools.
use bog_cloud::{CloudService, Operation, Principal};
use rmcp::{ErrorData, model::*};
use serde_json::json;
pub(crate) const INSPECTOR_URI: &str = "ui://bog-cloud/resource-inspector";
pub(crate) const INSPECTOR_MIME: &str = "text/html;profile=mcp-app";

pub(crate) fn definitions() -> Vec<Resource> {
    [
        ("quick-start", "Quick start"),
        ("templates", "Templates"),
        (
            "components",
            "Definition schema, examples and effective limits",
        ),
        ("access-rules", "Access rules"),
        ("allowances", "Your workspace allowances"),
    ]
    .into_iter()
    .map(|(slug, title)| {
        Resource::new(format!("bog://guide/{slug}"), title).with_mime_type("application/json")
    })
    .collect()
}
pub(crate) async fn read(
    service: &CloudService,
    principal: &Principal,
    uri: &str,
    request_id: &str,
) -> Result<ReadResourceResponse, ErrorData> {
    if uri == INSPECTOR_URI {
        // Static, credential-free document. Auth middleware still protects the
        // resource request; subsequent reads use ordinary authorized tool calls.
        let html = include_str!("../../cloud/static/inspector.html").replace(
            "/* BOG_INSPECTOR_SCRIPT */",
            include_str!("../../cloud/static/inspector.js"),
        );
        return Ok(ReadResourceResult::new(vec![ResourceContents::text(html, uri)
            .with_mime_type(INSPECTOR_MIME)
            .with_meta(MetaObject(json!({"ui":{"csp":{"connectDomains":[],"resourceDomains":[],"frameDomains":[]},"permissions":{},"prefersBorder":true}}).as_object().unwrap().clone()))]).into());
    }
    if uri == "bog://guide/allowances" {
        // App context is intentionally readable, but workspace allowances remain
        // management-only even when that context carries no quota information.
        service
            .auth
            .authorize(principal, None, false)
            .map_err(|e| {
                ErrorData::invalid_request(
                    e.message,
                    Some(json!({"code":e.code,"request_id":request_id})),
                )
            })?;
    }
    let operation = match uri {
        "bog://guide/templates" => Operation::ListTemplates,
        "bog://guide/components" => Operation::ListComponents,
        "bog://guide/quick-start" | "bog://guide/access-rules" | "bog://guide/allowances" => {
            Operation::GetCurrentContext
        }
        _ => {
            return Err(ErrorData::resource_not_found(
                "Unknown Bog resource; use resources/list.",
                Some(json!({"request_id":request_id})),
            ));
        }
    };
    let result = service.execute(principal, operation).await.map_err(|e| {
        ErrorData::invalid_request(
            e.message,
            Some(json!({"code":e.code,"request_id":request_id})),
        )
    })?;
    let content = match uri {
        "bog://guide/quick-start" => {
            json!({"instructions":bog_cloud::contract::AGENT_INSTRUCTIONS,"context":result.body})
        }
        "bog://guide/access-rules" => {
            json!({"instructions":bog_cloud::contract::ACCESS_RULES,"context":result.body})
        }
        _ => result.body,
    };
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(
            json!({"data":content,"request_id":request_id}).to_string(),
            uri,
        )
        .with_mime_type("application/json"),
    ])
    .into())
}
