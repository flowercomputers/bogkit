//! Versioned, inspectable definitions shared by local and hosted Bog runtimes.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const DEFINITION_VERSION: u32 = 1;
pub const MAX_RESOURCES: usize = 16;
pub const MAX_STAGES: usize = 8;
pub const MAX_SEMANTIC_INDEXES: usize = 1;
pub const MAX_VECTORS: usize = 10_000;
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_QUERY_BYTES: usize = 4 * 1024;
pub const MAX_HITS: usize = 50;
pub const SEMANTIC_DIMENSIONS: usize = 512;
pub const BM25_TOKENIZER: &str = "fold-ascii-alphanumeric-v1";
/// Must equal ese::ENCODER_ID; kept here to avoid linking inference into schema clients.
pub const SEMANTIC_MODEL: &str = "ese:static-retrieval-mrl-en-v1@f60985c706f192d45d218078e49e5a8b6f15283a:model-164fc63ee9f9267be7378fcbd7df99d09788a2f45244c92aa99ae5a574925716:tokenizer-d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66:preprocess-ese-bert-uncased-wordpiece-v1:dim-512:scalar-f32";
/// Host policy can lower ceilings, never enlarge the prototype's hard bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub resources: usize,
    pub stages_per_resource: usize,
    pub vectors: usize,
    pub text_bytes: usize,
    pub query_bytes: usize,
    pub hits: usize,
    pub build_timeout_seconds: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            resources: MAX_RESOURCES,
            stages_per_resource: MAX_STAGES,
            vectors: MAX_VECTORS,
            text_bytes: MAX_TEXT_BYTES,
            query_bytes: MAX_QUERY_BYTES,
            hits: MAX_HITS,
            build_timeout_seconds: 300,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<(), DefinitionError> {
        for (name, value, maximum) in [
            ("resources", self.resources, MAX_RESOURCES),
            ("stages_per_resource", self.stages_per_resource, MAX_STAGES),
            ("vectors", self.vectors, MAX_VECTORS),
            ("text_bytes", self.text_bytes, MAX_TEXT_BYTES),
            ("query_bytes", self.query_bytes, MAX_QUERY_BYTES),
            ("hits", self.hits, MAX_HITS),
        ] {
            ensure(
                value > 0 && value <= maximum,
                format!("{name} must be between 1 and {maximum}"),
            )?;
        }
        ensure(
            self.build_timeout_seconds > 0 && self.build_timeout_seconds <= 300,
            "build_timeout_seconds must be between 1 and 300",
        )
    }
    /// Creation/update admission only. Reopening existing stores must retain access
    /// even if the host subsequently lowered its resource or stage ceiling.
    pub fn validate_definition(&self, definition: &Definition) -> Result<(), DefinitionError> {
        self.validate()?;
        definition.validate()?;
        ensure(
            definition.resources.len() <= self.resources,
            "definition exceeds host resource limit",
        )?;
        for resource in definition.resources.values() {
            ensure(
                resource.stages.len() <= self.stages_per_resource,
                "definition exceeds host stage limit",
            )?;
        }
        Ok(())
    }
}
fn version() -> u32 {
    DEFINITION_VERSION
}
fn input() -> String {
    "docs".into()
}
fn tokenizer() -> String {
    BM25_TOKENIZER.into()
}
fn model() -> String {
    SEMANTIC_MODEL.into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default = "input")]
    pub input: String,
    pub resources: BTreeMap<String, Resource>,
    pub expose: BTreeMap<String, Operation>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resource {
    #[serde(default)]
    pub stages: Vec<Stage>,
    pub terminal: Terminal,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Stage {
    Filter {
        expression: Expression,
    },
    /// Output keys are literal object keys; source fields are JSON Pointers.
    Projection {
        fields: BTreeMap<String, String>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Expression {
    Equals { field: String, value: Value },
    NotEquals { field: String, value: Value },
    Lt { field: String, value: Value },
    Lte { field: String, value: Value },
    Gt { field: String, value: Value },
    Gte { field: String, value: Value },
    Exists { field: String },
    And { expressions: Vec<Expression> },
    Or { expressions: Vec<Expression> },
    Not { expression: Box<Expression> },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Terminal {
    Table,
    Count,
    Stats {
        field: String,
    },
    Ranked {
        field: String,
        #[serde(default)]
        ascending: bool,
    },
    Bm25 {
        fields: Vec<String>,
        #[serde(default = "tokenizer")]
        tokenizer: String,
    },
    Semantic {
        fields: Vec<String>,
        #[serde(default = "model")]
        model: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub target: String,
    pub action: Action,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Get,
    List,
    Read,
    Top,
    Search,
    Put,
    Remove,
    Batch,
    Wait,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OperationMetadata {
    pub name: String,
    pub target: String,
    pub action: Action,
    pub mutation: bool,
    pub request_schema: Value,
    /// Successful operation data, before any transport envelope.
    pub response_schema: Value,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AdditiveDiff {
    pub added_resources: Vec<String>,
    pub added_operations: Vec<String>,
}
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DefinitionError(pub String);
fn ensure(condition: bool, message: impl Into<String>) -> Result<(), DefinitionError> {
    if condition {
        Ok(())
    } else {
        Err(DefinitionError(message.into()))
    }
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
/// Strict RFC 6901 pointer syntax, including the empty root pointer.
pub fn validate_pointer(value: &str) -> Result<(), DefinitionError> {
    ensure(
        value.len() <= 1024 && (value.is_empty() || value.starts_with('/')),
        "field must be a JSON Pointer of at most 1024 bytes",
    )?;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            ensure(
                matches!(chars.next(), Some('0' | '1')),
                "invalid JSON Pointer escape",
            )?;
        }
    }
    Ok(())
}
impl Expression {
    fn validate(&self, depth: usize, budget: &mut usize) -> Result<(), DefinitionError> {
        ensure(
            depth <= 16 && *budget < 256,
            "filter expression exceeds depth or node limit",
        )?;
        *budget += 1;
        match self {
            Self::Equals { field, value }
            | Self::NotEquals { field, value }
            | Self::Lt { field, value }
            | Self::Lte { field, value }
            | Self::Gt { field, value }
            | Self::Gte { field, value } => {
                validate_pointer(field)?;
                ensure(
                    serde_json::to_vec(value)
                        .map_err(|e| DefinitionError(e.to_string()))?
                        .len()
                        <= MAX_TEXT_BYTES,
                    "filter literal too large",
                )?;
                if matches!(
                    self,
                    Self::Lt { .. } | Self::Lte { .. } | Self::Gt { .. } | Self::Gte { .. }
                ) {
                    ensure(
                        value.is_number() || value.is_string(),
                        "ordered filter literal must be number or string",
                    )?;
                }
            }
            Self::Exists { field } => validate_pointer(field)?,
            Self::And { expressions } | Self::Or { expressions } => {
                ensure(!expressions.is_empty(), "boolean filter requires children")?;
                for child in expressions {
                    child.validate(depth + 1, budget)?;
                }
            }
            Self::Not { expression } => expression.validate(depth + 1, budget)?,
        }
        Ok(())
    }
}
impl Terminal {
    pub fn supports(&self, action: Action) -> bool {
        action == Action::Wait
            || matches!(
                (self, action),
                (Self::Table, Action::Get | Action::List)
                    | (Self::Count | Self::Stats { .. }, Action::Read)
                    | (Self::Ranked { .. }, Action::Top)
                    | (Self::Bm25 { .. } | Self::Semantic { .. }, Action::Search)
            )
    }
}
impl Definition {
    pub fn records_v1() -> Self {
        Self {
            version: 1,
            input: input(),
            resources: BTreeMap::from([
                (
                    "docs".into(),
                    Resource {
                        stages: vec![],
                        terminal: Terminal::Table,
                    },
                ),
                (
                    "total".into(),
                    Resource {
                        stages: vec![],
                        terminal: Terminal::Count,
                    },
                ),
            ]),
            expose: BTreeMap::from([
                (
                    "get".into(),
                    Operation {
                        target: input(),
                        action: Action::Get,
                    },
                ),
                (
                    "list".into(),
                    Operation {
                        target: input(),
                        action: Action::List,
                    },
                ),
                (
                    "put".into(),
                    Operation {
                        target: input(),
                        action: Action::Put,
                    },
                ),
                (
                    "remove".into(),
                    Operation {
                        target: input(),
                        action: Action::Remove,
                    },
                ),
                (
                    "batch".into(),
                    Operation {
                        target: input(),
                        action: Action::Batch,
                    },
                ),
                (
                    "total".into(),
                    Operation {
                        target: "total".into(),
                        action: Action::Read,
                    },
                ),
                (
                    "wait".into(),
                    Operation {
                        target: input(),
                        action: Action::Wait,
                    },
                ),
            ]),
        }
    }
    pub fn validate(&self) -> Result<(), DefinitionError> {
        ensure(
            self.version == DEFINITION_VERSION,
            "unsupported definition version",
        )?;
        ensure(name(&self.input), "invalid input name")?;
        ensure(
            !self.resources.is_empty() && self.resources.len() <= MAX_RESOURCES,
            "resource count must be between 1 and 16",
        )?;
        ensure(self.expose.len() <= 128, "at most 128 exposed operations")?;
        let mut semantic = 0;
        for (id, resource) in &self.resources {
            ensure(name(id), format!("invalid resource name: {id}"))?;
            ensure(
                resource.stages.len() <= MAX_STAGES,
                "at most 8 stages per resource",
            )?;
            for stage in &resource.stages {
                match stage {
                    Stage::Filter { expression } => expression.validate(0, &mut 0)?,
                    Stage::Projection { fields } => {
                        ensure(fields.len() <= 64, "projection allows at most 64 fields")?;
                        for (key, pointer) in fields {
                            ensure(
                                !key.is_empty() && key.len() <= 256,
                                "invalid projection output key",
                            )?;
                            validate_pointer(pointer)?;
                        }
                    }
                }
            }
            match &resource.terminal {
                Terminal::Table | Terminal::Count => {}
                Terminal::Stats { field } | Terminal::Ranked { field, .. } => {
                    validate_pointer(field)?
                }
                Terminal::Bm25 { fields, tokenizer } => {
                    ensure(tokenizer == BM25_TOKENIZER, "unsupported BM25 tokenizer")?;
                    validate_fields(fields)?;
                }
                Terminal::Semantic { fields, model } => {
                    semantic += 1;
                    ensure(model == SEMANTIC_MODEL, "unsupported semantic model")?;
                    validate_fields(fields)?;
                }
            }
        }
        ensure(
            semantic <= MAX_SEMANTIC_INDEXES,
            "at most one semantic index",
        )?;
        for (id, operation) in &self.expose {
            ensure(name(id), format!("invalid operation name: {id}"))?;
            if matches!(
                operation.action,
                Action::Put | Action::Remove | Action::Batch
            ) {
                ensure(
                    operation.target == self.input,
                    "mutation must target the source input",
                )?;
            } else {
                let resource = self.resources.get(&operation.target).ok_or_else(|| {
                    DefinitionError(format!("operation {id} targets an unknown resource"))
                })?;
                ensure(
                    resource.terminal.supports(operation.action),
                    format!("operation {id} is not supported by its resource"),
                )?;
            }
        }
        Ok(())
    }
    pub fn normalized_json(&self) -> Result<String, DefinitionError> {
        self.validate()?;
        // Canonicalize recursively even if serde_json's preserve_order feature is enabled elsewhere.
        fn canonical(v: Value) -> Value {
            match v {
                Value::Object(o) => Value::Object(
                    o.into_iter()
                        .map(|(k, v)| (k, canonical(v)))
                        .collect::<BTreeMap<_, _>>()
                        .into_iter()
                        .collect(),
                ),
                Value::Array(a) => Value::Array(a.into_iter().map(canonical).collect()),
                other => other,
            }
        }
        serde_json::to_string(&canonical(
            serde_json::to_value(self).map_err(|e| DefinitionError(e.to_string()))?,
        ))
        .map_err(|e| DefinitionError(e.to_string()))
    }
    pub fn digest(&self) -> Result<String, DefinitionError> {
        Ok(format!(
            "{:x}",
            Sha256::digest(self.normalized_json()?.as_bytes())
        ))
    }
    pub fn validate_additive(&self, next: &Self) -> Result<AdditiveDiff, DefinitionError> {
        self.validate()?;
        next.validate()?;
        ensure(
            self.version == next.version && self.input == next.input,
            "input and version cannot change",
        )?;
        for (key, value) in &self.resources {
            ensure(
                next.resources.get(key) == Some(value),
                format!("existing resource {key} cannot change or be removed"),
            )?;
        }
        for (key, value) in &self.expose {
            ensure(
                next.expose.get(key) == Some(value),
                format!("existing operation {key} cannot change or be removed"),
            )?;
        }
        Ok(AdditiveDiff {
            added_resources: next
                .resources
                .keys()
                .filter(|k| !self.resources.contains_key(*k))
                .cloned()
                .collect(),
            added_operations: next
                .expose
                .keys()
                .filter(|k| !self.expose.contains_key(*k))
                .cloned()
                .collect(),
        })
    }
    pub fn operation_metadata(&self) -> Vec<OperationMetadata> {
        self.operation_metadata_with_limits(&Limits::default())
    }
    pub fn operation_metadata_with_limits(&self, limits: &Limits) -> Vec<OperationMetadata> {
        self.expose
            .iter()
            .map(|(name, op)| OperationMetadata {
                name: name.clone(),
                target: op.target.clone(),
                action: op.action,
                mutation: matches!(op.action, Action::Put | Action::Remove | Action::Batch),
                response_schema: response_schema(op.action, self.resources.get(&op.target).map(|r| &r.terminal)),
                request_schema: {
                    let mut schema = request_schema(op.action);
                    if op.action == Action::Search {
                        schema["properties"]["query"]["maxLength"] = json!(limits.query_bytes);
                        schema["properties"]["query"]["x-maxUtf8Bytes"] = json!(limits.query_bytes);
                        schema["properties"]["query"]["description"] = json!(format!("At most {} UTF-8 bytes; the runtime enforces this byte limit, including for multibyte text.", limits.query_bytes));
                        schema["properties"]["limit"]["maximum"] = json!(limits.hits);
                        if self.resources.get(&op.target).is_some_and(|r| matches!(r.terminal, Terminal::Semantic{..})) {
                            schema["properties"]["max_distance"] = json!({"type":"number","minimum":0,"maximum":2,"description":"Optional maximum cosine distance (inclusive). Lower is closer; no universal relevance cutoff is implied."});
                            schema["properties"]["vector"] = json!({"type":"array","minItems":SEMANTIC_DIMENSIONS,"maxItems":SEMANTIC_DIMENSIONS,"items":{"type":"number"},"description":"512 finite f32 values with nonzero finite norm; checked by the runtime."});
                            schema.as_object_mut().unwrap().remove("required");
                            schema["oneOf"] = json!([{"required":["query"]},{"required":["vector"]}]);
                        }
                    }
                    schema
                },
            })
            .collect()
    }
}
fn validate_fields(fields: &[String]) -> Result<(), DefinitionError> {
    ensure(
        !fields.is_empty() && fields.len() <= 64,
        "search requires 1 to 64 fields",
    )?;
    for field in fields {
        validate_pointer(field)?;
    }
    Ok(())
}
pub fn schema() -> Value {
    let mut schema =
        serde_json::to_value(schemars::schema_for!(Definition)).expect("schema serialization");
    // An absolute identifier establishes a resource scope when this schema is
    // embedded in a transport contract; its recursive #/$defs refs stay local.
    schema["$id"] = json!("urn:bog:definition:v1");
    schema
}
pub fn request_schema(action: Action) -> Value {
    let (properties, required) = match action {
        Action::Get => (
            json!({"key":{"type":"string"},"keys":{"type":"array","maxItems":100,"items":{"type":"string"},"description":"Batch get in request order, retaining duplicates and returning null for missing keys."}}),
            vec![],
        ),
        Action::Remove => (json!({"key":{"type":"string"}}), vec!["key"]),
        Action::Put => (
            json!({"key":{"type":"string"},"data":{"type":"object"}}),
            vec!["key", "data"],
        ),
        Action::Batch => (
            json!({"ops":{"type":"array","maxItems":100,"items":{"oneOf":[
                {"type":"object","properties":{"op":{"const":"upsert"},"key":{"type":"string"},"data":{"type":"object"}},"required":["op","key","data"],"additionalProperties":false},
                {"type":"object","properties":{"op":{"const":"remove"},"key":{"type":"string"}},"required":["op","key"],"additionalProperties":false}
            ]}}}),
            vec!["ops"],
        ),
        Action::List | Action::Top => (
            json!({"limit":{"type":"integer","minimum":0,"maximum":1000},"offset":{"type":"integer","minimum":0,"maximum":10000}}),
            vec![],
        ),
        Action::Search => (
            json!({"query":{"type":"string","maxLength":MAX_QUERY_BYTES,"x-maxUtf8Bytes":MAX_QUERY_BYTES,"description":"At most 4096 UTF-8 bytes; the runtime enforces this byte limit, including for multibyte text."},"limit":{"type":"integer","minimum":0,"maximum":MAX_HITS},"offset":{"type":"integer","minimum":0,"maximum":10000},"include_fields":{"type":"array","minItems":1,"maxItems":32,"uniqueItems":true,"items":{"type":"string","maxLength":1024,"pattern":"^(?:/(?:[^~]|~[01])*)*$"},"description":"Opt in to hit.value: an object keyed by requested JSON Pointers, read after resource stages. Missing fields are omitted. At most 1024 UTF-8 bytes per pointer; total response limited to 4 MiB."}}),
            vec!["query"],
        ),
        Action::Wait => (
            json!({"cursor":{"type":"string"},"timeout":{"type":"integer","minimum":0,"maximum":25,"description":"Wait timeout in seconds."}}),
            vec![],
        ),
        Action::Read => (json!({}), vec![]),
    };
    let mut schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    if action == Action::Get {
        schema["oneOf"] = json!([{"required":["key"]},{"required":["keys"]}]);
    }
    if action == Action::Top {
        schema["properties"]["include_fields"] =
            request_schema(Action::Search)["properties"]["include_fields"].clone();
    }
    if action == Action::List {
        schema["description"] = json!(
            "Rows ordered by key in ascending lexicographic order. after/before bounds are exclusive; neither can be combined with offset."
        );
        schema["properties"]["after"] =
            json!({"type":"string","description":"Exclusive lower key bound."});
        schema["properties"]["before"] =
            json!({"type":"string","description":"Exclusive upper key bound."});
        schema["not"] =
            json!({"required":["offset"],"anyOf":[{"required":["after"]},{"required":["before"]}]});
    }
    schema
}
/// Schema for successful operation data; transports may wrap this in an envelope.
pub fn response_schema(action: Action, terminal: Option<&Terminal>) -> Value {
    let object = |properties: Value, required: &[&str]| json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    let hit = |semantic: bool, projection: bool| {
        let mut properties = json!({"key":{"type":"string"},"score":{"type":"number"}});
        let mut required = vec!["key", "score"];
        if semantic {
            properties["distance"] = json!({"type":"number","description":"Cosine distance; lower is closer. score = 1 - distance is cosine similarity; higher is closer."});
            required.push("distance");
        }
        if projection {
            properties["value"] = json!({"type":"object","description":"Only present when include_fields is requested; keys are requested JSON Pointers, missing fields omitted."});
        }
        json!({"type":"array","items":object(properties, &required)})
    };
    match action {
        Action::Put | Action::Remove | Action::Batch => {
            object(json!({"ok":{"const":true}}), &["ok"])
        }
        Action::Get => {
            json!({"oneOf":[{"type":["object","null"]},{"type":"array","maxItems":100,"items":object(json!({"key":{"type":"string"},"value":{"type":["object","null"]}}), &["key","value"])}]})
        }
        Action::List => {
            json!({"type":"array","items":object(json!({"key":{"type":"string"},"value":{"type":"object"}}), &["key","value"])})
        }
        Action::Top => hit(false, true),
        Action::Search => {
            let semantic = matches!(terminal, Some(Terminal::Semantic { .. }));
            let mut schema = hit(semantic, true);
            schema["description"] = json!(if semantic {
                "Approximate nearest neighbors by cosine distance (lower is closer); score is 1 - distance (higher is closer). No universal relevance threshold."
            } else {
                "BM25 score (higher is more relevant). Whitespace tokenization, stripping non-ASCII-alphanumeric bytes within each token, ASCII lowercase, no stemming. Scores are query and corpus dependent."
            });
            schema
        }
        Action::Read if matches!(terminal, Some(Terminal::Stats { .. })) => object(
            json!({"count":{"type":"integer"},"sum":{"type":"number"},"mean":{"type":["number","null"]},"variance":{"type":["number","null"]},"stddev":{"type":["number","null"]}}),
            &["count", "sum", "mean", "variance", "stddev"],
        ),
        Action::Read => json!({"type":"integer"}),
        Action::Wait => object(
            json!({"seq":{"type":"integer","minimum":0},"cursor":{"type":"string"},"changed":{"type":"boolean"},"reset":{"type":"boolean"}}),
            &["seq", "cursor", "changed", "reset"],
        ),
    }
}
pub fn component_catalog() -> Value {
    component_catalog_with_limits(&Limits::default())
}
/// Callers must validate host configuration before publishing its effective policy.
pub fn component_catalog_with_limits(limits: &Limits) -> Value {
    json!({"version":DEFINITION_VERSION,"input":{"kind":"keyed_json_object","key_type":"string"},
    "stages":["filter","projection"],"terminals":["table","count","stats","ranked","bm25","semantic"],
    "bm25":{"tokenizer":BM25_TOKENIZER,"stemming":false,"score_direction":"higher is more relevant"},"semantic":{"model":SEMANTIC_MODEL,"dimensions":SEMANTIC_DIMENSIONS,"scalar":"f32","metric":"cosine","index":"anny"},
    "limits":{"resources":limits.resources,"stages_per_resource":limits.stages_per_resource,"semantic_indexes":MAX_SEMANTIC_INDEXES,"vectors":limits.vectors,"text_bytes":limits.text_bytes,"query_bytes":limits.query_bytes,"hits":limits.hits,"build_timeout_seconds":limits.build_timeout_seconds},
    "definition_schema":schema(),
    "examples":{
        "todo":serde_json::from_str::<Value>(include_str!("../../docs/examples/composable/todo.json")).expect("todo fixture"),
        "todo_search":serde_json::from_str::<Value>(include_str!("../../docs/examples/composable/todo-search.json")).expect("search fixture"),
        "todo_semantic":serde_json::from_str::<Value>(include_str!("../../docs/examples/composable/todo-semantic.json")).expect("semantic fixture")
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_definition_schema_resolves_recursive_refs_locally() {
        let wrapped_schema = json!({
            "$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object", "properties":{"definition":schema()},
            "required":["definition"], "additionalProperties":false
        });
        // jsonschema is built without HTTP/file resolution features: validation
        // must resolve all nested definition and recursive expression refs locally.
        let validator = jsonschema::validator_for(&wrapped_schema).unwrap();
        let mut definition = Definition::records_v1();
        definition
            .resources
            .get_mut("docs")
            .unwrap()
            .stages
            .push(Stage::Filter {
                expression: Expression::And {
                    expressions: vec![
                        Expression::Exists {
                            field: "/title".into(),
                        },
                        Expression::Not {
                            expression: Box::new(Expression::Equals {
                                field: "/completed".into(),
                                value: json!(true),
                            }),
                        },
                    ],
                },
            });
        let wrapped = json!({"definition":definition});
        assert!(validator.is_valid(&wrapped));
        let mut malformed = wrapped.clone();
        malformed["definition"]["resources"]["docs"]["stages"][0]["expression"]["expressions"][1]
            ["expression"]["field"] = json!(42);
        assert!(!validator.is_valid(&malformed));
        malformed = wrapped;
        malformed["definition"]["resources"]["docs"]["terminal"]["kind"] = json!("unknown");
        assert!(!validator.is_valid(&malformed));
    }
    #[test]
    fn limits_are_lowering_only_with_strict_configuration() {
        let defaults: Limits = serde_json::from_value(json!({})).unwrap();
        assert_eq!(defaults, Limits::default());
        defaults.validate().unwrap();
        assert!(serde_json::from_value::<Limits>(json!({"unknown": 2})).is_err());
        for key in [
            "resources",
            "stages_per_resource",
            "vectors",
            "text_bytes",
            "query_bytes",
            "hits",
            "build_timeout_seconds",
        ] {
            for value in [0, 100_000] {
                let candidate: Limits = serde_json::from_value(json!({key: value})).unwrap();
                assert!(candidate.validate().is_err(), "accepted {key}={value}");
            }
        }
        let limits: Limits = serde_json::from_value(json!({"resources":1,"stages_per_resource":1,"hits":4,"query_bytes":128,"build_timeout_seconds":10})).unwrap();
        limits.validate().unwrap();
        assert!(
            limits
                .validate_definition(&Definition::records_v1())
                .is_err()
        );
        // The definition remains valid for existing-store reopen under hard limits.
        Definition::records_v1().validate().unwrap();
        let mut definition = Definition::records_v1();
        definition.resources.remove("total");
        definition.expose.remove("total");
        limits.validate_definition(&definition).unwrap();
        definition.resources.get_mut("docs").unwrap().stages = vec![
            Stage::Projection {
                fields: BTreeMap::new()
            };
            2
        ];
        assert!(limits.validate_definition(&definition).is_err());
        assert_eq!(component_catalog_with_limits(&limits)["limits"]["hits"], 4);
        assert_eq!(
            component_catalog_with_limits(&limits)["limits"]["build_timeout_seconds"],
            10
        );
    }
    #[test]
    fn semantic_schema_requires_exactly_one_text_or_512d_vector() {
        let mut definition = Definition::records_v1();
        definition.resources.insert(
            "semantic".into(),
            Resource {
                stages: vec![],
                terminal: Terminal::Semantic {
                    fields: vec!["/text".into()],
                    model: model(),
                },
            },
        );
        definition.resources.insert(
            "text".into(),
            Resource {
                stages: vec![],
                terminal: Terminal::Bm25 {
                    fields: vec!["/text".into()],
                    tokenizer: tokenizer(),
                },
            },
        );
        for target in ["semantic", "text"] {
            definition.expose.insert(
                target.into(),
                Operation {
                    target: target.into(),
                    action: Action::Search,
                },
            );
        }
        let limits = Limits {
            hits: 3,
            query_bytes: 20,
            ..Limits::default()
        };
        let metadata = definition.operation_metadata_with_limits(&limits);
        let semantic = &metadata
            .iter()
            .find(|m| m.name == "semantic")
            .unwrap()
            .request_schema;
        let validator = jsonschema::validator_for(semantic).unwrap();
        let vector = vec![0.1; 512];
        assert!(validator.is_valid(&json!({"vector":vector})));
        assert!(validator.is_valid(&json!({"query":"hi","limit":3})));
        for invalid in [
            json!({}),
            json!({"query":"hi","vector":vector}),
            json!({"vector":vec![1;511]}),
            json!({"vector":vec![1;513]}),
            json!({"vector":vec!["a";512]}),
            json!({"query":"hi","limit":4}),
            json!({"query":"a".repeat(21)}),
        ] {
            assert!(
                !validator.is_valid(&invalid),
                "accepted invalid semantic query"
            );
        }
        assert_eq!(semantic["properties"]["query"]["x-maxUtf8Bytes"], 20);
        let text = &metadata
            .iter()
            .find(|m| m.name == "text")
            .unwrap()
            .request_schema;
        let validator = jsonschema::validator_for(text).unwrap();
        assert!(validator.is_valid(&json!({"query":"hi"})));
        assert!(!validator.is_valid(&json!({"vector":vector})));
    }
    #[test]
    fn batch_schema_describes_and_validates_runtime_mutations() {
        let schema = request_schema(Action::Batch);
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&json!({"ops":[{"op":"upsert","key":"a","data":{"title":"hello"}},{"op":"remove","key":"b"}]})));
        for invalid in [
            json!({"mutations":[]}),
            json!({"ops":[{}]}),
            json!({"ops":[{"op":"upsert","data":{}}]}),
            json!({"ops":[{"op":"upsert","key":"a","data":[] }]}),
            json!({"ops":[{"op":"upsert","key":"a","value":{} }]}),
            json!({"ops":[{"op":"remove","key":"a","data":{}}]}),
            json!({"ops":[{"op":"put","key":"a","data":{}}]}),
        ] {
            assert!(
                !validator.is_valid(&invalid),
                "accepted invalid mutation: {invalid}"
            );
        }
        assert!(!validator.is_valid(&json!({"ops":vec![json!({"op":"remove","key":"a"});101]})));
    }
    #[test]
    fn query_schemas_match_runtime_arguments_and_annotate_byte_limit() {
        let search = request_schema(Action::Search);
        assert_eq!(
            search["properties"]["query"]["x-maxUtf8Bytes"],
            MAX_QUERY_BYTES
        );
        assert!(
            search["properties"]["query"]["description"]
                .as_str()
                .unwrap()
                .contains("UTF-8 bytes")
        );
        let validator = jsonschema::validator_for(&search).unwrap();
        assert!(validator.is_valid(&json!({"query":"hello","limit":50,"offset":2})));
        assert!(!validator.is_valid(&json!({"query":"hello","limit":51})));
        let list = request_schema(Action::List);
        let validator = jsonschema::validator_for(&list).unwrap();
        assert!(validator.is_valid(&json!({"limit":1000,"offset":10000})));
        assert!(validator.is_valid(&json!({"after":"a","before":"z"})));
        assert!(!validator.is_valid(&json!({"after":"a","offset":0})));
        let put = request_schema(Action::Put);
        let validator = jsonschema::validator_for(&put).unwrap();
        assert!(validator.is_valid(&json!({"key":"a","data":{}})));
        assert!(!validator.is_valid(&json!({"key":"a","value":{}})));
    }
    #[test]
    fn compatibility_definition_has_table_and_count() {
        let d = Definition::records_v1();
        d.validate().unwrap();
        assert_eq!(d.resources["docs"].terminal, Terminal::Table);
        assert_eq!(d.resources["total"].terminal, Terminal::Count);
        assert_eq!(d.digest().unwrap().len(), 64);
    }
    #[test]
    fn defaults_and_object_order_normalize_identically() {
        let a: Definition = serde_json::from_value(
            json!({"resources":{"x":{"terminal":{"kind":"bm25","fields":["/title"]}}},"expose":{}}),
        )
        .unwrap();
        let b: Definition = serde_json::from_str(&a.normalized_json().unwrap()).unwrap();
        assert_eq!(a.digest().unwrap(), b.digest().unwrap());
        let mut a = Definition::records_v1();
        a.resources
            .get_mut("docs")
            .unwrap()
            .stages
            .push(Stage::Filter {
                expression: Expression::Equals {
                    field: "/x".into(),
                    value: serde_json::from_str("{\"b\":1,\"a\":2}").unwrap(),
                },
            });
        let mut b = a.clone();
        if let Stage::Filter {
            expression: Expression::Equals { value, .. },
        } = &mut b.resources.get_mut("docs").unwrap().stages[0]
        {
            *value = serde_json::from_str("{\"a\":2,\"b\":1}").unwrap();
        }
        assert_eq!(a.digest().unwrap(), b.digest().unwrap());
    }
    #[test]
    fn reject_unknown_fields_and_versions() {
        assert!(
            serde_json::from_value::<Resource>(
                json!({"terminal":{"kind":"table"},"source":"other"})
            )
            .is_err()
        );
        let mut d = Definition::records_v1();
        d.version = 99;
        assert!(d.validate().is_err());
        assert!(
            serde_json::from_value::<Terminal>(
                json!({"kind":"semantic","fields":["/a"],"dimensions":42})
            )
            .is_err()
        );
    }
    #[test]
    fn pointers_are_strict() {
        for p in ["", "/a", "/a~1b/~0", "/0"] {
            validate_pointer(p).unwrap();
        }
        for p in ["a", "$.a", "/a~2", "/~"] {
            assert!(validate_pointer(p).is_err(), "{p}");
        }
    }
    #[test]
    fn additive_updates_preserve_existing_contract() {
        let old = Definition::records_v1();
        let mut next = old.clone();
        next.resources.insert(
            "other".into(),
            Resource {
                stages: vec![],
                terminal: Terminal::Count,
            },
        );
        assert_eq!(
            old.validate_additive(&next).unwrap().added_resources,
            vec!["other"]
        );
        next.expose.get_mut("list").unwrap().action = Action::Wait;
        assert!(old.validate_additive(&next).is_err());
        let mut next = old.clone();
        next.resources.remove("total");
        assert!(old.validate_additive(&next).is_err());
    }
    #[test]
    fn validates_exposure_and_search_configuration() {
        let mut d = Definition::records_v1();
        d.expose.insert(
            "hidden".into(),
            Operation {
                target: "missing".into(),
                action: Action::Read,
            },
        );
        assert!(d.validate().is_err());
        d.expose.remove("hidden");
        d.expose.get_mut("list").unwrap().action = Action::Search;
        assert!(d.validate().is_err());
        d.expose.remove("list");
        for id in ["a", "b"] {
            d.resources.insert(
                id.into(),
                Resource {
                    stages: vec![],
                    terminal: Terminal::Semantic {
                        fields: vec!["/text".into()],
                        model: model(),
                    },
                },
            );
        }
        assert!(d.validate().is_err());
        d.resources.remove("b");
        d.validate().unwrap();
        if let Terminal::Semantic { model, .. } = &mut d.resources.get_mut("a").unwrap().terminal {
            *model = "custom".into();
        }
        assert!(d.validate().is_err());
    }
    #[test]
    fn bounded_expression_validation() {
        let mut expression = Expression::Exists { field: "/x".into() };
        for _ in 0..18 {
            expression = Expression::Not {
                expression: Box::new(expression),
            };
        }
        let mut d = Definition::records_v1();
        d.resources
            .get_mut("docs")
            .unwrap()
            .stages
            .push(Stage::Filter { expression });
        assert!(d.validate().is_err());
    }
    #[test]
    fn schemas_and_catalog_are_serializable() {
        for example in component_catalog()["examples"]
            .as_object()
            .unwrap()
            .values()
        {
            let definition: Definition = serde_json::from_value(example.clone()).unwrap();
            definition.validate().unwrap();
        }
        assert!(schema()["properties"]["resources"].is_object());
        assert_eq!(component_catalog()["semantic"]["dimensions"], 512);
        assert_eq!(
            component_catalog()["bm25"]["tokenizer"],
            "fold-ascii-alphanumeric-v1"
        );
        assert_eq!(Definition::records_v1().operation_metadata().len(), 7);
    }
}
