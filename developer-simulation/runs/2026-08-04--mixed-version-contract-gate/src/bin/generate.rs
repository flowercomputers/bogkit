use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 2 || !matches!(args[0].as_str(), "demo" | "workload") {
        eprintln!("usage: generate <demo|workload> <output-directory>");
        return ExitCode::from(2);
    }
    let output = Path::new(&args[1]);
    if let Err(error) = fs::create_dir_all(output) {
        eprintln!("cannot create {}: {error}", output.display());
        return ExitCode::from(2);
    }
    let files = if args[0] == "demo" {
        demo_files()
    } else {
        workload_files()
    };
    for (name, value) in files {
        let path = output.join(name);
        let data = serde_json::to_vec_pretty(&value).expect("generated JSON is serializable");
        if let Err(error) = fs::write(&path, data) {
            eprintln!("cannot write {}: {error}", path.display());
            return ExitCode::from(2);
        }
    }
    println!("generated {} data in {}", args[0], output.display());
    ExitCode::SUCCESS
}

fn demo_files() -> Vec<(&'static str, Value)> {
    let producer = object_schema(false, false);
    let consumer = object_schema(false, false);
    let candidate = object_schema(true, false);
    let mut contracts = Vec::new();
    for version in 1..=3 {
        contracts.push(contract("producer", "orders", version, producer.clone()));
        contracts.push(contract("consumer", "orders", version, consumer.clone()));
    }
    vec![
        ("contracts.json", json!({"contracts": contracts})),
        (
            "topology.json",
            json!({"relationships":[{"topic":"orders","producer":"producer","consumer":"consumer"}]}),
        ),
        (
            "fleet.json",
            json!({"services":{"producer":[1,2,3],"consumer":[1,2,3]}}),
        ),
        (
            "candidate.json",
            json!({"contracts":[contract("consumer", "orders", 3, candidate)]}),
        ),
    ]
}

fn workload_files() -> Vec<(&'static str, Value)> {
    let mut memberships = vec![BTreeSet::<usize>::new(); 300];
    for topics in memberships.iter_mut().take(150) {
        topics.insert(0);
    }
    for (service, topics) in memberships.iter_mut().enumerate() {
        topics.insert(1 + service % 119);
    }
    for (service, topics) in memberships.iter_mut().enumerate().take(150) {
        topics.insert(1 + (service + 37) % 119);
    }
    assert_eq!(memberships.iter().map(BTreeSet::len).sum::<usize>(), 600);

    let base_schema = object_schema(false, true);
    let mut contracts = Vec::with_capacity(1_800);
    for (service, topics) in memberships.iter().enumerate() {
        for topic in topics {
            for version in 1..=3 {
                contracts.push(contract(
                    &format!("svc{service:03}"),
                    &format!("topic{topic:03}"),
                    version,
                    base_schema.clone(),
                ));
            }
        }
    }
    assert_eq!(contracts.len(), 1_800);

    let mut relationships = Vec::with_capacity(12_000);
    // Put every declared topic into the live topology, then fill the remaining
    // relationship budget from the deliberately dense topic000 cohort.
    for topic in 1..120 {
        let members = memberships
            .iter()
            .enumerate()
            .filter_map(|(service, topics)| topics.contains(&topic).then_some(service))
            .take(2)
            .collect::<Vec<_>>();
        assert_eq!(members.len(), 2);
        relationships.push(json!({
            "topic":format!("topic{topic:03}"),
            "producer":format!("svc{:03}", members[0]),
            "consumer":format!("svc{:03}", members[1])
        }));
    }
    'outer: for producer in 0..150 {
        for consumer in 0..150 {
            if producer == consumer {
                continue;
            }
            relationships.push(json!({
                "topic":"topic000",
                "producer":format!("svc{producer:03}"),
                "consumer":format!("svc{consumer:03}")
            }));
            if relationships.len() == 12_000 {
                break 'outer;
            }
        }
    }
    assert_eq!(relationships.len(), 12_000);

    let services = (0..300)
        .map(|service| (format!("svc{service:03}"), json!([1, 2, 3])))
        .collect::<serde_json::Map<_, _>>();

    let mut candidates = vec![contract("svc000", "topic000", 3, object_schema(true, true))];
    for (service, topics) in memberships.iter().enumerate().take(174).skip(150) {
        let topic = *topics.iter().next().expect("service has a topic");
        candidates.push(contract(
            &format!("svc{service:03}"),
            &format!("topic{topic:03}"),
            3,
            base_schema.clone(),
        ));
    }
    assert_eq!(candidates.len(), 25);

    vec![
        ("contracts.json", json!({"contracts": contracts})),
        ("topology.json", json!({"relationships": relationships})),
        ("fleet.json", json!({"services": services})),
        ("candidate.json", json!({"contracts": candidates})),
    ]
}

fn contract(service: &str, topic: &str, version: u32, schema: Value) -> Value {
    json!({
        "service":service,
        "topic":topic,
        "version":version,
        "schema":schema
    })
}

fn object_schema(require_region: bool, include_payload: bool) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "id".to_string(),
        json!({"type":"integer","minimum":0,"maximum":1_000_000}),
    );
    properties.insert(
        "region".to_string(),
        json!({"type":"string","enum":["eu","us"]}),
    );
    if include_payload {
        properties.insert("payload".to_string(), json!({"type":"string"}));
    }
    let required = if require_region {
        json!(["id", "region"])
    } else {
        json!(["id"])
    };
    json!({
        "type":"object",
        "properties":properties,
        "required":required,
        "additionalProperties":false
    })
}
