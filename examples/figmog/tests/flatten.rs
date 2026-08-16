#![recursion_limit = "256"]

mod common;

use figmog::flatten::flatten_file;
use figmog::model::{Id, Rec};

fn node(recs: &[(Id, Rec)], id: &str) -> figmog::model::NodeRec {
    recs.iter()
        .find_map(|(k, r)| match (k, r) {
            (Id::Node(n), Rec::Node(rec)) if n == id => Some(rec.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("node {id} not flattened"))
}

#[test]
fn walks_the_whole_tree() {
    let out = flatten_file(&common::fixture_v1()).unwrap();
    let node_ids: Vec<&str> = out
        .recs
        .iter()
        .filter_map(|(k, _)| match k { Id::Node(n) => Some(n.as_str()), _ => None })
        .collect();
    assert_eq!(
        node_ids,
        ["0:0", "0:1", "1:1", "1:2", "1:3", "1:9", "0:2", "2:1", "2:2", "2:3", "3:1", "0:3"],
        "depth-first order, all 12 nodes"
    );
    assert_eq!(out.file.name, "Fixture");
    assert_eq!(out.file.version, "100");
    assert_eq!(out.file.last_modified, "2026-08-01T00:00:00Z");
}

#[test]
fn parent_index_page_attribution() {
    let out = flatten_file(&common::fixture_v1()).unwrap();
    let title = node(&out.recs, "1:2");
    assert_eq!(title.parent_id.as_deref(), Some("1:1"));
    assert_eq!(title.child_index, 0);
    assert_eq!(title.page_id, "0:1");
    let button = node(&out.recs, "1:3");
    assert_eq!(button.child_index, 1);

    let root = node(&out.recs, "0:0");
    assert_eq!(root.parent_id, None);
    assert_eq!(root.page_id, "0:0");
    let canvas = node(&out.recs, "0:2");
    assert_eq!(canvas.page_id, "0:2");
    let variant = node(&out.recs, "2:2");
    assert_eq!(variant.page_id, "0:2");
}

#[test]
fn basic_fields() {
    let out = flatten_file(&common::fixture_v1()).unwrap();
    let title = node(&out.recs, "1:2");
    assert_eq!(title.node_type, "TEXT");
    assert_eq!(title.name, "Title");
    assert!(title.visible);
    assert_eq!(title.text.as_deref(), Some("Welcome to the garden"));

    let hidden = node(&out.recs, "1:9");
    assert!(!hidden.visible);

    let hero = node(&out.recs, "1:1");
    assert_eq!(hero.abs_bounds, Some([0.0, 0.0, 800.0, 400.0]));
}

#[test]
fn raw_is_canonical_and_childless() {
    let out = flatten_file(&common::fixture_v1()).unwrap();
    let hero = node(&out.recs, "1:1");
    let raw: serde_json::Value = serde_json::from_str(&hero.raw).unwrap();
    assert!(raw.get("children").is_none());
    assert_eq!(raw["name"], "Hero");
    // canonical: re-serializing the parsed value reproduces the string
    assert_eq!(serde_json::to_string(&raw).unwrap(), hero.raw);
}

#[test]
fn deterministic_bytes() {
    let a = flatten_file(&common::fixture_v1()).unwrap();
    let b = flatten_file(&common::fixture_v1()).unwrap();
    let enc = |f: &figmog::flatten::Flattened| postcard::to_allocvec(&f.recs).unwrap();
    assert_eq!(enc(&a), enc(&b));
}

#[test]
fn missing_document_errors() {
    assert!(flatten_file(&serde_json::json!({"name": "x"})).is_err());
}
