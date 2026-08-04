use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use mixed_version_contract_gate::{GateStatus, check_schema_pair, run_files};
use serde_json::{Value, json};

#[test]
fn semantic_fixture_has_at_least_sixty_cases_and_matches() {
    let fixture: Value = serde_json::from_str(include_str!("../fixtures/semantic_cases.json"))
        .expect("valid fixture JSON");
    let schemas = fixture["schemas"].as_object().expect("schema catalog");
    let cases = fixture["cases"].as_array().expect("case array");
    assert!(cases.len() >= 60, "semantic suite must retain >=60 cases");
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let producer = &schemas[case["producer"].as_str().expect("producer ref")];
        let consumer = &schemas[case["consumer"].as_str().expect("consumer ref")];
        let outcome = check_schema_pair(producer, consumer)
            .unwrap_or_else(|review| panic!("{name}: unexpected review: {review:?}"));
        match case["expected"].as_str().expect("expected status") {
            "allow" => assert!(outcome.is_none(), "{name}: expected allow, got {outcome:?}"),
            "block" => {
                let (rule, _) = outcome.unwrap_or_else(|| panic!("{name}: expected block"));
                assert_eq!(
                    rule,
                    case["rule"].as_str().expect("expected rule"),
                    "{name}"
                );
            }
            other => panic!("{name}: unknown expected status {other}"),
        }
    }
}

#[test]
fn incremental_candidate_exactly_matches_full_evaluation() {
    let dir = temp_case_dir("parity");
    let (base, topology, fleet, candidate, merged) = mixed_version_inputs();
    write_json(&dir, "base.json", &base);
    write_json(&dir, "topology.json", &topology);
    write_json(&dir, "fleet.json", &fleet);
    write_json(&dir, "candidate.json", &candidate);
    write_json(&dir, "merged.json", &merged);
    write_json(&dir, "empty.json", &json!({"contracts":[]}));

    let incremental = run_case(&dir, "base.json", "candidate.json");
    let full = run_case(&dir, "merged.json", "empty.json");
    assert_eq!(incremental.status, full.status);
    assert_eq!(incremental.evaluated_pairs, full.evaluated_pairs);
    assert_eq!(incremental.issues, full.issues);
    assert_eq!(incremental.review, full.review);
    assert_eq!(incremental.evaluated_pairs, 9);
    assert_eq!(incremental.issues.len(), 3);
}

#[test]
fn shuffled_and_identical_duplicate_inputs_are_stable() {
    let dir = temp_case_dir("shuffle");
    let (base, topology, fleet, candidate, _) = mixed_version_inputs();
    write_json(&dir, "base.json", &base);
    write_json(&dir, "topology.json", &topology);
    write_json(&dir, "fleet.json", &fleet);
    write_json(&dir, "candidate.json", &candidate);
    let expected = run_case(&dir, "base.json", "candidate.json");

    let mut shuffled_contracts = base["contracts"].as_array().unwrap().clone();
    shuffled_contracts.reverse();
    shuffled_contracts.push(shuffled_contracts[0].clone());
    let mut duplicate_relationships = topology["relationships"].as_array().unwrap().clone();
    duplicate_relationships.push(duplicate_relationships[0].clone());
    let mut duplicate_candidates = candidate["contracts"].as_array().unwrap().clone();
    duplicate_candidates.push(duplicate_candidates[0].clone());
    let shuffled_fleet = json!({"services":{"consumer":[3,2,1,3],"producer":[2,1,3,2]}});

    write_json(
        &dir,
        "base-shuffled.json",
        &json!({"contracts":shuffled_contracts}),
    );
    write_json(
        &dir,
        "topology.json",
        &json!({"relationships":duplicate_relationships}),
    );
    write_json(&dir, "fleet.json", &shuffled_fleet);
    write_json(
        &dir,
        "candidate-shuffled.json",
        &json!({"contracts":duplicate_candidates}),
    );
    let actual = run_case(&dir, "base-shuffled.json", "candidate-shuffled.json");
    assert_eq!(expected, actual);
}

#[test]
fn unsupported_and_malformed_contracts_never_allow() {
    let dir = temp_case_dir("review");
    let base = json!({"contracts":[
        {"service":"producer","topic":"orders","version":1,"schema":{"type":"string","pattern":"^[a-z]+$"}},
        {"service":"consumer","topic":"orders","version":1,"schema":{"type":"integer","minimum":10,"maximum":1}}
    ]});
    write_json(&dir, "base.json", &base);
    write_json(
        &dir,
        "topology.json",
        &json!({"relationships":[{"topic":"orders","producer":"producer","consumer":"consumer"}]}),
    );
    write_json(
        &dir,
        "fleet.json",
        &json!({"services":{"producer":[1],"consumer":[1]}}),
    );
    write_json(&dir, "candidate.json", &json!({"contracts":[]}));
    let result = run_case(&dir, "base.json", "candidate.json");
    assert_eq!(result.status, GateStatus::ReviewRequired);
    assert!(
        result
            .review
            .iter()
            .any(|issue| issue.path.ends_with("/pattern"))
    );
    assert!(
        result
            .review
            .iter()
            .any(|issue| issue.message.contains("minimum must not exceed"))
    );
}

#[test]
fn duplicate_json_members_at_every_input_layer_require_review() {
    let cases = [
        ("root", "base.json", r#"{"contracts":[],"contracts":[]}"#),
        (
            "contract",
            "base.json",
            r#"{"contracts":[{"service":"producer","service":"other","topic":"orders","version":1,"schema":{"type":"string"}}]}"#,
        ),
        (
            "schema",
            "base.json",
            r#"{"contracts":[{"service":"producer","topic":"orders","version":1,"schema":{"type":"string","type":"integer"}}]}"#,
        ),
        (
            "topology",
            "topology.json",
            r#"{"relationships":[{"topic":"orders","producer":"producer","producer":"other","consumer":"consumer"}]}"#,
        ),
        (
            "fleet",
            "fleet.json",
            r#"{"services":{"producer":[1],"producer":[1],"consumer":[1]}}"#,
        ),
        (
            "candidate",
            "candidate.json",
            r#"{"contracts":[{"service":"consumer","topic":"orders","version":1,"schema":{"type":"string","type":"integer"}}]}"#,
        ),
    ];

    for (label, filename, raw) in cases {
        let dir = temp_case_dir(label);
        write_json(
            &dir,
            "base.json",
            &json!({"contracts":[
                contract("producer", 1, json!({"type":"string"})),
                contract("consumer", 1, json!({"type":"string"}))
            ]}),
        );
        write_json(
            &dir,
            "topology.json",
            &json!({"relationships":[{
                "topic":"orders","producer":"producer","consumer":"consumer"
            }]}),
        );
        write_json(
            &dir,
            "fleet.json",
            &json!({"services":{"producer":[1],"consumer":[1]}}),
        );
        write_json(&dir, "candidate.json", &json!({"contracts":[]}));
        fs::write(dir.join(filename), raw).expect("write raw duplicate-key JSON");

        let result = run_case(&dir, "base.json", "candidate.json");
        assert_eq!(result.status, GateStatus::ReviewRequired, "{label}");
        assert!(
            result
                .review
                .iter()
                .any(|issue| issue.message.contains("duplicate object member")),
            "{label}: {:?}",
            result.review
        );
    }
}

fn mixed_version_inputs() -> (Value, Value, Value, Value, Value) {
    let base_schema = schema(false);
    let candidate_schema = schema(true);
    let mut contracts = Vec::new();
    for version in 1..=3 {
        contracts.push(contract("producer", version, base_schema.clone()));
        contracts.push(contract("consumer", version, base_schema.clone()));
    }
    let base = json!({"contracts":contracts});
    let topology = json!({"relationships":[{
        "topic":"orders","producer":"producer","consumer":"consumer"
    }]});
    let fleet = json!({"services":{"producer":[1,2,3],"consumer":[1,2,3]}});
    let replacement = contract("consumer", 3, candidate_schema);
    let candidate = json!({"contracts":[replacement.clone()]});
    let mut merged_contracts = base["contracts"].as_array().unwrap().clone();
    let index = merged_contracts
        .iter()
        .position(|entry| entry["service"] == "consumer" && entry["version"] == 3)
        .unwrap();
    merged_contracts[index] = replacement;
    let merged = json!({"contracts":merged_contracts});
    (base, topology, fleet, candidate, merged)
}

fn schema(require_region: bool) -> Value {
    json!({
        "type":"object",
        "properties":{
            "id":{"type":"integer","minimum":0,"maximum":100},
            "region":{"type":"string","enum":["eu","us"]}
        },
        "required": if require_region { json!(["id","region"]) } else { json!(["id"]) },
        "additionalProperties":false
    })
}

fn contract(service: &str, version: u32, schema: Value) -> Value {
    json!({"service":service,"topic":"orders","version":version,"schema":schema})
}

fn temp_case_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "contract-gate-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create temp case");
    path
}

fn write_json(dir: &Path, name: &str, value: &Value) {
    fs::write(
        dir.join(name),
        serde_json::to_vec(value).expect("serialize test input"),
    )
    .expect("write test input");
}

fn run_case(
    dir: &Path,
    contracts: &str,
    candidate: &str,
) -> mixed_version_contract_gate::GateResult {
    run_files(
        &dir.join(contracts).to_string_lossy(),
        &dir.join("topology.json").to_string_lossy(),
        &dir.join("fleet.json").to_string_lossy(),
        &dir.join(candidate).to_string_lossy(),
    )
}
