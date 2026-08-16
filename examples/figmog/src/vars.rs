//! Variables: authoritative import parsing (this module also hosts the
//! free-plan inference in `infer`).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::{Id, Rec, VariableCollectionRec, VariableRec};

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("unrecognized variables export shape: {0}")]
    Shape(String),
}

/// Parse a variables export: either the Enterprise REST `variables/local`
/// response (`{meta: {variables, variableCollections}}`) or the bare
/// object a plugin-console export produces.
pub fn parse_variables_export(v: &Value) -> Result<Vec<(Id, Rec)>, ImportError> {
    let root = v.get("meta").unwrap_or(v);
    let variables = root
        .get("variables")
        .and_then(Value::as_object)
        .ok_or_else(|| ImportError::Shape("missing `variables` object".into()))?;
    let collections = root
        .get("variableCollections")
        .and_then(Value::as_object)
        .ok_or_else(|| ImportError::Shape("missing `variableCollections` object".into()))?;

    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();

    let mut recs = Vec::new();
    let sorted: BTreeMap<_, _> = collections.iter().collect();
    for (id, c) in sorted {
        let mut modes: Vec<(String, String)> = c
            .get("modes")
            .and_then(Value::as_array)
            .map(|ms| ms.iter().map(|m| (s(m, "modeId"), s(m, "name"))).collect())
            .unwrap_or_default();
        modes.sort();
        recs.push((
            Id::VariableCollection(id.clone()),
            Rec::VariableCollection(VariableCollectionRec {
                id: id.clone(),
                name: s(c, "name"),
                modes,
                default_mode_id: s(c, "defaultModeId"),
            }),
        ));
    }
    let sorted: BTreeMap<_, _> = variables.iter().collect();
    for (id, var) in sorted {
        let mut values_by_mode: Vec<(String, String)> = var
            .get("valuesByMode")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(mode, val)| {
                        (mode.clone(), serde_json::to_string(val).expect("Value serializes"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        values_by_mode.sort();
        let scopes: Vec<String> = var
            .get("scopes")
            .and_then(Value::as_array)
            .map(|xs| xs.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        recs.push((
            Id::Variable(id.clone()),
            Rec::Variable(VariableRec {
                id: id.clone(),
                name: s(var, "name"),
                resolved_type: s(var, "resolvedType"),
                collection_id: s(var, "variableCollectionId"),
                values_by_mode,
                description: s(var, "description"),
                scopes,
            }),
        ));
    }
    Ok(recs)
}
