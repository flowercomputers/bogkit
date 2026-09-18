use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::{Value, json};
use tower::ServiceExt;

// A deliberately small schema evaluator for the subset exercised by real worker responses.
fn validate(document: &Value, schema: &Value, value: &Value) {
    let document = if schema.get("$id").is_some() {
        schema
    } else {
        document
    };
    if let Some(reference) = schema["$ref"].as_str() {
        return validate(
            document,
            document
                .pointer(reference.strip_prefix('#').unwrap())
                .expect("unresolved reference"),
            value,
        );
    }
    if let Some(options) = schema["oneOf"].as_array() {
        assert_eq!(
            options
                .iter()
                .filter(|s| std::panic::catch_unwind(|| validate(document, s, value)).is_ok())
                .count(),
            1,
            "oneOf: {value}"
        );
    }
    if let Some(kind) = schema["type"].as_str() {
        assert!(
            match kind {
                "object" => value.is_object(),
                "array" => value.is_array(),
                "integer" => value.is_i64() || value.is_u64(),
                "string" => value.is_string(),
                "boolean" => value.is_boolean(),
                "null" => value.is_null(),
                _ => panic!("unsupported type"),
            },
            "expected {kind}, got {value}"
        );
    }
    if let Some(required) = schema["required"].as_array() {
        for key in required {
            assert!(
                value.get(key.as_str().unwrap()).is_some(),
                "missing {key} in {value}"
            );
        }
    }
    if let Some(properties) = schema["properties"].as_object() {
        for (key, s) in properties {
            if let Some(v) = value.get(key) {
                validate(document, s, v);
            }
        }
    }
    if let Some(items) = value.as_array() {
        for item in items {
            validate(document, &schema["items"], item);
        }
    }
    if let Some(constant) = schema.get("const") {
        assert_eq!(constant, value);
    }
    if let Some(options) = schema["enum"].as_array() {
        assert!(options.contains(value));
    }
}
fn refs(document: &Value, value: &Value) {
    // A nested JSON Schema resource establishes its own fragment resolution scope.
    let document = if value.get("$id").is_some() {
        value
    } else {
        document
    };
    if let Some(r) = value.get("$ref").and_then(Value::as_str) {
        assert!(
            document.pointer(r.strip_prefix('#').unwrap()).is_some(),
            "{r}"
        );
    }
    match value {
        Value::Object(o) => {
            for v in o.values() {
                refs(document, v)
            }
        }
        Value::Array(a) => {
            for v in a {
                refs(document, v)
            }
        }
        _ => {}
    }
}
#[test]
fn operations_have_unique_ids_typed_results_and_real_error_envelopes() {
    let doc = bog_cloud::contract::openapi();
    refs(&doc, &doc);
    let mut ids = std::collections::HashSet::new();
    for methods in doc["paths"].as_object().unwrap().values() {
        for op in methods.as_object().unwrap().values() {
            assert!(ids.insert(op["operationId"].as_str().unwrap()));
            assert!(!op["description"].as_str().unwrap().is_empty());
            for (status, result) in op["responses"].as_object().unwrap() {
                if status == "204" {
                    assert!(result.get("content").is_none());
                    continue;
                }
                assert!(
                    result
                        .pointer("/content/application~1json/schema")
                        .is_some()
                );
                if status.starts_with('4') || status.starts_with('5') {
                    validate(
                        &doc,
                        &result["content"]["application/json"]["schema"],
                        &json!({"error":{"code":"invalid_request","message":"Explain the correction"},"request_id":"test"}),
                    );
                }
            }
        }
    }
    assert_eq!(doc["paths"]["/v1"]["get"]["security"], json!([]));
    assert_eq!(doc["paths"]["/v1/templates"]["get"]["security"], json!([]));
    assert!(
        doc["paths"]["/v1/bogs"]["post"]["responses"]
            .get("200")
            .is_none()
    );
}
#[tokio::test]
async fn response_contract_matches_running_records_worker() {
    let dir = tempfile::tempdir().unwrap();
    let app = bog_cloud_records::records_router(dir.path()).unwrap();
    let doc = bog_cloud::contract::openapi();
    for (method, path, contract_path, body) in [
        (
            "PUT",
            "/docs/first",
            "/v1/bogs/{bog_id}/docs/{key}",
            json!({"hello":"world"}),
        ),
        (
            "GET",
            "/docs/first",
            "/v1/bogs/{bog_id}/docs/{key}",
            Value::Null,
        ),
        (
            "GET",
            "/views/docs",
            "/v1/bogs/{bog_id}/views/{view}",
            Value::Null,
        ),
        (
            "GET",
            "/views/total",
            "/v1/bogs/{bog_id}/views/{view}",
            Value::Null,
        ),
        (
            "GET",
            "/_cloud/usage",
            "/v1/bogs/{bog_id}/usage",
            Value::Null,
        ),
        (
            "POST",
            "/batch",
            "/v1/bogs/{bog_id}/batch",
            json!([{"op":"remove","key":"first"}]),
        ),
        (
            "DELETE",
            "/docs/first",
            "/v1/bogs/{bog_id}/docs/{key}",
            Value::Null,
        ),
    ] {
        let result = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.status(), 200, "{method} {path}");
        let value: Value =
            serde_json::from_slice(&to_bytes(result.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        validate(
            &doc,
            &doc["paths"][contract_path][method.to_lowercase()]["responses"]["200"]["content"]["application/json"]
                ["schema"],
            &value,
        );
    }
}

#[test]
fn composition_guidance_exposes_a_complete_fresh_agent_journey() {
    let spec = bog_cloud::contract::openapi();
    for (path, method) in [
        ("/v1/components", "get"),
        ("/v1/definitions/validate", "post"),
        ("/v1/bogs", "post"),
        ("/v1/bogs/{bog_id}/definition", "get"),
        ("/v1/bogs/{bog_id}/resources", "get"),
        ("/v1/bogs/{bog_id}/resources/{resource}/query", "post"),
        ("/v1/bogs/{bog_id}/resources/{resource}/search", "post"),
        ("/v1/bogs/{bog_id}/definition/plan", "post"),
        ("/v1/bogs/{bog_id}/definition/apply", "post"),
        ("/v1/bogs/{bog_id}/definition/jobs/{job_id}", "get"),
    ] {
        assert!(spec["paths"][path][method].is_object(), "{method} {path}");
    }
    let catalog = bog_definition::component_catalog();
    assert!(catalog["definition_schema"]["$defs"].is_object());
    for name in ["todo", "todo_search", "todo_semantic"] {
        let definition: bog_definition::Definition =
            serde_json::from_value(catalog["examples"][name].clone()).unwrap();
        definition.validate().unwrap();
    }
    let instructions = bog_cloud::contract::AGENT_INSTRUCTIONS;
    for tool in [
        "discover_capabilities",
        "validate_definition",
        "create_bog_from_definition",
        "describe_definition",
        "list_resources",
        "query_resource",
        "search_resource",
        "plan_definition_update",
        "apply_definition_update",
        "definition_update_status",
    ] {
        assert!(instructions.contains(tool), "missing {tool}");
    }
    assert!(instructions.contains("enabled"));
    assert!(instructions.contains("recovery_required"));
}
