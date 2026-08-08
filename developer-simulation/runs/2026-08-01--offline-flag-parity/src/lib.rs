use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

pub const MAX_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_FLAGS: usize = 5_000;
pub const MAX_RULES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Bool(bool),
    Number(f64),
    String(String),
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Number(value) => write!(f, "{value}"),
            Self::String(value) => write!(f, "{value:?}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    pub config_id: String,
    pub salt: String,
    pub flags: BTreeMap<String, Flag>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flag {
    pub default: Scalar,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub serve: Scalar,
    #[serde(default)]
    pub percentage: Option<Percentage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub attribute: String,
    pub op: Operator,
    pub value: Scalar,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    Eq,
    NotEq,
    GreaterThan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Percentage {
    pub attribute: String,
    pub basis_points: u16,
}

pub type Context = BTreeMap<String, Scalar>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationInput {
    pub case_id: String,
    pub flag: String,
    pub context: Context,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleTrace {
    pub rule_id: String,
    pub matched: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub flag: String,
    pub value: Scalar,
    pub source: String,
    pub explanation: Vec<RuleTrace>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenCase {
    pub input: EvaluationInput,
    pub expected: Decision,
}

#[derive(Debug, Clone)]
pub struct LoadError(String);

impl LoadError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LoadError {}

pub fn load_snapshot_file(path: impl AsRef<Path>) -> Result<Snapshot, LoadError> {
    let path = path.as_ref();
    let bytes = read_snapshot_file_bounded(path)?;
    load_snapshot_bytes(&bytes)
}

fn read_snapshot_file_bounded(path: &Path) -> Result<Vec<u8>, LoadError> {
    let file = File::open(path)
        .map_err(|error| LoadError::new(format!("open {}: {error}", path.display())))?;
    let limit = u64::try_from(MAX_SNAPSHOT_BYTES)
        .expect("snapshot byte limit fits u64")
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| LoadError::new(format!("read {}: {error}", path.display())))?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(LoadError::new(format!(
            "snapshot exceeds {MAX_SNAPSHOT_BYTES} byte file-read limit"
        )));
    }
    Ok(bytes)
}

pub fn load_snapshot_bytes(bytes: &[u8]) -> Result<Snapshot, LoadError> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(LoadError::new(format!(
            "snapshot is {} bytes; limit is {MAX_SNAPSHOT_BYTES}",
            bytes.len()
        )));
    }

    let mut duplicate_check = serde_json::Deserializer::from_slice(bytes);
    UniqueJson::deserialize(&mut duplicate_check)
        .map_err(|error| LoadError::new(format!("invalid JSON: {error}")))?;
    duplicate_check
        .end()
        .map_err(|error| LoadError::new(format!("invalid JSON: {error}")))?;
    let snapshot: Snapshot = serde_json::from_slice(bytes)
        .map_err(|error| LoadError::new(format!("invalid snapshot shape: {error}")))?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

pub fn validate_snapshot(snapshot: &Snapshot) -> Result<(), LoadError> {
    if snapshot.schema_version != 1 {
        return Err(LoadError::new("schema_version must be 1"));
    }
    if snapshot.config_id.trim().is_empty() {
        return Err(LoadError::new("config_id must not be empty"));
    }
    if snapshot.salt.is_empty() {
        return Err(LoadError::new("salt must not be empty"));
    }
    if snapshot.flags.is_empty() || snapshot.flags.len() > MAX_FLAGS {
        return Err(LoadError::new(format!(
            "flag count must be between 1 and {MAX_FLAGS}"
        )));
    }

    let mut rule_count = 0usize;
    for (flag_key, flag) in &snapshot.flags {
        if flag_key.trim().is_empty() {
            return Err(LoadError::new("flag keys must not be empty"));
        }
        rule_count = rule_count
            .checked_add(flag.rules.len())
            .ok_or_else(|| LoadError::new("rule count overflow"))?;
        if rule_count > MAX_RULES {
            return Err(LoadError::new(format!("rule count exceeds {MAX_RULES}")));
        }

        let mut ids = BTreeSet::new();
        for rule in &flag.rules {
            if rule.id.trim().is_empty() {
                return Err(LoadError::new(format!(
                    "flag {flag_key:?} has an empty rule id"
                )));
            }
            if !ids.insert(rule.id.as_str()) {
                return Err(LoadError::new(format!(
                    "flag {flag_key:?} has duplicate rule id {:?}",
                    rule.id
                )));
            }
            if rule.conditions.is_empty() && rule.percentage.is_none() {
                return Err(LoadError::new(format!(
                    "rule {:?} must have a condition or percentage",
                    rule.id
                )));
            }
            for condition in &rule.conditions {
                if condition.attribute.trim().is_empty() {
                    return Err(LoadError::new(format!(
                        "rule {:?} has an empty condition attribute",
                        rule.id
                    )));
                }
                if matches!(condition.op, Operator::GreaterThan)
                    && !matches!(condition.value, Scalar::Number(_))
                {
                    return Err(LoadError::new(format!(
                        "rule {:?} greater_than requires a numeric comparison value",
                        rule.id
                    )));
                }
            }
            if let Some(percentage) = &rule.percentage {
                if percentage.attribute.trim().is_empty() {
                    return Err(LoadError::new(format!(
                        "rule {:?} has an empty percentage attribute",
                        rule.id
                    )));
                }
                if percentage.basis_points > 10_000 {
                    return Err(LoadError::new(format!(
                        "rule {:?} percentage exceeds 10000 basis points",
                        rule.id
                    )));
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Evaluator {
    active: Arc<Snapshot>,
}

impl Evaluator {
    pub fn new(snapshot: Snapshot) -> Self {
        Self {
            active: Arc::new(snapshot),
        }
    }

    pub fn config_id(&self) -> &str {
        &self.active.config_id
    }

    pub fn flag_keys(&self) -> impl Iterator<Item = &str> {
        self.active.flags.keys().map(String::as_str)
    }

    pub fn evaluate(&self, flag_key: &str, context: &Context) -> Result<Decision, String> {
        evaluate_snapshot(&self.active, flag_key, context)
    }

    pub fn reload_bytes(&mut self, bytes: &[u8]) -> Result<(), LoadError> {
        let candidate = load_snapshot_bytes(bytes)?;
        self.active = Arc::new(candidate);
        Ok(())
    }

    pub fn reload_file(&mut self, path: impl AsRef<Path>) -> Result<(), LoadError> {
        let path = path.as_ref();
        let bytes = read_snapshot_file_bounded(path)?;
        self.reload_bytes(&bytes)
    }
}

pub fn evaluate_snapshot(
    snapshot: &Snapshot,
    flag_key: &str,
    context: &Context,
) -> Result<Decision, String> {
    let flag = snapshot
        .flags
        .get(flag_key)
        .ok_or_else(|| format!("unknown flag {flag_key:?}"))?;
    let mut explanation = Vec::with_capacity(flag.rules.len() + 1);

    for rule in &flag.rules {
        let mut failed = None;
        for condition in &rule.conditions {
            match context.get(&condition.attribute) {
                None => {
                    failed = Some(format!("missing attribute {:?}", condition.attribute));
                    break;
                }
                Some(actual) if !condition_matches(actual, condition) => {
                    failed = Some(format!(
                        "attribute {:?} was {actual}; condition did not match",
                        condition.attribute
                    ));
                    break;
                }
                Some(_) => {}
            }
        }

        if let Some(reason) = failed {
            explanation.push(RuleTrace {
                rule_id: rule.id.clone(),
                matched: false,
                reason,
            });
            continue;
        }

        if let Some(percentage) = &rule.percentage {
            let Some(bucket_value) = context.get(&percentage.attribute) else {
                explanation.push(RuleTrace {
                    rule_id: rule.id.clone(),
                    matched: false,
                    reason: format!("missing percentage attribute {:?}", percentage.attribute),
                });
                continue;
            };
            let bucket_key = scalar_bucket_key(bucket_value);
            let bucket = stable_bucket(&snapshot.salt, flag_key, &rule.id, bucket_key.as_bytes());
            if bucket >= percentage.basis_points {
                explanation.push(RuleTrace {
                    rule_id: rule.id.clone(),
                    matched: false,
                    reason: format!(
                        "stable bucket {bucket} was outside 0..{}",
                        percentage.basis_points
                    ),
                });
                continue;
            }
            explanation.push(RuleTrace {
                rule_id: rule.id.clone(),
                matched: true,
                reason: format!(
                    "conditions matched; stable bucket {bucket} was inside 0..{}",
                    percentage.basis_points
                ),
            });
        } else {
            explanation.push(RuleTrace {
                rule_id: rule.id.clone(),
                matched: true,
                reason: "all conditions matched".to_string(),
            });
        }

        return Ok(Decision {
            flag: flag_key.to_string(),
            value: rule.serve.clone(),
            source: rule.id.clone(),
            explanation,
        });
    }

    explanation.push(RuleTrace {
        rule_id: "default".to_string(),
        matched: true,
        reason: "no targeting rule matched".to_string(),
    });
    Ok(Decision {
        flag: flag_key.to_string(),
        value: flag.default.clone(),
        source: "default".to_string(),
        explanation,
    })
}

fn condition_matches(actual: &Scalar, condition: &Condition) -> bool {
    match condition.op {
        Operator::Eq => actual == &condition.value,
        Operator::NotEq => actual != &condition.value,
        Operator::GreaterThan => match (actual, &condition.value) {
            (Scalar::Number(actual), Scalar::Number(expected)) => actual > expected,
            _ => false,
        },
    }
}

fn scalar_bucket_key(value: &Scalar) -> String {
    match value {
        Scalar::Bool(value) => format!("b:{value}"),
        Scalar::Number(value) => format!("n:{value}"),
        Scalar::String(value) => format!("s:{value}"),
    }
}

pub fn stable_bucket(salt: &str, flag: &str, rule: &str, attribute: &[u8]) -> u16 {
    let mut hash = 0xcbf29ce484222325u64;
    for part in [salt.as_bytes(), flag.as_bytes(), rule.as_bytes(), attribute] {
        for byte in part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash % 10_000) as u16
}

/// Rough retained heap estimate for the active parsed snapshot. This excludes
/// allocator bookkeeping and process/runtime overhead; use OS peak RSS as the
/// acceptance measurement.
pub fn estimated_snapshot_heap(snapshot: &Snapshot) -> usize {
    let mut bytes =
        std::mem::size_of::<Snapshot>() + snapshot.config_id.capacity() + snapshot.salt.capacity();
    for (flag_key, flag) in &snapshot.flags {
        bytes += std::mem::size_of::<String>() + flag_key.capacity();
        bytes += std::mem::size_of::<Flag>() + scalar_heap(&flag.default);
        bytes += flag.rules.capacity() * std::mem::size_of::<Rule>();
        for rule in &flag.rules {
            bytes += rule.id.capacity() + scalar_heap(&rule.serve);
            bytes += rule.conditions.capacity() * std::mem::size_of::<Condition>();
            for condition in &rule.conditions {
                bytes += condition.attribute.capacity() + scalar_heap(&condition.value);
            }
            if let Some(percentage) = &rule.percentage {
                bytes += percentage.attribute.capacity();
            }
        }
    }
    bytes
}

fn scalar_heap(value: &Scalar) -> usize {
    match value {
        Scalar::String(value) => value.capacity(),
        Scalar::Bool(_) | Scalar::Number(_) => 0,
    }
}

struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        let _ = value;
        Ok(UniqueJson)
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        let _ = value;
        Ok(UniqueJson)
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        let _ = value;
        Ok(UniqueJson)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.is_finite() {
            Ok(UniqueJson)
        } else {
            Err(E::custom("non-finite JSON number"))
        }
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let _ = value;
        Ok(UniqueJson)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        let _ = value;
        Ok(UniqueJson)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<UniqueJson>()?.is_some() {}
        Ok(UniqueJson)
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            object.next_value::<UniqueJson>()?;
        }
        Ok(UniqueJson)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_json_keys_are_rejected() {
        let bytes = br#"{
            "schema_version":1,
            "config_id":"a",
            "salt":"s",
            "flags":{},
            "flags":{}
        }"#;
        let error = load_snapshot_bytes(bytes).unwrap_err().to_string();
        assert!(error.contains("duplicate JSON object key \"flags\""));
    }

    #[test]
    fn failed_reload_keeps_active_snapshot() {
        let snapshot = one_flag_snapshot();
        let mut evaluator = Evaluator::new(snapshot);
        let before = evaluator.evaluate("checkout", &Context::new()).unwrap();
        assert!(evaluator.reload_bytes(b"not json").is_err());
        assert_eq!(evaluator.config_id(), "one");
        assert_eq!(
            evaluator.evaluate("checkout", &Context::new()).unwrap(),
            before
        );
    }

    #[test]
    fn bucket_has_a_fixed_known_value() {
        assert_eq!(stable_bucket("salt", "flag", "rule", b"user-42"), 7_307);
    }

    #[test]
    fn file_reader_stops_at_the_snapshot_limit() {
        use std::io::{Seek, SeekFrom, Write};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "offline-flag-parity-oversized-{}-{nonce}.json",
            std::process::id()
        ));
        let mut file = File::create(&path).expect("create sparse oversized fixture");
        file.seek(SeekFrom::Start(MAX_SNAPSHOT_BYTES as u64))
            .expect("seek to byte after limit");
        file.write_all(b"x").expect("finish sparse fixture");
        drop(file);

        let error = load_snapshot_file(&path)
            .expect_err("oversized file must fail")
            .to_string();
        std::fs::remove_file(&path).expect("remove sparse fixture");
        assert!(error.contains("file-read limit"), "{error}");
    }

    fn one_flag_snapshot() -> Snapshot {
        Snapshot {
            schema_version: 1,
            config_id: "one".to_string(),
            salt: "salt".to_string(),
            flags: BTreeMap::from([(
                "checkout".to_string(),
                Flag {
                    default: Scalar::Bool(false),
                    rules: Vec::new(),
                },
            )]),
        }
    }
}
