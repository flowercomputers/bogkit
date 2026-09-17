//! Small read-only context resources using the same authorized operations as tools.
use bog_cloud::{CloudService, Operation, Principal};
use rmcp::{ErrorData, model::*};
use serde_json::json;
pub(crate) fn definitions() -> Vec<Resource> {
    [
        ("quick-start", "Quick start"),
        ("templates", "Templates"),
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
    let operation = match uri {
        "bog://guide/templates" => Operation::ListTemplates,
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
