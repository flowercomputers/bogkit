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
struct Create {
    name: String,
    template: String,
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
    Tool::new(name, description, schema::<T>())
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
    serde_json::from_value(value)
        .map_err(|_| ErrorData::invalid_params("arguments do not match the tool schema", None))
}
fn id(value: String) -> Result<BogId, ErrorData> {
    uuid::Uuid::parse_str(&value)
        .map(BogId)
        .map_err(|_| ErrorData::invalid_params("bog_id must be a UUID", None))
}
fn operation(name: &str, args: Value) -> Result<Operation, ErrorData> {
    Ok(match name {
        "create_bog" => {
            let a: Create = parse(args)?;
            Operation::CreateBog {
                name: a.name,
                template: a.template,
                idempotency_key: a.idempotency_key,
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
fn tool_error(error: CloudError) -> CallToolResult {
    CallToolResult::structured_error(json!({"error":{"code":error.code,"message":error.message}}))
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
        // rmcp injects the current HTTP request Parts, including middleware extensions,
        // on EVERY request. Never retain a Principal in Handler or session state.
        let principal = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<Principal>())
            .ok_or_else(|| ErrorData::internal_error("request authentication unavailable", None))?;
        let op = operation(
            &request.name,
            Value::Object(request.arguments.unwrap_or_default()),
        )?;
        let describe = match &op {
            Operation::DescribeBog { bog_id } => Some(*bog_id),
            _ => None,
        };
        let result = async {
            let mut result = self.0.execute(principal, op).await?;
            if let Some(bog_id) = describe
                && result.body["status"] == "ready"
            {
                let schema = self
                    .0
                    .execute(principal, Operation::Schema { bog_id })
                    .await?;
                result.body["schema"] = schema.body;
            }
            Ok::<_, CloudError>(result)
        }
        .await;
        Ok(match result {
            Ok(result) => {
                let value = json!({"status":result.status,"data":result.body});
                // Structured content is duplicated as text by the SDK; bound the whole result.
                let response = CallToolResult::structured(value);
                if serde_json::to_vec(&response).map_or(true, |b| b.len() > MAX_RESULT_BYTES) {
                    tool_error(CloudError::new(
                        "result_too_large",
                        "result exceeds 1 MiB; request a smaller page",
                    ))
                } else {
                    response
                }
            }
            Err(error) => tool_error(error),
        }
        .into())
    }
}
