//! Operation-specific success contracts; record payloads remain arbitrary JSON objects.
use serde_json::{Value, json};
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":true})
}
pub(crate) fn schema(name: &str) -> serde_json::Map<String, Value> {
    let string = json!({"type":"string"});
    let integer = json!({"type":"integer"});
    let boolean = json!({"type":"boolean"});
    let nullable_integer = json!({"type":["integer","null"]});
    let bog = object(
        json!({"id":string,"name":string,"template":string,"template_version":string,"status":string,"desired_state":string,"generation":integer,"failure_code":{"type":["string","null"]},"created_at":integer,"schema":{"type":"object"},"api_url":string}),
        &["id", "name", "template", "status", "generation"],
    );
    let workspace = object(
        json!({"id":string,"name":string,"role":string,"personal":boolean,"uncapped_bogs":boolean,"bog_limit":nullable_integer}),
        &["id", "name", "role", "personal", "bog_limit"],
    );
    let workspaces = json!({"type":"array","items":workspace});
    let body = match name {
        "discover_capabilities" => object(
            json!({"enabled":boolean,"version":integer,"definition_schema":{"type":"object"},"examples":{"type":"object","description":"Complete validated definitions: todo, todo_search and todo_semantic."},"stages":{"type":"array","items":string},"terminals":{"type":"array","items":string},"input":{"type":"object"},"limits":{"type":"object"},"bm25":{"type":"object"},"semantic":{"type":"object"}}),
            &[
                "enabled",
                "version",
                "definition_schema",
                "stages",
                "terminals",
                "input",
                "limits",
                "bm25",
                "semantic",
            ],
        ),
        "validate_definition" => object(
            json!({"valid":boolean,"definition":{"type":"object"},"digest":string,"operations":{"type":"array","items":{"type":"object"}}}),
            &["valid", "definition", "digest", "operations"],
        ),
        "create_bog_from_definition" => bog.clone(),
        "describe_definition" => object(
            json!({"definition":{"type":"object"},"digest":string,"revision":integer}),
            &["definition", "digest", "revision"],
        ),
        "list_resources" => object(
            json!({"resources":{"type":"array","items":object(json!({"name":string,"stages":{"type":"array","items":{"type":"object"}},"terminal":{"type":"object"},"operations":{"type":"array","items":{"type":"object"}}}), &["name","stages","terminal","operations"])},"revision":integer,"digest":string}),
            &["resources", "revision", "digest"],
        ),
        "query_resource" | "search_resource" => object(
            json!({"seq":integer,"data":{"description":"Result shape depends on the exposed action; list_resources returns its response_schema."}}),
            &["seq", "data"],
        ),
        "plan_definition_update" => object(
            json!({"compatible":boolean,"expected_revision":integer,"target_digest":string,"requires_rebuild":boolean}),
            &[
                "compatible",
                "expected_revision",
                "target_digest",
                "requires_rebuild",
            ],
        ),
        "apply_definition_update" => object(
            json!({"job_id":string,"bog_id":string,"status":string}),
            &["job_id", "bog_id", "status"],
        ),
        "definition_update_status" => object(
            json!({"job_id":string,"bog_id":string,"status":string,"error":{"type":["string","null"]},"expected_revision":integer,"definition":{"type":"object"},"target_digest":string,"revision":nullable_integer}),
            &["job_id", "bog_id", "status"],
        ),
        "bog_metrics" => bog_cloud::contract::metrics_schema(),
        "bog_events" => bog_cloud::contract::events_schema(),
        "create_bog" | "describe_bog" => bog.clone(),
        "list_bogs" => object(json!({"bogs":{"type":"array","items":bog}}), &["bogs"]),
        "list_workspaces" => object(json!({"workspaces":workspaces}), &["workspaces"]),
        "get_current_context" => object(
            json!({"kind":string,"account":{"type":["object","null"]},"workspace_id":{"type":["string","null"]},"platform_operator":boolean,"workspaces":workspaces}),
            &["kind", "account", "workspace_id", "workspaces"],
        ),
        "list_templates" => object(
            json!({"templates":{"type":"array","items":object(json!({"id":string,"default":boolean,"description":string}), &["id","default","description"])}}),
            &["templates"],
        ),
        "get_record" => object(
            json!({"seq":integer,"data":{"type":"object"}}),
            &["seq", "data"],
        ),
        "read_view" => object(
            json!({"seq":integer,"data":{"oneOf":[object(json!({"value":integer}), &["value"]),{"type":"array","items":object(json!({"key":string,"value":{"type":"object"}}), &["key","value"])}]}}),
            &["seq", "data"],
        ),
        "upsert_record" => object(
            json!({"seq":integer,"replaced":boolean}),
            &["seq", "replaced"],
        ),
        "delete_record" => object(
            json!({"seq":integer,"removed":boolean}),
            &["seq", "removed"],
        ),
        "batch" => object(
            json!({"seq":integer,"applied":integer}),
            &["seq", "applied"],
        ),
        "wait_for_change" => object(
            json!({"seq":integer,"changed":boolean,"reset":boolean,"cursor":string}),
            &["seq", "changed", "reset", "cursor"],
        ),
        "issue_token" => object(
            json!({"id":string,"token":string,"scope":{"enum":["read","write"]}}),
            &["id", "token", "scope"],
        ),
        "list_tokens" => object(
            json!({"tokens":{"type":"array","items":object(json!({"id":string,"bog_id":string,"scope":string,"label":{"type":["string","null"]},"created_at":integer,"expires_at":nullable_integer,"revoked_at":nullable_integer,"last_used_at":{"type":["integer","null"],"description":"Last observed credential use as Unix seconds, rounded down to a whole minute; null when unknown."}}), &["id","bog_id","scope","created_at","expires_at","revoked_at","last_used_at"])}}),
            &["tokens"],
        ),
        "revoke_token" => json!({"type":"null"}),
        "prepare_app_access" => object(
            json!({"handoff_id":string,"bog_id":string,"workspace_id":string,"expires_at":integer,"scope":{"enum":["read","write"]},"label":string,"redeem_path":string,"console_path":string,"installation":string}),
            &[
                "handoff_id",
                "bog_id",
                "workspace_id",
                "expires_at",
                "scope",
                "label",
                "redeem_path",
                "console_path",
            ],
        ),
        _ => panic!("missing output schema for {name}"),
    };
    object(json!({"status":{"type":"integer","minimum":200,"maximum":299},"data":body,"request_id":string}), &["status","data","request_id"]).as_object().unwrap().clone()
}
