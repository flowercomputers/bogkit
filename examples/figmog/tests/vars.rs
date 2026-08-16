#![recursion_limit = "256"]

mod common;

use figmog::model::{Id, Rec};
use figmog::vars::parse_variables_export;

fn export() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/variables-export.json")).unwrap()
}

#[test]
fn parses_rest_shape() {
    let recs = parse_variables_export(&export()).unwrap();
    // 2 collections then 3 variables, sorted by id
    assert_eq!(recs.len(), 5);
    assert!(matches!(&recs[0].0, Id::VariableCollection(id) if id == "VariableCollectionId:1"));
    let Rec::VariableCollection(c) = &recs[0].1 else { panic!() };
    assert_eq!(c.modes, vec![("1:0".to_string(), "light".to_string()), ("1:1".to_string(), "dark".to_string())]);
    assert_eq!(c.default_mode_id, "1:0");

    let Rec::Variable(v) = &recs[2].1 else { panic!() };
    assert_eq!(v.id, "VariableID:100");
    assert_eq!(v.resolved_type, "COLOR");
    assert_eq!(v.collection_id, "VariableCollectionId:1");
    // values canonical JSON, sorted by mode id; alias kept as-is
    assert_eq!(v.values_by_mode[0].0, "1:0");
    assert!(v.values_by_mode[1].1.contains("VARIABLE_ALIAS"));
    assert_eq!(v.scopes, vec!["FRAME_FILL", "SHAPE_FILL"]);
}

#[test]
fn accepts_bare_shape_and_is_deterministic() {
    let bare = export()["meta"].clone();
    let a = parse_variables_export(&bare).unwrap();
    let b = parse_variables_export(&export()).unwrap();
    assert_eq!(
        postcard::to_allocvec(&a).unwrap(),
        postcard::to_allocvec(&b).unwrap(),
        "both shapes produce byte-identical records"
    );
}

#[test]
fn garbage_is_a_shape_error() {
    assert!(parse_variables_export(&serde_json::json!({"nope": 1})).is_err());
    assert!(parse_variables_export(&serde_json::json!(null)).is_err());
}
