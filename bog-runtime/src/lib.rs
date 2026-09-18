//! Configurable, bounded JSON dataflow backed by real Fold terminals.
mod document;
mod pipeline;
use bog_definition::{Action, Definition, Expression, Limits, Resource, Stage, Terminal};
pub use document::{DocumentError, JsonDocument};
use fold::stream::KeyedStream;
use pipeline::Pipeline;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub const MAX_LOGICAL_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_BATCH_OPS: usize = 100;
pub const MAX_TEXT_BYTES: usize = 8192;
pub const MAX_QUERY_BYTES: usize = 4096;
pub const MAX_VECTORS: usize = 10000;
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct RuntimeError(pub String);
impl From<fold::fjall::Error> for RuntimeError {
    fn from(e: fold::fjall::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<bog_definition::DefinitionError> for RuntimeError {
    fn from(e: bog_definition::DefinitionError) -> Self {
        Self(e.to_string())
    }
}
impl From<DocumentError> for RuntimeError {
    fn from(e: DocumentError) -> Self {
        Self(e.to_string())
    }
}
type Result<T> = std::result::Result<T, RuntimeError>;
fn invalid(s: impl Into<String>) -> RuntimeError {
    RuntimeError(s.into())
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mutation {
    Upsert { key: String, data: JsonDocument },
    Remove { key: String },
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub key: Option<String>,
    pub query: Option<String>,
    pub vector: Option<Vec<f32>>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Record {
    pub key: String,
    pub value: JsonDocument,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Export {
    pub records: Vec<Record>,
    pub record_count: usize,
    pub source_digest: String,
    pub next_offset: Option<usize>,
}
pub struct Runtime {
    definition: Definition,
    stream: KeyedStream<String, JsonDocument, Pipeline>,
    limit: u64,
    limits: Limits,
    failed: bool,
}
impl Runtime {
    pub fn open(path: impl AsRef<Path>, definition: Definition) -> Result<Self> {
        Self::open_with_limit(path, definition, MAX_LOGICAL_BYTES)
    }
    pub fn open_with_limit(
        path: impl AsRef<Path>,
        definition: Definition,
        limit: u64,
    ) -> Result<Self> {
        Self::open_with_limits(path, definition, limit, Limits::default())
    }
    pub fn open_with_limits(
        path: impl AsRef<Path>,
        definition: Definition,
        limit: u64,
        limits: Limits,
    ) -> Result<Self> {
        limits.validate()?;
        definition.validate()?;
        if definition
            .resources
            .values()
            .any(|r| matches!(r.terminal, Terminal::Semantic { .. }))
            && (ese::DIMENSIONS != 512
                || ese::ENCODER_IDENTITY.scalar_type != "f32"
                || ese::ENCODER_ID != bog_definition::SEMANTIC_MODEL)
        {
            return Err(invalid(
                "semantic runtime requires pinned 512-dimensional f32 encoder",
            ));
        }
        let path = path.as_ref();
        let digest = definition.digest()?;
        let identity_path = path.join("bog-definition.json");
        if !identity_path.exists() {
            limits.validate_definition(&definition)?;
        }
        if identity_path.exists() {
            let previous: Definition = serde_json::from_slice(
                &std::fs::read(&identity_path).map_err(|e| invalid(e.to_string()))?,
            )
            .map_err(|e| invalid(e.to_string()))?;
            if previous.digest()? != digest {
                return Err(invalid(
                    "definition differs from persisted store; build a separate candidate",
                ));
            }
        }
        let stream = KeyedStream::try_new(path, Pipeline::new(&definition))?;
        if !identity_path.exists() {
            let file = std::fs::File::create(&identity_path).map_err(|e| invalid(e.to_string()))?;
            serde_json::to_writer(&file, &definition).map_err(|e| invalid(e.to_string()))?;
            file.sync_all().map_err(|e| invalid(e.to_string()))?;
            std::fs::File::open(path)
                .and_then(|f| f.sync_all())
                .map_err(|e| invalid(e.to_string()))?;
        }
        Ok(Self {
            definition,
            stream,
            limit,
            limits,
            failed: false,
        })
    }
    pub fn definition(&self) -> &Definition {
        &self.definition
    }
    pub fn get(&self, key: &str) -> Option<JsonDocument> {
        self.stream.get(&key.to_owned())
    }
    pub fn logical_bytes(&self) -> u64 {
        self.stream
            .rtx(|r| r.source.iter().map(|(k, v)| encoded_size(&k, &v)).sum())
    }
    pub fn limit(&self) -> u64 {
        self.limit
    }
    pub fn limits(&self) -> &Limits {
        &self.limits
    }
    pub fn vector_count(&self) -> usize {
        self.stream
            .rtx(|r| r.resources.values().map(|v| v.vector_count()).sum())
    }
    pub fn checkpoint(&mut self) -> Result<()> {
        self.stream.try_checkpoint().map_err(Into::into)
    }
    pub fn mutate(&mut self, ops: &[Mutation]) -> Result<()> {
        if self.failed {
            return Err(invalid("runtime requires restart after checkpoint failure"));
        }
        if ops.len() > MAX_BATCH_OPS {
            return Err(invalid("batch exceeds 100 operations"));
        }
        if serde_json::to_vec(ops)
            .map_err(|e| invalid(e.to_string()))?
            .len()
            > MAX_REQUEST_BYTES
        {
            return Err(invalid("request exceeds 1 MiB"));
        }
        for op in ops {
            match op {
                Mutation::Upsert { key, data } => {
                    validate_key(key)?;
                    for resource in self.definition.resources.values() {
                        if let Some(value) = evaluate(resource, data.as_value())?
                            && let Terminal::Bm25 { fields, .. } | Terminal::Semantic { fields, .. } =
                                &resource.terminal
                            && extracted_text(&value, fields)?
                                .is_some_and(|text| text.len() > self.limits.text_bytes)
                        {
                            return Err(invalid("extracted text exceeds configured byte limit"));
                        }
                    }
                }
                Mutation::Remove { key } => validate_key(key)?,
            }
        }
        let limit = self.limit;
        let vector_limit = self.limits.vectors;
        let semantic = self
            .definition
            .resources
            .iter()
            .filter(|(_, r)| matches!(r.terminal, Terminal::Semantic { .. }))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        self.stream.try_wtx(|tx| {
            let before: u64 = tx.rtx(|r| r.source.iter().map(|(k, v)| encoded_size(&k, &v)).sum());
            let before_vectors: BTreeMap<String, usize> = tx.rtx(|r| {
                semantic
                    .iter()
                    .map(|name| (name.clone(), r.resources[name].vector_count()))
                    .collect()
            });
            for op in ops {
                match op {
                    Mutation::Upsert { key, data } => {
                        tx.upsert(key, data);
                    }
                    Mutation::Remove { key } => {
                        tx.remove(key);
                    }
                }
            }
            // Flush every real Fold sink before validating totals. Err rolls back persistent and in-memory search effects.
            tx.rtx(|r| {
                let used: u64 = r.source.iter().map(|(k, v)| encoded_size(&k, &v)).sum();
                if used > limit && used > before {
                    return Err(invalid("source logical byte quota exceeded"));
                }
                for reader in r.resources.values() {
                    reader.validate_state()?;
                }
                for name in &semantic {
                    if r.resources[name].vector_count() > vector_limit
                        && r.resources[name].vector_count() > before_vectors[name]
                    {
                        return Err(invalid("semantic vector quota exceeded"));
                    }
                }
                Ok(())
            })
        })?;
        if self.stream.try_checkpoint().is_err() {
            self.failed = true;
            return Err(invalid(
                "storage checkpoint failed; restart required; commit may have persisted",
            ));
        }
        Ok(())
    }
    pub fn execute(&mut self, name: &str, body: Value) -> Result<Value> {
        let op = self
            .definition
            .expose
            .get(name)
            .cloned()
            .ok_or_else(|| invalid("operation is not exposed"))?;
        match op.action {
            Action::Put => {
                let key = body
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("key required"))?;
                let data = JsonDocument::try_from_value(
                    body.get("data")
                        .cloned()
                        .ok_or_else(|| invalid("data required"))?,
                )?;
                self.mutate(&[Mutation::Upsert {
                    key: key.into(),
                    data,
                }])?;
                Ok(json!({"ok":true}))
            }
            Action::Remove => {
                let key = body
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("key required"))?;
                self.mutate(&[Mutation::Remove { key: key.into() }])?;
                Ok(json!({"ok":true}))
            }
            Action::Batch => {
                let ops: Vec<Mutation> =
                    serde_json::from_value(body.get("ops").cloned().unwrap_or(body))
                        .map_err(|e| invalid(e.to_string()))?;
                self.mutate(&ops)?;
                Ok(json!({"ok":true}))
            }
            Action::Wait => Err(invalid("wait is a transport notification operation")),
            action => {
                let q: Query = serde_json::from_value(body).map_err(|e| invalid(e.to_string()))?;
                self.query(&op.target, action, &q)
            }
        }
    }
    /// Internal read entry point. Transport adapters must enforce exposure before calling.
    pub fn query(&self, target: &str, action: Action, q: &Query) -> Result<Value> {
        if q.limit.is_some_and(|n| n > 1000) || q.offset.is_some_and(|n| n > 10000) {
            return Err(invalid("page limit exceeded"));
        }
        if q.query
            .as_ref()
            .is_some_and(|s| s.len() > self.limits.query_bytes)
        {
            return Err(invalid("query exceeds configured byte limit"));
        }
        if action == Action::Search && q.limit.is_some_and(|n| n > self.limits.hits) {
            return Err(invalid("search hit limit exceeds configured maximum"));
        }
        if target == self.definition.input
            && !self.definition.resources.contains_key(target)
            && action == Action::Get
        {
            let key = q.key.as_ref().ok_or_else(|| invalid("key required"))?;
            return Ok(self
                .get(key)
                .map(|v| v.as_value().clone())
                .unwrap_or(Value::Null));
        }
        let mut effective_query = q.clone();
        if action == Action::Search && effective_query.limit.is_none() {
            effective_query.limit = Some(self.limits.hits.min(10));
        }
        self.stream.rtx(|r| {
            r.resources
                .get(target)
                .ok_or_else(|| invalid("unknown resource"))?
                .query(action, &effective_query)
        })
    }
    pub fn export(&self, offset: usize) -> Export {
        self.stream.rtx(|r| {
            let all: BTreeMap<_, _> = r.source.iter().collect();
            let mut hash = Sha256::new();
            for (k, v) in &all {
                hash.update(serde_json::to_vec(&(k, v)).unwrap());
                hash.update(b"\n");
            }
            let mut records = Vec::new();
            let mut bytes = 0;
            for (k, v) in all.iter().skip(offset) {
                let n = encoded_size(k, v) as usize + 64;
                if !records.is_empty() && bytes + n > 768 * 1024 {
                    break;
                }
                bytes += n;
                records.push(Record {
                    key: k.clone(),
                    value: v.clone(),
                });
                if records.len() == 100 {
                    break;
                }
            }
            let next = offset + records.len();
            Export {
                records,
                record_count: all.len(),
                source_digest: format!("{:x}", hash.finalize()),
                next_offset: (next < all.len()).then_some(next),
            }
        })
    }
}
fn encoded_size(k: &String, v: &JsonDocument) -> u64 {
    (serde_json::to_vec(k).unwrap().len() + serde_json::to_vec(v).unwrap().len()) as u64
}
pub fn validate_key(k: &str) -> Result<()> {
    if k.is_empty()
        || k == "."
        || k == ".."
        || k.len() > 256
        || k.contains('/')
        || k.chars().any(char::is_control)
    {
        Err(invalid("invalid record key"))
    } else {
        Ok(())
    }
}
pub(crate) fn evaluate(resource: &Resource, value: &Value) -> Result<Option<Value>> {
    let mut value = value.clone();
    for stage in &resource.stages {
        match stage {
            Stage::Filter { expression } => {
                if !predicate(expression, &value) {
                    return Ok(None);
                }
            }
            Stage::Projection { fields } => {
                let mut projected = serde_json::Map::new();
                let mut bytes = 2usize;
                for (name, path) in fields {
                    if let Some(v) = value.pointer(path) {
                        bytes += serde_json::to_vec(name).unwrap().len()
                            + serde_json::to_vec(v).unwrap().len()
                            + 2;
                        if bytes > document::MAX_DOCUMENT_BYTES {
                            return Err(invalid("projection exceeds maximum record size"));
                        }
                        projected.insert(name.clone(), v.clone());
                    }
                }
                value = Value::Object(projected);
                JsonDocument::try_from_value(value.clone())?;
            }
        }
    }
    match &resource.terminal {
        Terminal::Stats { field } | Terminal::Ranked { field, .. } => match value.pointer(field) {
            None => return Ok(None),
            Some(v) => {
                let n = v
                    .as_f64()
                    .ok_or_else(|| invalid("numeric terminal field must be a number"))?;
                if !n.is_finite()
                    || (matches!(resource.terminal, Terminal::Stats { .. })
                        && !n.mul_add(n, 0.0).is_finite())
                {
                    return Err(invalid("numeric terminal field out of range"));
                }
            }
        },
        Terminal::Bm25 { fields, .. } if extracted_text(&value, fields)?.is_none() => {
            return Ok(None);
        }
        Terminal::Semantic { fields, .. } => match extracted_text(&value, fields)? {
            None => return Ok(None),
            Some(text) => pipeline::validate_vector(&ese::encode_single(&text))?,
        },
        _ => {}
    }
    Ok(Some(value))
}
pub(crate) fn extracted_text(value: &Value, fields: &[String]) -> Result<Option<String>> {
    let mut text = Vec::new();
    for field in fields {
        if let Some(v) = value.pointer(field) {
            text.push(
                v.as_str()
                    .ok_or_else(|| invalid("search field must be a string"))?,
            )
        }
    }
    if text.is_empty() {
        return Ok(None);
    }
    let text = text.join("\n");
    if text.len() > MAX_TEXT_BYTES {
        return Err(invalid("extracted search text exceeds 8 KiB"));
    }
    Ok(Some(text))
}
pub fn predicate(e: &Expression, v: &Value) -> bool {
    use Expression::*;
    match e {
        Equals { field, value } => v.pointer(field) == Some(value),
        NotEquals { field, value } => v.pointer(field).is_some_and(|x| x != value),
        Exists { field } => v.pointer(field).is_some(),
        And { expressions } => expressions.iter().all(|e| predicate(e, v)),
        Or { expressions } => expressions.iter().any(|e| predicate(e, v)),
        Not { expression } => !predicate(expression, v),
        Lt { field, value } | Lte { field, value } | Gt { field, value } | Gte { field, value } => {
            let cmp = v.pointer(field).and_then(|x| match (x, value) {
                (Value::Number(a), Value::Number(b)) => a.as_f64()?.partial_cmp(&b.as_f64()?),
                (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
                _ => None,
            });
            cmp.is_some_and(|c| match e {
                Lt { .. } => c.is_lt(),
                Lte { .. } => !c.is_gt(),
                Gt { .. } => c.is_gt(),
                Gte { .. } => !c.is_lt(),
                _ => false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition() -> Definition {
        serde_json::from_value(json!({"resources":{
  "docs":{"terminal":{"kind":"table"}},
  "pending":{"stages":[{"kind":"filter","expression":{"op":"equals","field":"/done","value":false}},{"kind":"projection","fields":{"title":"/title"}}],"terminal":{"kind":"table"}},
  "total":{"terminal":{"kind":"count"}},
  "stats":{"terminal":{"kind":"stats","field":"/priority"}},
  "rank":{"terminal":{"kind":"ranked","field":"/priority"}},
  "text":{"terminal":{"kind":"bm25","fields":["/title"]}},
  "semantic":{"terminal":{"kind":"semantic","fields":["/title"]}}
 },"expose":{"put":{"target":"docs","action":"put"},"pending":{"target":"pending","action":"list"},"total":{"target":"total","action":"read"},"rank":{"target":"rank","action":"top"},"text":{"target":"text","action":"search"},"semantic":{"target":"semantic","action":"search"}}})).unwrap()
    }
    fn put(key: &str, title: &str, priority: f64) -> Mutation {
        Mutation::Upsert {
            key: key.into(),
            data: JsonDocument::try_from_value(
                json!({"title":title,"priority":priority,"done":false}),
            )
            .unwrap(),
        }
    }
    #[test]
    fn shared_encoder_identity() {
        assert_eq!(bog_definition::SEMANTIC_MODEL, ese::ENCODER_ID);
        assert_eq!(ese::DIMENSIONS, 512)
    }
    #[test]
    fn atomic_updates_search_restart_and_retractions() {
        let dir = tempfile::tempdir().unwrap();
        let def = definition();
        let mut runtime = Runtime::open(dir.path(), def.clone()).unwrap();
        runtime
            .mutate(&[
                put("a", "write and publish software release", 2.),
                put("b", "bake bread in oven", 1.),
            ])
            .unwrap();
        assert_eq!(runtime.execute("total", json!({})).unwrap(), json!(2));
        assert_eq!(
            runtime.execute("rank", json!({"limit":1})).unwrap()[0]["key"],
            "a"
        );
        assert_eq!(
            runtime
                .execute("text", json!({"query":"software"}))
                .unwrap()[0]["key"],
            "a"
        );
        let before = runtime
            .execute("semantic", json!({"query":"publish software"}))
            .unwrap();
        assert_eq!(before[0]["key"], "a");
        let digest = runtime.export(0).source_digest;
        drop(runtime);
        let mut runtime = Runtime::open(dir.path(), def).unwrap();
        assert_eq!(runtime.export(0).source_digest, digest);
        assert_eq!(
            runtime
                .execute("semantic", json!({"query":"publish software"}))
                .unwrap(),
            before
        );
        runtime
            .mutate(&[
                put("a", "bake cake", 3.),
                Mutation::Remove { key: "b".into() },
            ])
            .unwrap();
        assert!(
            runtime
                .execute("text", json!({"query":"software"}))
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(runtime.execute("total", json!({})).unwrap(), json!(1));
    }
    #[test]
    fn wrong_type_rejects_entire_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Runtime::open(dir.path(), definition()).unwrap();
        let bad = Mutation::Upsert {
            key: "bad".into(),
            data: JsonDocument::try_from_value(json!({"title":4})).unwrap(),
        };
        assert!(r.mutate(&[put("valid", "bake bread", 1.), bad]).is_err());
        assert!(r.get("valid").is_none());
        assert_eq!(r.execute("total", json!({})).unwrap(), json!(0));
    }
    #[test]
    fn quota_failure_after_real_index_flush_rolls_back_search() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Runtime::open_with_limit(dir.path(), definition(), 160).unwrap();
        r.mutate(&[put("a", "bread", 1.)]).unwrap();
        let before = r.execute("semantic", json!({"query":"bread"})).unwrap();
        let digest = r.export(0).source_digest;
        assert!(r.mutate(&[put("b", &"software ".repeat(80), 2.)]).is_err());
        assert_eq!(r.export(0).source_digest, digest);
        assert_eq!(r.execute("total", json!({})).unwrap(), json!(1));
        assert_eq!(
            r.execute("semantic", json!({"query":"bread"})).unwrap(),
            before
        );
        assert!(
            r.execute("text", json!({"query":"software"}))
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
        r.mutate(&[Mutation::Remove { key: "a".into() }]).unwrap();
        assert_eq!(
            r.execute("semantic", json!({"query":"bread"})).unwrap(),
            json!([])
        );
    }
    #[test]
    fn aggregate_overflow_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Runtime::open(dir.path(), definition()).unwrap();
        r.mutate(&[put("a", "bread", 1e154)]).unwrap();
        assert!(r.mutate(&[put("b", "cake", 1e154)]).is_err());
        assert!(r.get("b").is_none());
        assert_eq!(r.execute("total", json!({})).unwrap(), json!(1));
    }
    #[test]
    fn missing_null_predicate_and_projection() {
        let e = Expression::Equals {
            field: "/x".into(),
            value: Value::Null,
        };
        assert!(!predicate(&e, &json!({})));
        assert!(predicate(&e, &json!({"x":null})));
        let dir = tempfile::tempdir().unwrap();
        let mut r = Runtime::open(dir.path(), definition()).unwrap();
        r.mutate(&[put("a", "bread", 1.)]).unwrap();
        assert_eq!(
            r.execute("pending", json!({})).unwrap(),
            json!([{"key":"a","value":{"title":"bread"}}])
        );
    }
    #[test]
    fn source_collision_does_not_bypass_projection() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = definition();
        d.resources
            .get_mut("docs")
            .unwrap()
            .stages
            .push(Stage::Projection {
                fields: BTreeMap::from([("title".into(), "/title".into())]),
            });
        let mut r = Runtime::open(dir.path(), d).unwrap();
        r.mutate(&[put("a", "bread", 1.)]).unwrap();
        assert_eq!(
            r.query(
                "docs",
                Action::Get,
                &Query {
                    key: Some("a".into()),
                    ..Default::default()
                }
            )
            .unwrap(),
            json!({"title":"bread"})
        );
    }
    #[test]
    fn raw_vectors_enforce_dimensions_and_nonzero() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Runtime::open(dir.path(), definition()).unwrap();
        assert!(r.execute("semantic", json!({"vector":[1.,2.]})).is_err());
        assert!(
            r.execute("semantic", json!({"vector":vec![0.;512]}))
                .is_err()
        );
        assert!(
            r.execute("semantic", json!({"query":"bread","vector":vec![1.;512]}))
                .is_err()
        );
        assert!(
            r.execute("semantic", json!({"query":"é".repeat(2049)}))
                .is_err()
        );
        assert!(
            r.execute("semantic", json!({"vector":vec![1.;512]}))
                .is_ok()
        );
    }
    #[test]
    fn definitions_cannot_change_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let d = definition();
        drop(Runtime::open(dir.path(), d.clone()).unwrap());
        let mut changed = d;
        changed.resources.remove("semantic");
        changed.expose.remove("semantic");
        assert!(Runtime::open(dir.path(), changed).is_err());
    }
    #[test]
    fn export_is_bounded_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Runtime::open(dir.path(), Definition::records_v1()).unwrap();
        for n in 0..5 {
            r.mutate(&[Mutation::Upsert {
                key: format!("{n}"),
                data: JsonDocument::try_from_value(json!({"text":"x".repeat(200000)})).unwrap(),
            }])
            .unwrap();
        }
        let a = r.export(0);
        assert!(a.records.len() < 5);
        let b = r.export(a.next_offset.unwrap());
        assert_eq!(a.source_digest, b.source_digest);
        assert_eq!(a.records.len() + b.records.len(), 5);
        assert!(b.next_offset.is_none());
    }
    #[test]
    fn projection_growth_rejected_before_any_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let mut d = Definition::records_v1();
        d.resources
            .get_mut("docs")
            .unwrap()
            .stages
            .push(Stage::Projection {
                fields: BTreeMap::from([
                    ("a".into(), "/text".into()),
                    ("b".into(), "/text".into()),
                ]),
            });
        let mut r = Runtime::open(dir.path(), d).unwrap();
        assert!(
            r.mutate(&[Mutation::Upsert {
                key: "large".into(),
                data: JsonDocument::try_from_value(json!({"text":"x".repeat(150000)})).unwrap()
            }])
            .is_err()
        );
        assert!(r.get("large").is_none());
    }
    #[test]
    fn lowered_source_quota_still_allows_shrinking() {
        let dir = tempfile::tempdir().unwrap();
        let d = Definition::records_v1();
        let mut r = Runtime::open(dir.path(), d.clone()).unwrap();
        r.mutate(&[put("a", "bread", 1.), put("b", "cake", 2.)])
            .unwrap();
        drop(r);
        let mut r = Runtime::open_with_limit(dir.path(), d, 1).unwrap();
        r.mutate(&[Mutation::Remove { key: "a".into() }]).unwrap();
        assert!(r.get("b").is_some());
    }
    #[test]
    fn tiny_vector_norm_is_rejected() {
        assert!(pipeline::validate_vector(&vec![1e-30; 512]).is_err());
    }
    #[test]
    fn lowered_limits_preserve_existing_data_and_allow_vector_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let d = definition();
        let mut r = Runtime::open(dir.path(), d.clone()).unwrap();
        r.mutate(&[
            put("a", "bread recipe", 1.),
            put("b", "cake recipe", 2.),
            put("c", "pasta recipe", 3.),
        ])
        .unwrap();
        drop(r);
        let limits = Limits {
            resources: 1,
            stages_per_resource: 1,
            vectors: 1,
            text_bytes: 4,
            query_bytes: 4,
            hits: 1,
            ..Limits::default()
        };
        let mut r = Runtime::open_with_limits(dir.path(), d, MAX_LOGICAL_BYTES, limits).unwrap();
        assert_eq!(r.export(0).record_count, 3);
        assert_eq!(
            r.execute("semantic", json!({"query":"food"}))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            r.execute("semantic", json!({"query":"long query"}))
                .is_err()
        );
        assert!(
            r.execute("semantic", json!({"query":"food","limit":2}))
                .is_err()
        );
        assert!(r.mutate(&[put("d", "cake", 4.)]).is_err());
        assert!(r.mutate(&[put("a", "long text", 4.)]).is_err());
        r.mutate(&[Mutation::Remove { key: "a".into() }]).unwrap();
        assert_eq!(r.vector_count(), 2);
        r.mutate(&[Mutation::Remove { key: "b".into() }]).unwrap();
        assert_eq!(r.vector_count(), 1);
        r.mutate(&[put("c", "cake", 4.)]).unwrap();
        assert_eq!(r.vector_count(), 1);
    }
    #[test]
    fn new_definitions_respect_lowered_resource_and_stage_limits() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits {
            resources: 1,
            ..Limits::default()
        };
        assert!(
            Runtime::open_with_limits(dir.path(), definition(), MAX_LOGICAL_BYTES, limits).is_err()
        );
        let limits = Limits {
            stages_per_resource: 1,
            ..Limits::default()
        };
        assert!(
            Runtime::open_with_limits(dir.path(), definition(), MAX_LOGICAL_BYTES, limits).is_err()
        );
    }
    #[test]
    fn same_key_semantic_replacement_updates_distance_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let d = definition();
        let mut r = Runtime::open(dir.path(), d.clone()).unwrap();
        r.mutate(&[put("a", "bread and butter", 1.)]).unwrap();
        let before = r
            .execute("semantic", json!({"query":"computer programming software"}))
            .unwrap()[0]["distance"]
            .as_f64()
            .unwrap();
        r.mutate(&[put("a", "computer programming software", 2.)])
            .unwrap();
        let after = r
            .execute("semantic", json!({"query":"computer programming software"}))
            .unwrap()[0]["distance"]
            .as_f64()
            .unwrap();
        assert!(
            after < 1e-5,
            "exact text must have approximately zero distance, got {after}"
        );
        assert!(before > after + 0.01);
        drop(r);
        let mut r = Runtime::open(dir.path(), d).unwrap();
        let reopened = r
            .execute("semantic", json!({"query":"computer programming software"}))
            .unwrap()[0]["distance"]
            .as_f64()
            .unwrap();
        assert!((after - reopened).abs() < 1e-6);
    }
}
