use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

struct StrictValueSeed;

impl<'de> DeserializeSeed<'de> for StrictValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictValueSeed)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate object member `{key}`"
                )));
            }
            values.insert(key, object.next_value_seed(StrictValueSeed)?);
        }
        Ok(Value::Object(values))
    }
}

fn parse_json_strict(text: &str) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = StrictValueSeed.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Schema {
    kind: Kind,
    default: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Kind {
    String {
        values: Option<BTreeSet<String>>,
    },
    Integer {
        minimum: Option<i64>,
        maximum: Option<i64>,
    },
    Array {
        items: Box<Schema>,
    },
    Object {
        properties: BTreeMap<String, Schema>,
        required: BTreeSet<String>,
        open: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ContractKey {
    service: String,
    topic: String,
    version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Relationship {
    topic: String,
    producer: String,
    consumer: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct PairKey {
    topic: String,
    producer_service: String,
    producer_version: u32,
    consumer_service: String,
    consumer_version: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Violation {
    rule: String,
    path: String,
    witness: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockIssue {
    pub topic: String,
    pub producer_service: String,
    pub producer_version: u32,
    pub consumer_service: String,
    pub consumer_version: u32,
    pub rule: String,
    pub path: String,
    pub witness: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ReviewIssue {
    pub source: String,
    pub path: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateStatus {
    Allow,
    Block,
    ReviewRequired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    pub status: GateStatus,
    pub evaluated_pairs: usize,
    pub contract_count: usize,
    pub candidate_count: usize,
    pub issues: Vec<BlockIssue>,
    pub review: Vec<ReviewIssue>,
}

#[derive(Default)]
struct ParsedInput {
    contracts: BTreeMap<ContractKey, Schema>,
    relationships: BTreeSet<Relationship>,
    fleet: BTreeMap<String, BTreeSet<u32>>,
    review: BTreeSet<ReviewIssue>,
}

pub fn run_files(contracts: &str, topology: &str, fleet: &str, candidate: &str) -> GateResult {
    let mut parsed = ParsedInput::default();
    let base_value = read_json(contracts, "contracts.json", &mut parsed.review);
    let topology_value = read_json(topology, "topology.json", &mut parsed.review);
    let fleet_value = read_json(fleet, "fleet.json", &mut parsed.review);
    let candidate_value = read_json(candidate, "candidate.json", &mut parsed.review);

    if let Some(value) = base_value {
        parsed.contracts = parse_contract_file(value, "contracts.json", &mut parsed.review);
    }
    if let Some(value) = topology_value {
        parsed.relationships = parse_topology(value, &mut parsed.review);
    }
    if let Some(value) = fleet_value {
        parsed.fleet = parse_fleet(value, &mut parsed.review);
    }
    let candidates = candidate_value.map_or_else(BTreeMap::new, |value| {
        parse_contract_file(value, "candidate.json", &mut parsed.review)
    });

    if !parsed.review.is_empty() {
        return GateResult {
            status: GateStatus::ReviewRequired,
            evaluated_pairs: 0,
            contract_count: parsed.contracts.len(),
            candidate_count: candidates.len(),
            issues: Vec::new(),
            review: parsed.review.into_iter().collect(),
        };
    }

    let mut reference_review = BTreeSet::new();
    validate_references(&parsed, &candidates, &mut reference_review);
    parsed.review.extend(reference_review);
    if !parsed.review.is_empty() {
        return GateResult {
            status: GateStatus::ReviewRequired,
            evaluated_pairs: 0,
            contract_count: parsed.contracts.len(),
            candidate_count: candidates.len(),
            issues: Vec::new(),
            review: parsed.review.into_iter().collect(),
        };
    }

    let base_pairs = evaluate_map(&parsed.contracts, &parsed.relationships, &parsed.fleet);
    let evaluated_pairs = base_pairs.len();
    let final_pairs = evaluate_incremental(
        base_pairs,
        &parsed.contracts,
        &candidates,
        &parsed.relationships,
        &parsed.fleet,
    );
    let issues = final_pairs.into_values().flatten().collect::<Vec<_>>();
    GateResult {
        status: if issues.is_empty() {
            GateStatus::Allow
        } else {
            GateStatus::Block
        },
        evaluated_pairs,
        contract_count: parsed.contracts.len(),
        candidate_count: candidates.len(),
        issues,
        review: Vec::new(),
    }
}

fn read_json(path: &str, source: &str, review: &mut BTreeSet<ReviewIssue>) -> Option<Value> {
    match fs::read_to_string(path) {
        Ok(text) => match parse_json_strict(&text) {
            Ok(value) => Some(value),
            Err(error) => {
                review.insert(ReviewIssue {
                    source: source.to_string(),
                    path: format!("line {}, column {}", error.line(), error.column()),
                    message: format!("malformed JSON: {error}"),
                });
                None
            }
        },
        Err(error) => {
            review.insert(ReviewIssue {
                source: source.to_string(),
                path: "$".to_string(),
                message: format!("cannot read input: {error}"),
            });
            None
        }
    }
}

fn review_issue(review: &mut BTreeSet<ReviewIssue>, source: &str, path: &str, message: &str) {
    review.insert(ReviewIssue {
        source: source.to_string(),
        path: path.to_string(),
        message: message.to_string(),
    });
}

fn object_at<'a>(
    value: &'a Value,
    source: &str,
    path: &str,
    review: &mut BTreeSet<ReviewIssue>,
) -> Option<&'a Map<String, Value>> {
    match value.as_object() {
        Some(object) => Some(object),
        None => {
            review_issue(review, source, path, "expected an object");
            None
        }
    }
}

fn allowed_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    source: &str,
    path: &str,
    review: &mut BTreeSet<ReviewIssue>,
) {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            review_issue(
                review,
                source,
                &format!("{path}/{}", pointer(key)),
                &format!("unsupported field or schema keyword `{key}`"),
            );
        }
    }
}

fn parse_contract_file(
    value: Value,
    source: &str,
    review: &mut BTreeSet<ReviewIssue>,
) -> BTreeMap<ContractKey, Schema> {
    let mut out = BTreeMap::new();
    let Some(root) = object_at(&value, source, "$", review) else {
        return out;
    };
    allowed_keys(root, &["contracts"], source, "$", review);
    let Some(entries) = root.get("contracts").and_then(Value::as_array) else {
        review_issue(review, source, "/contracts", "expected an array");
        return out;
    };

    for (index, entry) in entries.iter().enumerate() {
        let fallback = format!("/contracts/{index}");
        let Some(object) = object_at(entry, source, &fallback, review) else {
            continue;
        };
        allowed_keys(
            object,
            &["service", "topic", "version", "schema"],
            source,
            &fallback,
            review,
        );
        let service = required_string(object, "service", source, &fallback, review);
        let topic = required_string(object, "topic", source, &fallback, review);
        let version = required_u32(object, "version", source, &fallback, review);
        let (Some(service), Some(topic), Some(version)) = (service, topic, version) else {
            continue;
        };
        let key = ContractKey {
            service,
            topic,
            version,
        };
        let identity = contract_path(&key);
        let Some(schema_value) = object.get("schema") else {
            review_issue(
                review,
                source,
                &format!("{identity}/schema"),
                "missing schema",
            );
            continue;
        };
        let Some(schema) =
            parse_schema(schema_value, source, &format!("{identity}/schema"), review)
        else {
            continue;
        };
        match out.get(&key) {
            None => {
                out.insert(key, schema);
            }
            Some(existing) if existing == &schema => {}
            Some(_) => review_issue(
                review,
                source,
                &identity,
                "conflicting duplicate contract identity",
            ),
        }
    }
    out
}

fn parse_topology(value: Value, review: &mut BTreeSet<ReviewIssue>) -> BTreeSet<Relationship> {
    let source = "topology.json";
    let mut out = BTreeSet::new();
    let Some(root) = object_at(&value, source, "$", review) else {
        return out;
    };
    allowed_keys(root, &["relationships"], source, "$", review);
    let Some(entries) = root.get("relationships").and_then(Value::as_array) else {
        review_issue(review, source, "/relationships", "expected an array");
        return out;
    };
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("/relationships/{index}");
        let Some(object) = object_at(entry, source, &path, review) else {
            continue;
        };
        allowed_keys(
            object,
            &["topic", "producer", "consumer"],
            source,
            &path,
            review,
        );
        let topic = required_string(object, "topic", source, &path, review);
        let producer = required_string(object, "producer", source, &path, review);
        let consumer = required_string(object, "consumer", source, &path, review);
        if let (Some(topic), Some(producer), Some(consumer)) = (topic, producer, consumer) {
            out.insert(Relationship {
                topic,
                producer,
                consumer,
            });
        }
    }
    out
}

fn parse_fleet(
    value: Value,
    review: &mut BTreeSet<ReviewIssue>,
) -> BTreeMap<String, BTreeSet<u32>> {
    let source = "fleet.json";
    let mut out = BTreeMap::new();
    let Some(root) = object_at(&value, source, "$", review) else {
        return out;
    };
    allowed_keys(root, &["services"], source, "$", review);
    let Some(services) = root.get("services").and_then(Value::as_object) else {
        review_issue(review, source, "/services", "expected an object");
        return out;
    };
    for (service, versions) in services {
        let path = format!("/services/{}", pointer(service));
        let Some(array) = versions.as_array() else {
            review_issue(review, source, &path, "expected an array of versions");
            continue;
        };
        let mut version_set = BTreeSet::new();
        for (index, version) in array.iter().enumerate() {
            match version.as_u64().and_then(|n| u32::try_from(n).ok()) {
                Some(version) if version > 0 => {
                    version_set.insert(version);
                }
                _ => review_issue(
                    review,
                    source,
                    &format!("{path}/{index}"),
                    "version must be a positive 32-bit integer",
                ),
            }
        }
        if version_set.is_empty() {
            review_issue(
                review,
                source,
                &path,
                "service must permit at least one version",
            );
        }
        out.insert(service.clone(), version_set);
    }
    out
}

fn required_string(
    object: &Map<String, Value>,
    key: &str,
    source: &str,
    path: &str,
    review: &mut BTreeSet<ReviewIssue>,
) -> Option<String> {
    match object.get(key).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Some(value.to_string()),
        _ => {
            review_issue(
                review,
                source,
                &format!("{path}/{}", pointer(key)),
                "expected a non-empty string",
            );
            None
        }
    }
}

fn required_u32(
    object: &Map<String, Value>,
    key: &str,
    source: &str,
    path: &str,
    review: &mut BTreeSet<ReviewIssue>,
) -> Option<u32> {
    match object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
    {
        Some(value) if value > 0 => Some(value),
        _ => {
            review_issue(
                review,
                source,
                &format!("{path}/{}", pointer(key)),
                "expected a positive 32-bit integer",
            );
            None
        }
    }
}

fn parse_schema(
    value: &Value,
    source: &str,
    path: &str,
    review: &mut BTreeSet<ReviewIssue>,
) -> Option<Schema> {
    let object = object_at(value, source, path, review)?;
    let Some(kind_name) = object.get("type").and_then(Value::as_str) else {
        review_issue(
            review,
            source,
            &format!("{path}/type"),
            "missing or non-string type",
        );
        return None;
    };
    let default = object.get("default").cloned();
    let kind = match kind_name {
        "string" => {
            allowed_keys(object, &["type", "enum", "default"], source, path, review);
            let values = match object.get("enum") {
                None => None,
                Some(value) => {
                    let Some(array) = value.as_array() else {
                        review_issue(review, source, &format!("{path}/enum"), "expected an array");
                        return None;
                    };
                    let mut values = BTreeSet::new();
                    for (index, value) in array.iter().enumerate() {
                        let Some(value) = value.as_str() else {
                            review_issue(
                                review,
                                source,
                                &format!("{path}/enum/{index}"),
                                "enum members must be strings",
                            );
                            continue;
                        };
                        if !values.insert(value.to_string()) {
                            review_issue(
                                review,
                                source,
                                &format!("{path}/enum/{index}"),
                                "duplicate enum member",
                            );
                        }
                    }
                    if values.is_empty() {
                        review_issue(
                            review,
                            source,
                            &format!("{path}/enum"),
                            "enum must contain at least one string",
                        );
                    }
                    Some(values)
                }
            };
            Kind::String { values }
        }
        "integer" => {
            allowed_keys(
                object,
                &["type", "minimum", "maximum", "default"],
                source,
                path,
                review,
            );
            let minimum = optional_i64(object, "minimum", source, path, review);
            let maximum = optional_i64(object, "maximum", source, path, review);
            if let (Some(minimum), Some(maximum)) = (minimum, maximum)
                && minimum > maximum
            {
                review_issue(review, source, path, "minimum must not exceed maximum");
            }
            Kind::Integer { minimum, maximum }
        }
        "array" => {
            allowed_keys(object, &["type", "items", "default"], source, path, review);
            let Some(items) = object.get("items") else {
                review_issue(
                    review,
                    source,
                    &format!("{path}/items"),
                    "missing items schema",
                );
                return None;
            };
            Kind::Array {
                items: Box::new(parse_schema(
                    items,
                    source,
                    &format!("{path}/items"),
                    review,
                )?),
            }
        }
        "object" => {
            allowed_keys(
                object,
                &[
                    "type",
                    "properties",
                    "required",
                    "additionalProperties",
                    "default",
                ],
                source,
                path,
                review,
            );
            let Some(properties_value) = object.get("properties") else {
                review_issue(
                    review,
                    source,
                    &format!("{path}/properties"),
                    "missing properties object",
                );
                return None;
            };
            let Some(properties_object) = properties_value.as_object() else {
                review_issue(
                    review,
                    source,
                    &format!("{path}/properties"),
                    "expected an object",
                );
                return None;
            };
            let mut properties = BTreeMap::new();
            for (name, property) in properties_object {
                if let Some(schema) = parse_schema(
                    property,
                    source,
                    &format!("{path}/properties/{}", pointer(name)),
                    review,
                ) {
                    properties.insert(name.clone(), schema);
                }
            }
            let mut required = BTreeSet::new();
            match object.get("required") {
                None => {}
                Some(value) => {
                    let Some(array) = value.as_array() else {
                        review_issue(
                            review,
                            source,
                            &format!("{path}/required"),
                            "expected an array",
                        );
                        return None;
                    };
                    for (index, value) in array.iter().enumerate() {
                        let Some(name) = value.as_str() else {
                            review_issue(
                                review,
                                source,
                                &format!("{path}/required/{index}"),
                                "required member must be a string",
                            );
                            continue;
                        };
                        if !properties.contains_key(name) {
                            review_issue(
                                review,
                                source,
                                &format!("{path}/required/{index}"),
                                "required member must name a declared property",
                            );
                        }
                        if !required.insert(name.to_string()) {
                            review_issue(
                                review,
                                source,
                                &format!("{path}/required/{index}"),
                                "duplicate required member",
                            );
                        }
                    }
                }
            }
            let open = match object.get("additionalProperties") {
                None => true,
                Some(value) => match value.as_bool() {
                    Some(value) => value,
                    None => {
                        review_issue(
                            review,
                            source,
                            &format!("{path}/additionalProperties"),
                            "only boolean additionalProperties is supported",
                        );
                        return None;
                    }
                },
            };
            Kind::Object {
                properties,
                required,
                open,
            }
        }
        other => {
            review_issue(
                review,
                source,
                &format!("{path}/type"),
                &format!("unsupported schema type `{other}`"),
            );
            return None;
        }
    };
    let schema = Schema { kind, default };
    if let Some(default) = &schema.default
        && !accepts(&schema, default)
    {
        review_issue(
            review,
            source,
            &format!("{path}/default"),
            "default does not satisfy its schema",
        );
    }
    Some(schema)
}

fn optional_i64(
    object: &Map<String, Value>,
    key: &str,
    source: &str,
    path: &str,
    review: &mut BTreeSet<ReviewIssue>,
) -> Option<i64> {
    let value = object.get(key)?;
    match value.as_i64() {
        Some(value) => Some(value),
        None => {
            review_issue(
                review,
                source,
                &format!("{path}/{}", pointer(key)),
                "expected a signed 64-bit integer",
            );
            None
        }
    }
}

fn validate_references(
    parsed: &ParsedInput,
    candidates: &BTreeMap<ContractKey, Schema>,
    review: &mut BTreeSet<ReviewIssue>,
) {
    let mut merged = parsed.contracts.clone();
    merged.extend(candidates.clone());
    for relationship in &parsed.relationships {
        let identity = format!(
            "/relationships/topic={}/producer={}/consumer={}",
            pointer(&relationship.topic),
            pointer(&relationship.producer),
            pointer(&relationship.consumer)
        );
        for service in [&relationship.producer, &relationship.consumer] {
            let Some(versions) = parsed.fleet.get(service) else {
                review_issue(
                    review,
                    "fleet.json",
                    &format!("/services/{}", pointer(service)),
                    "relationship service has no fleet entry",
                );
                continue;
            };
            for version in versions {
                let key = ContractKey {
                    service: service.clone(),
                    topic: relationship.topic.clone(),
                    version: *version,
                };
                if !merged.contains_key(&key) {
                    review_issue(
                        review,
                        "topology.json",
                        &identity,
                        &format!(
                            "missing contract for service `{service}`, topic `{}`, version {version}",
                            relationship.topic
                        ),
                    );
                }
            }
        }
    }
}

fn evaluate_incremental(
    mut base: BTreeMap<PairKey, Option<BlockIssue>>,
    base_contracts: &BTreeMap<ContractKey, Schema>,
    candidates: &BTreeMap<ContractKey, Schema>,
    relationships: &BTreeSet<Relationship>,
    fleet: &BTreeMap<String, BTreeSet<u32>>,
) -> BTreeMap<PairKey, Option<BlockIssue>> {
    if candidates.is_empty() {
        return base;
    }
    let mut merged = base_contracts.clone();
    merged.extend(candidates.clone());
    for (pair, issue) in &mut base {
        let producer_key = ContractKey {
            service: pair.producer_service.clone(),
            topic: pair.topic.clone(),
            version: pair.producer_version,
        };
        let consumer_key = ContractKey {
            service: pair.consumer_service.clone(),
            topic: pair.topic.clone(),
            version: pair.consumer_version,
        };
        if candidates.contains_key(&producer_key) || candidates.contains_key(&consumer_key) {
            *issue = evaluate_pair(pair, &merged);
        }
    }
    // Candidate-only keys may become active if a base contract was absent, although
    // normal reference validation ensures all active keys already existed.
    let full_pair_keys = pair_keys(relationships, fleet);
    for pair in full_pair_keys {
        base.entry(pair.clone())
            .or_insert_with(|| evaluate_pair(&pair, &merged));
    }
    base
}

fn evaluate_map(
    contracts: &BTreeMap<ContractKey, Schema>,
    relationships: &BTreeSet<Relationship>,
    fleet: &BTreeMap<String, BTreeSet<u32>>,
) -> BTreeMap<PairKey, Option<BlockIssue>> {
    pair_keys(relationships, fleet)
        .into_iter()
        .map(|pair| {
            let issue = evaluate_pair(&pair, contracts);
            (pair, issue)
        })
        .collect()
}

fn pair_keys(
    relationships: &BTreeSet<Relationship>,
    fleet: &BTreeMap<String, BTreeSet<u32>>,
) -> BTreeSet<PairKey> {
    let mut out = BTreeSet::new();
    for relationship in relationships {
        let Some(producer_versions) = fleet.get(&relationship.producer) else {
            continue;
        };
        let Some(consumer_versions) = fleet.get(&relationship.consumer) else {
            continue;
        };
        for producer_version in producer_versions {
            for consumer_version in consumer_versions {
                out.insert(PairKey {
                    topic: relationship.topic.clone(),
                    producer_service: relationship.producer.clone(),
                    producer_version: *producer_version,
                    consumer_service: relationship.consumer.clone(),
                    consumer_version: *consumer_version,
                });
            }
        }
    }
    out
}

fn evaluate_pair(pair: &PairKey, contracts: &BTreeMap<ContractKey, Schema>) -> Option<BlockIssue> {
    let producer = contracts.get(&ContractKey {
        service: pair.producer_service.clone(),
        topic: pair.topic.clone(),
        version: pair.producer_version,
    })?;
    let consumer = contracts.get(&ContractKey {
        service: pair.consumer_service.clone(),
        topic: pair.topic.clone(),
        version: pair.consumer_version,
    })?;
    incompatibility(producer, consumer).map(|violation| BlockIssue {
        topic: pair.topic.clone(),
        producer_service: pair.producer_service.clone(),
        producer_version: pair.producer_version,
        consumer_service: pair.consumer_service.clone(),
        consumer_version: pair.consumer_version,
        rule: violation.rule,
        path: violation.path,
        witness: violation.witness,
    })
}

pub fn check_schema_pair(
    producer: &Value,
    consumer: &Value,
) -> Result<Option<(String, Value)>, Vec<ReviewIssue>> {
    let mut review = BTreeSet::new();
    let producer = parse_schema(producer, "producer", "/schema", &mut review);
    let consumer = parse_schema(consumer, "consumer", "/schema", &mut review);
    if !review.is_empty() {
        return Err(review.into_iter().collect());
    }
    let violation = incompatibility(
        producer.as_ref().expect("parsed producer"),
        consumer.as_ref().expect("parsed consumer"),
    );
    Ok(violation.map(|violation| (violation.rule, violation.witness)))
}

fn incompatibility(producer: &Schema, consumer: &Schema) -> Option<Violation> {
    let mut candidates = Vec::new();
    compatibility_candidates(producer, consumer, "", &mut candidates);
    candidates.retain(|candidate| {
        accepts(producer, &candidate.witness) && !accepts(consumer, &candidate.witness)
    });
    candidates.into_iter().min_by(compare_violation)
}

fn compatibility_candidates(
    producer: &Schema,
    consumer: &Schema,
    path: &str,
    out: &mut Vec<Violation>,
) {
    match (&producer.kind, &consumer.kind) {
        (Kind::String { values: producer }, Kind::String { values: consumer }) => {
            if let Some(consumer) = consumer {
                let witness = match producer {
                    Some(producer) => producer
                        .iter()
                        .filter(|value| !consumer.contains(*value))
                        .map(|value| Value::String(value.clone()))
                        .min_by(compare_value),
                    None => Some(Value::String(smallest_unlisted_string(consumer))),
                };
                if let Some(witness) = witness {
                    out.push(Violation {
                        rule: "enum-value-rejected".to_string(),
                        path: path_or_root(path),
                        witness,
                    });
                }
            }
        }
        (
            Kind::Integer {
                minimum: producer_min,
                maximum: producer_max,
            },
            Kind::Integer {
                minimum: consumer_min,
                maximum: consumer_max,
            },
        ) => {
            if let Some(consumer_min) = consumer_min
                && producer_min.is_none_or(|minimum| minimum < *consumer_min)
            {
                let low = producer_min.unwrap_or(i64::MIN);
                let high = producer_max
                    .unwrap_or(i64::MAX)
                    .min(consumer_min.saturating_sub(1));
                if low <= high {
                    out.push(Violation {
                        rule: "integer-below-minimum".to_string(),
                        path: path_or_root(path),
                        witness: json!(representative_integer(low, high)),
                    });
                }
            }
            if let Some(consumer_max) = consumer_max
                && producer_max.is_none_or(|maximum| maximum > *consumer_max)
            {
                let low = producer_min
                    .unwrap_or(i64::MIN)
                    .max(consumer_max.saturating_add(1));
                let high = producer_max.unwrap_or(i64::MAX);
                if low <= high {
                    out.push(Violation {
                        rule: "integer-above-maximum".to_string(),
                        path: path_or_root(path),
                        witness: json!(representative_integer(low, high)),
                    });
                }
            }
        }
        (Kind::Array { items: producer }, Kind::Array { items: consumer }) => {
            let mut nested = Vec::new();
            compatibility_candidates(producer, consumer, "/0", &mut nested);
            for candidate in nested {
                out.push(Violation {
                    rule: candidate.rule,
                    path: join_path(path, &candidate.path),
                    witness: Value::Array(vec![candidate.witness]),
                });
            }
        }
        (
            Kind::Object {
                properties: producer_properties,
                required: producer_required,
                open: producer_open,
            },
            Kind::Object {
                properties: consumer_properties,
                required: consumer_required,
                open: consumer_open,
            },
        ) => {
            let base = minimum_object(producer);
            for name in consumer_required {
                let consumer_property = &consumer_properties[name];
                let producer_must_emit = producer_required.contains(name)
                    && producer_properties
                        .get(name)
                        .is_some_and(|schema| schema.default.is_none());
                if consumer_property.default.is_none() && !producer_must_emit {
                    let mut witness = base.clone();
                    witness.remove(name);
                    out.push(Violation {
                        rule: "required-field-missing".to_string(),
                        path: join_path(path, &format!("/{}", pointer(name))),
                        witness: Value::Object(witness),
                    });
                }
            }

            for (name, producer_property) in producer_properties {
                if let Some(consumer_property) = consumer_properties.get(name) {
                    let mut nested = Vec::new();
                    compatibility_candidates(
                        producer_property,
                        consumer_property,
                        &format!("/{}", pointer(name)),
                        &mut nested,
                    );
                    for candidate in nested {
                        let mut witness = base.clone();
                        witness.insert(name.clone(), candidate.witness);
                        out.push(Violation {
                            rule: candidate.rule,
                            path: join_path(path, &candidate.path),
                            witness: Value::Object(witness),
                        });
                    }
                } else if !consumer_open {
                    let mut witness = base.clone();
                    witness.insert(name.clone(), minimum_value(producer_property));
                    out.push(Violation {
                        rule: "closed-object-rejects-property".to_string(),
                        path: join_path(path, &format!("/{}", pointer(name))),
                        witness: Value::Object(witness),
                    });
                }
            }

            if *producer_open {
                if !consumer_open {
                    let name = smallest_unknown_key(producer_properties, consumer_properties);
                    let mut witness = base.clone();
                    witness.insert(name.clone(), Value::Null);
                    out.push(Violation {
                        rule: "closed-object-rejects-property".to_string(),
                        path: join_path(path, &format!("/{}", pointer(&name))),
                        witness: Value::Object(witness),
                    });
                }
                for name in consumer_properties.keys() {
                    if !producer_properties.contains_key(name) {
                        let mut witness = base.clone();
                        witness.insert(name.clone(), Value::Null);
                        out.push(Violation {
                            rule: "open-object-property-unconstrained".to_string(),
                            path: join_path(path, &format!("/{}", pointer(name))),
                            witness: Value::Object(witness),
                        });
                    }
                }
            }
        }
        _ => out.push(Violation {
            rule: "type-mismatch".to_string(),
            path: path_or_root(path),
            witness: minimum_value(producer),
        }),
    }
}

fn accepts(schema: &Schema, value: &Value) -> bool {
    match &schema.kind {
        Kind::String { values } => value
            .as_str()
            .is_some_and(|value| values.as_ref().is_none_or(|values| values.contains(value))),
        Kind::Integer { minimum, maximum } => value.as_i64().is_some_and(|value| {
            minimum.is_none_or(|minimum| value >= minimum)
                && maximum.is_none_or(|maximum| value <= maximum)
        }),
        Kind::Array { items } => value
            .as_array()
            .is_some_and(|values| values.iter().all(|value| accepts(items, value))),
        Kind::Object {
            properties,
            required,
            open,
        } => value.as_object().is_some_and(|object| {
            required.iter().all(|name| {
                object.contains_key(name)
                    || properties
                        .get(name)
                        .is_some_and(|schema| schema.default.is_some())
            }) && object.iter().all(|(name, value)| {
                properties
                    .get(name)
                    .map_or(*open, |schema| accepts(schema, value))
            })
        }),
    }
}

fn minimum_value(schema: &Schema) -> Value {
    match &schema.kind {
        Kind::String { values } => values.as_ref().map_or_else(
            || Value::String(String::new()),
            |values| {
                values
                    .iter()
                    .map(|value| Value::String(value.clone()))
                    .min_by(compare_value)
                    .expect("validated non-empty enum")
            },
        ),
        Kind::Integer { minimum, maximum } => json!(representative_integer(
            minimum.unwrap_or(i64::MIN),
            maximum.unwrap_or(i64::MAX)
        )),
        Kind::Array { .. } => Value::Array(Vec::new()),
        Kind::Object { .. } => Value::Object(minimum_object(schema)),
    }
}

fn minimum_object(schema: &Schema) -> Map<String, Value> {
    let Kind::Object {
        properties,
        required,
        ..
    } = &schema.kind
    else {
        unreachable!()
    };
    required
        .iter()
        .filter_map(|name| {
            let property = &properties[name];
            property
                .default
                .is_none()
                .then(|| (name.clone(), minimum_value(property)))
        })
        .collect()
}

fn representative_integer(low: i64, high: i64) -> i64 {
    let mut candidates = vec![low, high];
    if low <= 0 && high >= 0 {
        candidates.push(0);
    }
    if low <= -1 && high >= -1 {
        candidates.push(-1);
    }
    if low <= 1 && high >= 1 {
        candidates.push(1);
    }
    candidates
        .into_iter()
        .min_by(|left, right| compare_value(&json!(left), &json!(right)))
        .expect("non-empty integer interval")
}

fn smallest_unlisted_string(values: &BTreeSet<String>) -> String {
    if !values.contains("") {
        return String::new();
    }
    for length in 1.. {
        let candidate = "a".repeat(length);
        if !values.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn smallest_unknown_key(
    producer: &BTreeMap<String, Schema>,
    consumer: &BTreeMap<String, Schema>,
) -> String {
    if !producer.contains_key("") && !consumer.contains_key("") {
        return String::new();
    }
    for length in 1.. {
        let candidate = "a".repeat(length);
        if !producer.contains_key(&candidate) && !consumer.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn compare_violation(left: &Violation, right: &Violation) -> Ordering {
    compare_value(&left.witness, &right.witness)
        .then_with(|| left.rule.cmp(&right.rule))
        .then_with(|| left.path.cmp(&right.path))
}

fn compare_value(left: &Value, right: &Value) -> Ordering {
    value_nodes(left)
        .cmp(&value_nodes(right))
        .then_with(|| canonical_json(left).len().cmp(&canonical_json(right).len()))
        .then_with(|| canonical_json(left).cmp(&canonical_json(right)))
}

fn value_nodes(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(value_nodes).sum::<usize>(),
        Value::Object(values) => 1 + values.values().map(value_nodes).sum::<usize>(),
        _ => 1,
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let body = object
                .iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("string serialization"),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).expect("value serialization"),
    }
}

fn path_or_root(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn join_path(prefix: &str, suffix: &str) -> String {
    if prefix.is_empty() {
        path_or_root(suffix)
    } else if suffix == "/" {
        prefix.to_string()
    } else {
        format!("{prefix}{suffix}")
    }
}

fn pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn contract_path(key: &ContractKey) -> String {
    format!(
        "/contracts/service={}/topic={}/version={}",
        pointer(&key.service),
        pointer(&key.topic),
        key.version
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_keyword_has_exact_semantic_location() {
        let mut review = BTreeSet::new();
        let input = json!({"contracts":[{
            "service":"a", "topic":"t", "version":1,
            "schema":{"type":"string", "pattern":"x"}
        }]});
        parse_contract_file(input, "contracts.json", &mut review);
        let issue = review.into_iter().next().expect("review issue");
        assert_eq!(
            issue.path,
            "/contracts/service=a/topic=t/version=1/schema/pattern"
        );
    }

    #[test]
    fn malformed_default_needs_review() {
        let result = check_schema_pair(
            &json!({"type":"integer", "minimum":1, "default":0}),
            &json!({"type":"integer"}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn witness_is_accepted_only_by_producer() {
        let producer = json!({"type":"object","properties":{"n":{"type":"integer","minimum":0,"maximum":10}},"required":["n"],"additionalProperties":false});
        let consumer = json!({"type":"object","properties":{"n":{"type":"integer","minimum":1,"maximum":10}},"required":["n"],"additionalProperties":false});
        let (rule, witness) = check_schema_pair(&producer, &consumer)
            .expect("valid")
            .expect("block");
        assert_eq!(rule, "integer-below-minimum");
        let mut review = BTreeSet::new();
        let producer = parse_schema(&producer, "p", "/", &mut review).unwrap();
        let consumer = parse_schema(&consumer, "c", "/", &mut review).unwrap();
        assert!(accepts(&producer, &witness));
        assert!(!accepts(&consumer, &witness));
    }
}
