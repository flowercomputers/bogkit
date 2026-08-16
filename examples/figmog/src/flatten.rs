//! Pure flattening of a Figma file response into deterministic records.
//!
//! No I/O, no clock, no randomness: two calls on equal JSON must produce
//! byte-identical records (postcard), because `KeyedStream::upsert` uses
//! byte equality as its change detector.

use serde_json::Value;

use crate::model::{Id, NodeRec, Rec};

/// File-level fields lifted from the response envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct FileInfo {
    pub name: String,
    pub version: String,
    pub last_modified: String,
}

/// Everything `flatten_file` extracts.
#[derive(Debug)]
pub struct Flattened {
    pub recs: Vec<(Id, Rec)>,
    pub file: FileInfo,
}

#[derive(Debug, thiserror::Error)]
pub enum FlattenError {
    #[error("missing field: {0}")]
    Missing(&'static str),
}

/// Flatten a full `GET /v1/files/:key` response.
pub fn flatten_file(resp: &Value) -> Result<Flattened, FlattenError> {
    let file = FileInfo {
        name: str_field(resp, "name").ok_or(FlattenError::Missing("name"))?,
        version: str_field(resp, "version").ok_or(FlattenError::Missing("version"))?,
        last_modified: str_field(resp, "lastModified").ok_or(FlattenError::Missing("lastModified"))?,
    };
    let document = resp.get("document").ok_or(FlattenError::Missing("document"))?;

    let mut recs = Vec::new();
    walk(document, None, 0, None, &mut recs);
    Ok(Flattened { recs, file })
}

fn str_field(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(str::to_string)
}

/// Depth-first walk. `page_id` is the nearest CANVAS ancestor (None above
/// pages — the record then carries the node's own id).
fn walk(
    node: &Value,
    parent_id: Option<&str>,
    child_index: u32,
    page_id: Option<&str>,
    out: &mut Vec<(Id, Rec)>,
) {
    let Some(id) = node.get("id").and_then(Value::as_str) else {
        return; // node without id: skip it and its subtree
    };
    let node_type = node
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_string();
    let own_page = matches!(node_type.as_str(), "DOCUMENT" | "CANVAS");
    let page = if own_page { id } else { page_id.unwrap_or(id) };

    let mut raw = node.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.remove("children");
    }

    let rec = NodeRec {
        id: id.to_string(),
        parent_id: parent_id.map(str::to_string),
        child_index,
        page_id: page.to_string(),
        node_type,
        name: str_field(node, "name").unwrap_or_default(),
        visible: node.get("visible").and_then(Value::as_bool).unwrap_or(true),
        text: str_field(node, "characters"),
        component_id: None,
        component_properties: Vec::new(),
        property_definitions: None,
        style_refs: Vec::new(),
        bound_variables: Vec::new(),
        abs_bounds: node.get("absoluteBoundingBox").and_then(|b| {
            Some([
                b.get("x")?.as_f64()?,
                b.get("y")?.as_f64()?,
                b.get("width")?.as_f64()?,
                b.get("height")?.as_f64()?,
            ])
        }),
        raw: serde_json::to_string(&raw).expect("serde_json::Value serializes"),
    };
    out.push((Id::Node(id.to_string()), Rec::Node(rec)));

    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for (i, child) in children.iter().enumerate() {
            walk(child, Some(id), i as u32, Some(page), out);
        }
    }
}
