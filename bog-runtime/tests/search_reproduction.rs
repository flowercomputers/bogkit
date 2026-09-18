use bog_definition::{Action, Definition};
use bog_runtime::{JsonDocument, Mutation, Query, Runtime};
use serde_json::{Value, json};
fn definition(fields: Vec<&str>) -> Definition {
    serde_json::from_value(json!({"resources": {
        "docs":{"terminal":{"kind":"table"}},
        "semantic":{"terminal":{"kind":"semantic","fields":fields}},
        "text":{"terminal":{"kind":"bm25","fields":fields}}
    },"expose":{"semantic":{"target":"semantic","action":"search"},"text":{"target":"text","action":"search"}}})).unwrap()
}
fn put(key: &str, value: Value) -> Mutation {
    Mutation::Upsert {
        key: key.into(),
        data: JsonDocument::try_from_value(value).unwrap(),
    }
}
fn search(r: &Runtime, resource: &str, query: &str) -> Value {
    r.query(
        resource,
        Action::Search,
        &Query {
            query: Some(query.into()),
            limit: Some(3),
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn exact_cowork_and_scatter_model_reproduction_and_freshness() {
    let dir = tempfile::tempdir().unwrap();
    let d = definition(vec!["/text"]);
    let mut r = Runtime::open(dir.path(), d.clone()).unwrap();
    let m1 = json!({"user":"ed","text":"anyone want to grab ramen tonight","ts":1});
    r.mutate(&[
        put("m1", m1.clone()),
        put(
            "m2",
            json!({"user":"ash","text":"the shop was slammed today, new jackets sold out","ts":2}),
        ),
        put(
            "m3",
            json!({"user":"ed","text":"deploying the database fix now","ts":3}),
        ),
    ])
    .unwrap();
    let queries = ["dinner plans", "noodles for dinner"];
    let initial: Vec<_> = queries.iter().map(|q| search(&r, "semantic", q)).collect();
    println!(
        "encoder={} identity={:?}",
        ese::ENCODER_ID,
        ese::ENCODER_IDENTITY
    );
    for (q, hits) in queries.iter().zip(&initial) {
        println!(
            "Cowork query={q:?} semantic={hits} bm25={}",
            search(&r, "text", q)
        );
        assert_eq!(hits.as_array().unwrap().len(), 3);
        // Compare direct encoder cosine scores to the index, proving extraction
        // uses /text alone (user and timestamp do not enter the embedding).
        let query_vector = ese::encode_single(q);
        for hit in hits.as_array().unwrap() {
            let record = r.get(hit["key"].as_str().unwrap()).unwrap();
            let vector = ese::encode_single(record.as_value()["text"].as_str().unwrap());
            let dot: f64 = query_vector
                .iter()
                .zip(&vector)
                .map(|(a, b)| f64::from(*a) * f64::from(*b))
                .sum();
            let qa: f64 = query_vector.iter().map(|a| f64::from(*a).powi(2)).sum();
            let va: f64 = vector.iter().map(|a| f64::from(*a).powi(2)).sum();
            assert!((hit["score"].as_f64().unwrap() - dot / (qa * va).sqrt()).abs() < 0.00001);
        }
    }
    println!("Cowork diagnostics={}", r.search_diagnostics());
    r.mutate(&[put("m1", json!({"text":"mountain telescope observatory"}))])
        .unwrap();
    assert_eq!(
        search(&r, "semantic", "mountain telescope observatory")[0]["key"],
        "m1"
    );
    assert_eq!(search(&r, "text", "ramen"), json!([]));
    r.mutate(&[Mutation::Remove { key: "m1".into() }]).unwrap();
    assert!(
        search(&r, "semantic", "mountain telescope observatory")
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["key"] != "m1")
    );
    r.checkpoint().unwrap();
    drop(r);
    let mut r = Runtime::open(dir.path(), d).unwrap();
    assert!(
        search(&r, "semantic", "mountain telescope observatory")
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["key"] != "m1")
    );
    assert!(
        r.search_diagnostics()
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["searchable_records"] == 2)
    );
    r.mutate(&[put("m1", m1)]).unwrap();
    for (q, hits) in queries.iter().zip(initial) {
        assert_eq!(search(&r, "semantic", q), hits);
    }
    let dir = tempfile::tempdir().unwrap();
    let mut r = Runtime::open(dir.path(), definition(vec!["/title", "/body"])).unwrap();
    r.mutate(&[put("cabin",json!({"title":"A cabin for deep work","body":"An isolated shelter among trees, away from interruptions."}))]).unwrap();
    let hits = search(&r, "semantic", "quiet woodland retreat");
    println!(
        "Scatter semantic={hits} bm25={}",
        search(&r, "text", "quiet woodland retreat")
    );
    assert_eq!(hits[0]["key"], "cabin");
    assert_eq!(search(&r, "text", "quiet woodland retreat"), json!([]));
}
