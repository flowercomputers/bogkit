//! Typed MCP arguments. Identity is deliberately absent from every schema.
use bog_cloud::{BogId, CloudError, CloudService, Operation, Principal};
use rmcp::{ErrorData, RoleServer, ServerHandler, model::*, service::RequestContext};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // Schema-only: shared validation retains aggregate diagnostics.
struct Create {
    name: String,
    template: Option<String>,
    idempotency_key: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ValidateDefinition {
    #[schemars(schema_with = "object_value_schema")]
    definition: Value,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateDefined {
    name: String,
    #[schemars(schema_with = "object_value_schema")]
    definition: Value,
    idempotency_key: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResourceOperation {
    bog_id: String,
    resource: String,
    #[schemars(schema_with = "object_value_schema")]
    query: Value,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DefinitionUpdate {
    bog_id: String,
    #[schemars(schema_with = "object_value_schema")]
    definition: Value,
    expected_revision: u64,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DefinitionUpdateStatus {
    bog_id: String,
    job_id: String,
}
fn object_value_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    serde_json::from_value(json!({"type":"object"})).expect("object value schema")
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Describe {
    bog_id: String,
}
fn default_metrics_window() -> String {
    "1h".into()
}
fn default_events_limit() -> usize {
    50
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Metrics {
    bog_id: String,
    #[serde(default = "default_metrics_window")]
    window: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Events {
    bog_id: String,
    cursor: Option<String>,
    #[serde(default = "default_events_limit")]
    limit: usize,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Record {
    bog_id: String,
    key: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Upsert {
    bog_id: String,
    key: String,
    data: Map<String, Value>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct View {
    bog_id: String,
    view: String,
    limit: Option<usize>,
    offset: Option<usize>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Mutation {
    Upsert {
        key: String,
        data: Map<String, Value>,
    },
    Remove {
        key: String,
    },
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Batch {
    bog_id: String,
    #[serde(alias = "operations")]
    ops: Vec<Mutation>,
}

fn schema<T: JsonSchema>() -> Arc<Map<String, Value>> {
    Arc::new(
        serde_json::to_value(schemars::schema_for!(T))
            .expect("schema")
            .as_object()
            .expect("object schema")
            .clone(),
    )
}
fn definition<T: JsonSchema>(
    name: &'static str,
    description: &'static str,
    read: bool,
    destructive: bool,
    idempotent: bool,
) -> Tool {
    let mut input = (*schema::<T>()).clone();
    if let Some(properties) = input
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    {
        properties.insert("workspace_id".into(), json!({"type":"string","format":"uuid","description":"Explicit workspace selection; defaults to personal workspace for accounts or legacy workspace for legacy management credentials."}));
    }
    if name == "batch" {
        input["properties"]["operations"] = input["properties"]["ops"].clone();
        input["properties"]["operations"]["deprecated"] = json!(true);
        input.insert("required".into(), json!(["bog_id"]));
        input.insert(
            "oneOf".into(),
            json!([{ "required": ["ops"] }, { "required": ["operations"] }]),
        );
    }
    if name == "bog_metrics" {
        input["properties"]["window"] = json!({"type":"string","enum":["5m","1h"],"default":"1h"});
    }
    if name == "bog_events" {
        input["properties"]["limit"] =
            json!({"type":"integer","minimum":1,"maximum":100,"default":50});
    }
    if matches!(name, "plan_definition_update" | "apply_definition_update") {
        input["properties"]["expected_revision"] = json!({
            "type":"integer",
            "minimum":1,
            "maximum":9_007_199_254_740_991_u64,
            "description":"Active definition revision. JavaScript-safe positive integer."
        });
    }
    if name == "wait_for_change" {
        // serde aliases are accepted at runtime but omitted by schemars.
        // Advertise both spellings so strict schema clients can use the alias.
        let properties = input["properties"]
            .as_object_mut()
            .expect("object properties");
        properties.insert("timeout".into(),json!({"type":"integer","minimum":0,"maximum":25,"default":25,"description":"Maximum seconds to wait; omit for 25 seconds."}));
        properties.insert("timeout_seconds".into(),json!({"type":"integer","minimum":0,"maximum":25,"deprecated":true,"description":"Legacy alias for timeout. Do not supply both spellings."}));
        input.insert(
            "not".into(),
            json!({"required":["timeout","timeout_seconds"]}),
        );
    }
    let description = match name {
        "create_bog" => {
            "Create a workspace database. name and idempotency_key are required strings; template defaults to records-v1."
        }
        "wait_for_change" => description,
        _ => bog_cloud::contract::description(name).unwrap_or(description),
    };
    Tool::new(name, description, Arc::new(input))
        .with_raw_output_schema(Arc::new(crate::output::schema(name)))
        .with_annotations(
            ToolAnnotations::new()
                .read_only(read)
                .destructive(destructive)
                .idempotent(idempotent)
                .open_world(false),
        )
}

pub fn definitions() -> Vec<Tool> {
    vec![
        definition::<Empty>(
            "discover_capabilities",
            "Discover enabled state, the full definition schema, executable examples, trusted components and effective limits before authoring a Bog definition.",
            true,
            false,
            true,
        ),
        definition::<ValidateDefinition>(
            "validate_definition",
            "Validate and normalize a Bog definition without creating or changing a Bog.",
            true,
            false,
            true,
        ),
        definition::<CreateDefined>(
            "create_bog_from_definition",
            "Create a Bog from a validated JSON definition. Reuse the same idempotency key and identical body to retry safely.",
            false,
            false,
            true,
        ),
        definition::<Describe>(
            "describe_definition",
            "Management authority required. Read the active normalized definition, digest and revision for an accessible Bog.",
            true,
            false,
            true,
        ),
        definition::<Describe>(
            "list_resources",
            "List the public resources exposed by a Bog definition. Private resources are never returned.",
            true,
            false,
            true,
        ),
        definition::<ResourceOperation>(
            "query_resource",
            "Use list_resources first for operation request/response schemas. Query a public resource by resource name; query is an object with action get, list, read or top and the action parameters.",
            true,
            false,
            true,
        ),
        definition::<ResourceOperation>(
            "search_resource",
            "Use list_resources first. Search a public BM25 or semantic resource by resource name; query is an object with query text, optional limit and offset. Respect discover_capabilities effective limits.",
            true,
            false,
            true,
        ),
        definition::<DefinitionUpdate>(
            "plan_definition_update",
            "Validate and plan an additive definition update against the expected active revision without changing the Bog.",
            true,
            false,
            true,
        ),
        definition::<DefinitionUpdate>(
            "apply_definition_update",
            "Management authority required. Start an additive definition update against expected_revision. Poll definition_update_status until succeeded or failed; acceptance is not activation. Writes may return writes_paused during rebuild.",
            false,
            false,
            false,
        ),
        definition::<DefinitionUpdateStatus>(
            "definition_update_status",
            "Read durable progress or the terminal result of one definition update job.",
            true,
            false,
            true,
        ),
        definition::<Metrics>(
            "bog_metrics",
            "Read bounded operational metrics without waking the Bog. App credentials see only their own request metrics.",
            true,
            false,
            true,
        ),
        definition::<Events>(
            "bog_events",
            "Read bounded lifecycle and credential events without waking the Bog. Requires workspace management access.",
            true,
            false,
            true,
        ),
        definition::<Empty>(
            "list_templates",
            "List available template IDs and defaults before creating a Bog.",
            true,
            false,
            true,
        ),
        definition::<Empty>(
            "get_current_context",
            "Identify the caller, default workspace, memberships and current workspace Bog allowances. Choose shared workspaces explicitly.",
            true,
            false,
            true,
        ),
        definition::<Prepare>(
            "prepare_app_access",
            "Prepare a ten-minute private installation of a single-Bog credential. Returns only a handoff reference; install through the local helper or an explicit console download. No secret is returned or minted until redemption.",
            false,
            false,
            false,
        ),
        definition::<Wait>(
            "wait_for_change",
            "Wait up to 25 seconds for a change using timeout; timeout_seconds is a legacy alias (do not supply both). Omit cursor to get current position. Reset requires refetch; no event replay.",
            true,
            false,
            true,
        ),
        definition::<Empty>("list_workspaces", "List workspaces.", true, false, true),
        definition::<Describe>(
            "list_tokens",
            "List credential metadata.",
            true,
            false,
            true,
        ),
        definition::<Issue>(
            "issue_token",
            "Compatibility operation: returns a secret in the tool result. Prefer prepare_app_access to install a credential privately without exposing it in the conversation.",
            false,
            false,
            false,
        ),
        definition::<Revoke>("revoke_token", "Revoke a credential.", false, true, true),
        definition::<Create>(
            "create_bog",
            "Create records-v1 database; reuse the same required idempotency key and body to retry safely.",
            false,
            false,
            true,
        ),
        definition::<Empty>(
            "list_bogs",
            "List Bogs in your personal workspace, or supply workspace_id explicitly for a shared workspace.",
            true,
            false,
            true,
        ),
        definition::<Describe>(
            "describe_bog",
            "Read database status and, when ready, actual template schema.",
            true,
            false,
            true,
        ),
        definition::<Record>("get_record", "Read one JSON record.", true, false, true),
        definition::<Upsert>(
            "upsert_record",
            "Replace the entire JSON object at a record key.",
            false,
            true,
            false,
        ),
        definition::<Record>("delete_record", "Delete one record.", false, true, false),
        definition::<View>(
            "read_view",
            "Read docs or total; limit defaults to 100, maximum 1000; offset maximum 10000. Reduce page size if output is too large.",
            true,
            false,
            true,
        ),
        definition::<Batch>(
            "batch",
            "Atomically apply up to 100 upsert/remove operations in one database.",
            false,
            true,
            false,
        ),
    ]
}
fn parse<T: DeserializeOwned>(value: Value) -> Result<T, ErrorData> {
    serde_json::from_value(value).map_err(|e| {
        ErrorData::invalid_params(format!("arguments do not match the tool schema: {e}"), None)
    })
}
fn id(value: String) -> Result<BogId, ErrorData> {
    uuid::Uuid::parse_str(&value)
        .map(BogId)
        .map_err(|_| ErrorData::invalid_params("bog_id must be a UUID", None))
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Wait {
    bog_id: String,
    cursor: Option<String>,
    #[serde(alias = "timeout_seconds")]
    timeout: Option<u64>,
}
#[derive(JsonSchema)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum AppScope {
    Read,
    Write,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Issue {
    bog_id: String,
    #[schemars(with = "AppScope")]
    scope: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Revoke {
    bog_id: String,
    token_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Prepare {
    bog_id: String,
    #[schemars(with = "AppScope")]
    scope: String,
    label: String,
}
pub(crate) fn operation(name: &str, args: Value) -> Result<Operation, ErrorData> {
    Ok(match name {
        "discover_capabilities" => {
            let _: Empty = parse(args)?;
            Operation::ListComponents
        }
        "validate_definition" => {
            let a: ValidateDefinition = parse(args)?;
            Operation::ValidateDefinition {
                definition: a.definition,
            }
        }
        "create_bog_from_definition" => {
            let a: CreateDefined = parse(args)?;
            Operation::CreateDefinedBog {
                name: a.name,
                definition: a.definition,
                idempotency_key: a.idempotency_key,
            }
        }
        "describe_definition" => {
            let a: Describe = parse(args)?;
            Operation::DescribeDefinition {
                bog_id: id(a.bog_id)?,
            }
        }
        "list_resources" => {
            let a: Describe = parse(args)?;
            Operation::ListResources {
                bog_id: id(a.bog_id)?,
            }
        }
        "query_resource" | "search_resource" => {
            let a: ResourceOperation = parse(args)?;
            let bog_id = id(a.bog_id)?;
            if name == "query_resource" {
                Operation::QueryResource {
                    bog_id,
                    resource: a.resource,
                    query: a.query,
                }
            } else {
                Operation::SearchResource {
                    bog_id,
                    resource: a.resource,
                    query: a.query,
                }
            }
        }
        "plan_definition_update" | "apply_definition_update" => {
            let a: DefinitionUpdate = parse(args)?;
            if a.expected_revision == 0 || a.expected_revision > 9_007_199_254_740_991 {
                return Err(ErrorData::invalid_params(
                    "expected_revision must be a JavaScript-safe positive integer",
                    None,
                ));
            }
            let bog_id = id(a.bog_id)?;
            if name == "plan_definition_update" {
                Operation::PlanDefinitionUpdate {
                    bog_id,
                    definition: a.definition,
                    expected_revision: a.expected_revision,
                }
            } else {
                Operation::ApplyDefinitionUpdate {
                    bog_id,
                    definition: a.definition,
                    expected_revision: a.expected_revision,
                }
            }
        }
        "definition_update_status" => {
            let a: DefinitionUpdateStatus = parse(args)?;
            Operation::DefinitionUpdateStatus {
                bog_id: id(a.bog_id)?,
                job_id: a.job_id,
            }
        }
        "bog_metrics" => {
            let a: Metrics = parse(args)?;
            let window = a.window;
            if !matches!(window.as_str(), "5m" | "1h") {
                return Err(ErrorData::invalid_params("window must be 5m or 1h", None));
            }
            Operation::BogMetrics {
                bog_id: id(a.bog_id)?,
                window,
            }
        }
        "bog_events" => {
            let a: Events = parse(args)?;
            let limit = a.limit;
            if !(1..=100).contains(&limit) {
                return Err(ErrorData::invalid_params(
                    "limit must be 1 through 100",
                    None,
                ));
            }
            Operation::BogEvents {
                bog_id: id(a.bog_id)?,
                cursor: a.cursor,
                limit,
            }
        }

        "list_templates" => {
            let _: Empty = parse(args)?;
            Operation::ListTemplates
        }
        "get_current_context" => {
            let _: Empty = parse(args)?;
            Operation::GetCurrentContext
        }
        "prepare_app_access" => {
            let a: Prepare = parse(args)?;
            let scope = match a.scope.as_str() {
                "read" => bog_cloud::Scope::Read,
                "write" => bog_cloud::Scope::Write,
                _ => {
                    return Err(ErrorData::invalid_params(
                        "scope must be read or write",
                        None,
                    ));
                }
            };
            Operation::PrepareAppAccess {
                bog_id: id(a.bog_id)?,
                scope,
                label: a.label,
            }
        }
        "wait_for_change" => {
            let a: Wait = parse(args)?;
            Operation::WaitForChange {
                bog_id: id(a.bog_id)?,
                cursor: a.cursor,
                timeout_seconds: a.timeout.unwrap_or(25),
            }
        }
        "list_workspaces" => {
            let _: Empty = parse(args)?;
            Operation::ListWorkspaces
        }
        "list_tokens" => {
            let a: Describe = parse(args)?;
            Operation::ListTokens {
                bog_id: id(a.bog_id)?,
            }
        }
        "issue_token" => {
            let a: Issue = parse(args)?;
            Operation::IssueToken {
                bog_id: id(a.bog_id)?,
                scope: match a.scope.as_str() {
                    "read" => bog_cloud::Scope::Read,
                    "write" => bog_cloud::Scope::Write,
                    _ => {
                        return Err(ErrorData::invalid_params(
                            "scope must be read or write",
                            None,
                        ));
                    }
                },
            }
        }
        "revoke_token" => {
            let a: Revoke = parse(args)?;
            Operation::RevokeToken {
                bog_id: id(a.bog_id)?,
                token_id: a.token_id,
            }
        }
        "create_bog" => {
            let mut args = args.as_object().cloned().ok_or_else(|| {
                ErrorData::invalid_params("creation arguments must be an object", None)
            })?;
            let key = args.remove("idempotency_key");
            let (name, template, idempotency_key) = bog_cloud::contract::validate_creation(
                &Value::Object(args),
                key.as_ref().and_then(Value::as_str),
                "idempotency_key",
            )
            .map_err(|e| ErrorData::invalid_params(e.message, None))?;
            Operation::CreateBog {
                name,
                template,
                idempotency_key,
            }
        }
        "list_bogs" => {
            let _: Empty = parse(args)?;
            Operation::ListBogs
        }
        "describe_bog" => {
            let a: Describe = parse(args)?;
            Operation::DescribeBog {
                bog_id: id(a.bog_id)?,
            }
        }
        "get_record" | "delete_record" => {
            let a: Record = parse(args)?;
            let bog_id = id(a.bog_id)?;
            if name == "get_record" {
                Operation::GetRecord { bog_id, key: a.key }
            } else {
                Operation::DeleteRecord { bog_id, key: a.key }
            }
        }
        "upsert_record" => {
            let a: Upsert = parse(args)?;
            Operation::UpsertRecord {
                bog_id: id(a.bog_id)?,
                key: a.key,
                data: Value::Object(a.data),
            }
        }
        "read_view" => {
            let a: View = parse(args)?;
            Operation::ReadView {
                bog_id: id(a.bog_id)?,
                view: a.view,
                limit: a.limit,
                offset: a.offset,
            }
        }
        "batch" => {
            let a: Batch = parse(args)?;
            Operation::Batch {
                bog_id: id(a.bog_id)?,
                operations: serde_json::to_value(a.ops).expect("serializable mutations"),
            }
        }
        _ => return Err(ErrorData::invalid_params("unknown tool", None)),
    })
}
const MAX_RESULT_BYTES: usize = 1024 * 1024;
fn tool_error(error: CloudError, request_id: &str) -> CallToolResult {
    let next = error.next_action();
    CallToolResult::structured_error(
        json!({"error":{"code":error.code,"message":error.message,"next_action":next},"request_id":request_id}),
    )
}
#[derive(Clone)]
pub(crate) struct Handler(pub Arc<CloudService>);
impl ServerHandler for Handler {
    // The SDK knows newer inline-discovery revisions, but this service's
    // verified protocol contract ends at 2025-11-25. Bound both advertisement
    // and negotiation rather than inheriting every SDK-known revision.
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25))
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("bog-cloud", env!("CARGO_PKG_VERSION")))
        .with_instructions(bog_cloud::contract::AGENT_INSTRUCTIONS)
    }
    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(
            crate::resources::definitions(),
        ))
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let parts = context.extensions.get::<axum::http::request::Parts>();
        let request_id = parts
            .and_then(|p| p.extensions.get::<crate::transport::RequestId>())
            .map(|id| id.0.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let principal = parts
            .and_then(|p| p.extensions.get::<Principal>())
            .ok_or_else(|| {
                ErrorData::internal_error(
                    "request authentication unavailable",
                    Some(json!({"request_id":request_id})),
                )
            })?;
        crate::resources::read(&self.0, principal, &request.uri, &request_id).await
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(definitions()))
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<crate::transport::RequestId>())
            .map(|id| id.0.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let result = async {
            // rmcp injects the current HTTP request Parts, including middleware extensions,
            // on EVERY request. Never retain a Principal in Handler or session state.
            let principal = context
                .extensions
                .get::<axum::http::request::Parts>()
                .and_then(|p| p.extensions.get::<Principal>())
                .ok_or_else(|| {
                    ErrorData::internal_error("request authentication unavailable", None)
                })?;
            if principal.kind() == bog_cloud::PrincipalKind::App && matches!(request.name.as_ref(), "create_bog" | "create_bog_from_definition" | "issue_token" | "prepare_app_access" | "revoke_token" | "list_tokens" | "validate_definition" | "describe_definition" | "plan_definition_update" | "apply_definition_update" | "definition_update_status") {
                return Ok(tool_error(CloudError::new("forbidden", "app credentials cannot provision Bogs or manage credentials; use an authorized management connection"), &request_id).into());
            }
            let mut args = request.arguments.unwrap_or_default();
            let principal = if let Some(workspace) = args.remove("workspace_id") {
                let workspace = workspace
                    .as_str()
                    .and_then(|w| uuid::Uuid::parse_str(w).ok())
                    .map(bog_cloud::WorkspaceId)
                    .ok_or_else(|| {
                        ErrorData::invalid_params("workspace_id must be a UUID", None)
                    })?;
                match self.0.auth.select_workspace(principal, workspace) {
                    Ok(p) => p,
                    Err(e) => return Ok(tool_error(e, &request_id).into()),
                }
            } else {
                principal.clone()
            };
            let op = operation(&request.name, Value::Object(args))?;
            let describe = match &op {
                Operation::DescribeBog { bog_id } => Some(*bog_id),
                _ => None,
            };
            let result = async {
                let mut result = self
                    .0
                    .execute_with_request_id(&principal, op, &request_id)
                    .await?;
                if let Some(bog_id) = describe
                    && result.body["status"] == "ready"
                {
                    let schema = self
                        .0
                        .execute_with_request_id(
                            &principal,
                            Operation::Schema { bog_id },
                            &request_id,
                        )
                        .await?;
                    result.body["schema"] = schema.body;
                }
                Ok::<_, CloudError>(result)
            }
            .await;
            Ok(match result {
                Ok(result) => {
                    let value =
                        json!({"status":result.status,"data":result.body,"request_id":request_id});
                    // Structured content is duplicated as text by the SDK; bound the whole result.
                    let response = CallToolResult::structured(value);
                    if serde_json::to_vec(&response).map_or(true, |b| b.len() > MAX_RESULT_BYTES) {
                        tool_error(
                            CloudError::new(
                                "result_too_large",
                                "result exceeds 1 MiB; request a smaller page",
                            ),
                            &request_id,
                        )
                    } else {
                        response
                    }
                }
                Err(error) => tool_error(error, &request_id),
            }
            .into())
        }
        .await;
        result.map_err(|mut error: ErrorData| {
            error.data = Some(json!({"request_id":request_id}));
            error
        })
    }
}
