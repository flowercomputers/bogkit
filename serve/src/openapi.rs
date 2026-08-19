//! Assemble the OpenAPI document and the `/schema` fingerprint from the
//! input type's JSON schema plus the pipeline's view specs — the same
//! values the router dispatches with, so doc and behavior cannot drift.

use serde_json::{Value, json};

use crate::views::ViewSpec;

/// How this server writes: raw deltas, or upsert/remove by primary key.
#[derive(Clone, Copy)]
pub(crate) enum WriteStyle<'a> {
    Unkeyed,
    Keyed { key_schema: &'a Value },
}

/// Reads respond `{ "seq": <commit seq>, "data": <view-specific> }`.
fn envelope(data: Value) -> Value {
    json!({
        "type": "object",
        "properties": { "seq": { "type": "integer" }, "data": data },
        "required": ["seq", "data"],
    })
}

/// A JSON response wrapped in OpenAPI's content/media-type nesting.
fn json_response(desc: &str, schema: Value) -> Value {
    json!({
        "200": {
            "description": desc,
            "content": { "application/json": { "schema": schema } },
        }
    })
}

/// A write endpoint: POST a body, get back the committed seq.
fn write_op(summary: &str, body_schema: &Value) -> Value {
    let seq = json!({
        "type": "object",
        "properties": { "seq": { "type": "integer" } },
        "required": ["seq"],
    });
    json!({
        "post": {
            "summary": summary,
            "requestBody": {
                "required": true,
                "content": { "application/json": { "schema": body_schema } },
            },
            "responses": json_response("committed atomically", seq),
        }
    })
}

fn page_params() -> Value {
    json!([
        { "name": "limit", "in": "query", "schema": { "type": "integer" } },
        { "name": "offset", "in": "query", "schema": { "type": "integer" } },
    ])
}

pub(crate) fn openapi_doc(
    input: &Value,
    specs: &[ViewSpec],
    style: WriteStyle<'_>,
    custom_paths: &[String],
) -> Value {
    let mut paths = serde_json::Map::new();

    match style {
        WriteStyle::Unkeyed => {
            paths.insert(
                "/insert".into(),
                write_op("insert one record into the pipeline", input),
            );
            paths.insert(
                "/remove".into(),
                write_op(
                    "retract one record: every view rolls back as if it was never inserted",
                    input,
                ),
            );
            let batch_body = json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["insert", "remove"] },
                        "data": input,
                    },
                    "required": ["op", "data"],
                },
            });
            paths.insert(
                "/batch".into(),
                write_op(
                    "apply a mix of inserts and removes in one atomic transaction",
                    &batch_body,
                ),
            );
        }
        WriteStyle::Keyed { key_schema } => {
            let key_param = json!([{
                "name": "key", "in": "path", "required": true,
                "schema": { "type": "string" },
                "description": "the key, as JSON or a bare string",
            }]);
            paths.insert(
                "/docs/{key}".into(),
                json!({
                    "put": {
                        "summary": "insert or replace the record under this key; \
                                    the old record is retracted from every view",
                        "parameters": key_param,
                        "requestBody": {
                            "required": true,
                            "content": { "application/json": { "schema": input } },
                        },
                        "responses": json_response("committed", json!({
                            "type": "object",
                            "properties": {
                                "seq": { "type": "integer" },
                                "replaced": { "type": "boolean" },
                            },
                            "required": ["seq", "replaced"],
                        })),
                    },
                    "delete": {
                        "summary": "remove by key, retracting the record from every view",
                        "parameters": key_param,
                        "responses": json_response("committed", json!({
                            "type": "object",
                            "properties": {
                                "seq": { "type": "integer" },
                                "removed": { "type": "boolean" },
                            },
                            "required": ["seq", "removed"],
                        })),
                    },
                    "get": {
                        "summary": "the current record under this key",
                        "parameters": key_param,
                        "responses": json_response("found", envelope(input.clone())),
                    },
                }),
            );
            let batch_body = json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": { "type": "string", "enum": ["upsert", "remove"] },
                        "key": key_schema,
                        "data": input,
                    },
                    "required": ["op", "key"],
                },
            });
            paths.insert(
                "/batch".into(),
                write_op(
                    "apply a mix of upserts and removes in one atomic transaction",
                    &batch_body,
                ),
            );
        }
    }

    for spec in specs {
        // count views return one item; bag and table views return a page
        let (data, summary) = match spec.kind {
            "count" => (spec.item_schema.clone(), "read this view".to_string()),
            _ => (
                json!({ "type": "array", "items": spec.item_schema }),
                format!("list this {} view (paginated)", spec.kind),
            ),
        };
        let mut get = json!({
            "summary": summary,
            "responses": json_response("one consistent snapshot", envelope(data)),
        });
        if spec.kind != "count" {
            get["parameters"] = page_params();
        }
        paths.insert(format!("/views/{}", spec.name), json!({ "get": get }));

        if spec.keyed {
            paths.insert(
                format!("/views/{}/{{key}}", spec.name),
                json!({
                    "get": {
                        "summary": "point-read one key",
                        "parameters": [{
                            "name": "key", "in": "path", "required": true,
                            "schema": { "type": "string" },
                            "description": "the key, as JSON or a bare string",
                        }],
                        "responses": json_response(
                            "the current value under this key",
                            envelope(spec.item_schema.clone()),
                        ),
                    }
                }),
            );
        }

        if let Some(mode) = spec.search {
            let hits = envelope(json!({ "type": "array", "items": spec.item_schema }));
            let mut ops = serde_json::Map::new();
            if mode.contains("text") {
                ops.insert("get".into(), json!({
                    "summary": "text search, ranked",
                    "parameters": [
                        { "name": "q", "in": "query", "required": true,
                          "schema": { "type": "string" } },
                        { "name": "k", "in": "query",
                          "schema": { "type": "integer", "default": 10 } },
                    ],
                    "responses": json_response("hits, best first", hits.clone()),
                }));
            }
            if mode.contains("vector") {
                ops.insert("post".into(), json!({
                    "summary": "nearest-neighbor search by raw vector",
                    "requestBody": {
                        "required": true,
                        "content": { "application/json": { "schema": {
                            "type": "object",
                            "properties": {
                                "vector": { "type": "array", "items": { "type": "number" } },
                                "k": { "type": "integer", "default": 10 },
                            },
                            "required": ["vector"],
                        } } },
                    },
                    "responses": json_response("hits, nearest first", hits),
                }));
            }
            paths.insert(format!("/views/{}/search", spec.name), Value::Object(ops));
        }
    }

    for path in custom_paths {
        paths.insert(
            path.clone(),
            json!({ "get": {
                "summary": "custom route (app-defined; shape not generated)",
                "responses": { "200": { "description": "app-defined data in the seq envelope" } },
            } }),
        );
    }

    paths.insert(
        "/watch".into(),
        json!({ "get": {
            "summary": "server-sent events: one {\"seq\": n} event per commit",
            "responses": { "200": { "description": "text/event-stream" } },
        } }),
    );
    paths.insert(
        "/healthz".into(),
        json!({ "get": { "summary": "liveness probe", "responses": { "200": { "description": "ok" } } } }),
    );

    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "bog-serve",
            "description": "auto-generated HTTP API over a fold pipeline",
            "version": "0.0.0",
        },
        "paths": paths,
    })
}

/// The `/schema` document: the machine-comparable identity of this server's
/// pipeline. Two builds serving the same input type and sink structure
/// produce the same fingerprint; a mismatch against a data dir's previous
/// fingerprint is how deploys will detect incompatible pipeline changes.
pub(crate) fn schema_doc(input: &Value, specs: &[ViewSpec], style: WriteStyle<'_>) -> Value {
    let views: Vec<Value> = specs
        .iter()
        .map(|s| json!({
            "name": s.name, "kind": s.kind, "keyed": s.keyed,
            "search": s.search, "item": s.item_schema,
        }))
        .collect();
    let write = match style {
        WriteStyle::Unkeyed => json!("unkeyed"),
        WriteStyle::Keyed { key_schema } => json!({ "keyed": key_schema }),
    };
    let body = json!({ "input": input, "views": views, "write": write });
    // serde_json maps iterate in sorted key order, so this string is a
    // canonical encoding — safe to hash
    let fingerprint = format!("{:016x}", fnv1a(body.to_string().as_bytes()));
    json!({ "fingerprint": fingerprint, "input": input, "views": views, "write": write })
}

/// FNV-1a, 64-bit: tiny, dependency-free, and stable across builds and
/// platforms — unlike std's DefaultHasher, which explicitly is not.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
