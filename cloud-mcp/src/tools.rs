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
struct Describe {
    bog_id: String,
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
    operations: Vec<Mutation>,
}
#[derive(Serialize, JsonSchema)]
struct Success {
    status: u16,
    data: Value,
    request_id: String,
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
    let description = match name {
        "create_bog" => {
            "Create a workspace database. name and idempotency_key are required strings; template defaults to records-v1."
        }
        "wait_for_change" => description,
        _ => bog_cloud::contract::description(name).unwrap_or(description),
    };
    Tool::new(name, description, Arc::new(input))
        .with_raw_output_schema(schema::<Success>())
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
            "Issue a single Bog credential.",
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
            "List databases (owner only).",
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
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Issue {
    bog_id: String,
    scope: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Revoke {
    bog_id: String,
    token_id: String,
}
fn operation(name: &str, args: Value) -> Result<Operation, ErrorData> {
    Ok(match name {
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
                operations: serde_json::to_value(a.operations).expect("serializable mutations"),
            }
        }
        _ => return Err(ErrorData::invalid_params("unknown tool", None)),
    })
}
const MAX_RESULT_BYTES: usize = 1024 * 1024;
fn tool_error(error: CloudError, request_id: &str) -> CallToolResult {
    CallToolResult::structured_error(
        json!({"error":{"code":error.code,"message":error.message},"request_id":request_id}),
    )
}
#[derive(Clone)]
pub(crate) struct Handler(pub Arc<CloudService>);
impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("bog-cloud", env!("CARGO_PKG_VERSION")))
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
                let mut result = self.0.execute(&principal, op).await?;
                if let Some(bog_id) = describe
                    && result.body["status"] == "ready"
                {
                    let schema = self
                        .0
                        .execute(&principal, Operation::Schema { bog_id })
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
