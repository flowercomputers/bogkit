//! The [`Views`] trait: uniform, JSON-shaped read access to a pipeline's
//! terminal sinks.
//!
//! A fold pipeline's reader mirrors its sink structure — a lone sink yields
//! its reader, tuple branches yield tuples of readers, and operators in
//! between are already erased. Implementing `Views` on each sink reader
//! (fold's types, our trait — the orphan rule allows it) and on tuples of
//! `Views` lets one generic HTTP handler dispatch `GET /views/{name}` to
//! whichever sink claims that name, with no per-pipeline code.

use fold::pipeline::Score;
use fold::pipeline::terminal::{
    BagReader, CountReader, HistogramReader, InvertedIndexReader, MultimapReader, RankedReader,
    StatsReader, TableReader,
};
use fold::stream::Readable;
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// What one sink contributes to the OpenAPI doc and `/schema` fingerprint.
pub struct ViewSpec {
    pub name: String,
    /// `"count"` | `"bag"` | `"table"` | `"bm25"` | `"hnsw"` — decides the
    /// documented response shape.
    pub kind: &'static str,
    /// Whether the view supports point lookup at `/views/{name}/{key}`.
    pub keyed: bool,
    /// How `/views/{name}/search` queries this view, if it is searchable:
    /// `"text"` (GET `?q=`), `"vector"` (POST `{vector}`), or `"text+vector"`.
    pub search: Option<&'static str>,
    /// JSON schema of one item as this view returns it.
    pub item_schema: Value,
}

/// A read request, already routed to `/views/{name}[/{key}|/search]`.
pub struct ViewQuery {
    /// The raw `{key}` path segment, when present.
    pub key: Option<String>,
    /// True when the request came through `/views/{name}/search`.
    pub searching: bool,
    /// Text query (`?q=`), for text-searchable views.
    pub q: Option<String>,
    /// Raw vector (POST body), for vector-searchable views.
    pub vector: Option<Value>,
    /// Max search hits to return.
    pub k: usize,
    pub limit: usize,
    pub offset: usize,
    /// List ordered views highest-first (`?desc=true`). Honored by ordered
    /// views (ranked, histogram); ignored by unordered ones.
    pub desc: bool,
}

impl ViewQuery {
    fn base() -> Self {
        ViewQuery {
            key: None,
            searching: false,
            q: None,
            vector: None,
            k: 10,
            limit: 100,
            offset: 0,
            desc: false,
        }
    }

    /// A paginated listing: `GET /views/{name}`.
    pub fn list(limit: usize, offset: usize, desc: bool) -> Self {
        ViewQuery {
            limit,
            offset,
            desc,
            ..Self::base()
        }
    }

    /// A point lookup: `GET /views/{name}/{key}`.
    pub fn point(key: String) -> Self {
        ViewQuery {
            key: Some(key),
            ..Self::base()
        }
    }

    /// A search: `/views/{name}/search`, by text (`q`) or raw vector.
    pub fn search(q: Option<String>, vector: Option<Value>, k: usize) -> Self {
        ViewQuery {
            searching: true,
            q,
            vector,
            k,
            ..Self::base()
        }
    }
}

/// Outcome of asking one pipeline branch to serve a read.
pub enum ViewRead {
    /// No sink in this branch has that name (or the key holds nothing).
    NotFound,
    /// The view exists but the request doesn't fit it.
    BadRequest(String),
    Data(Value),
}

/// Read dispatch and self-description for a pipeline's readers.
pub trait Views {
    /// Append a spec for every sink in this branch, in pipeline order.
    fn specs(&self, out: &mut Vec<ViewSpec>);

    /// Serve `q` if a sink in this branch is named `view`.
    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead;
}

/// JSON schema for `T`. The generator inlines subschemas under `$defs`, so
/// the result is self-contained and can be embedded anywhere in a document.
pub(crate) fn schema_of<T: JsonSchema>() -> Value {
    let schema = schemars::SchemaGenerator::default().into_root_schema_for::<T>();
    serde_json::to_value(schema).unwrap()
}

/// Parse a raw URL path segment as a view's key type: first as JSON
/// (`7`, `"quoted"`, `[1,2]`), then as a bare string — so `/views/t/7`
/// and `/views/t/alice` both do what they look like.
pub(crate) fn parse_key<K: DeserializeOwned>(raw: &str) -> Option<K> {
    serde_json::from_str(raw)
        .ok()
        .or_else(|| serde_json::from_value(Value::String(raw.to_string())).ok())
}

impl<R: Readable> Views for CountReader<'_, R> {
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "count",
            keyed: false,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": { "value": { "type": "integer" } },
                "required": ["value"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        ViewRead::Data(json!({ "value": self.get() }))
    }
}

impl<R: Readable, D> Views for BagReader<'_, R, D>
where
    D: Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "bag",
            keyed: false,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": {
                    "value": schema_of::<D>(),
                    "count": { "type": "integer" },
                },
                "required": ["value", "count"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        if q.key.is_some() {
            return ViewRead::BadRequest("bag views have no key lookup".into());
        }
        let items: Vec<Value> = self
            .iter()
            .skip(q.offset)
            .take(q.limit)
            .map(|(value, count)| json!({ "value": value, "count": count }))
            .collect();
        ViewRead::Data(Value::Array(items))
    }
}

impl<R: Readable, K, V> Views for TableReader<'_, R, K, V>
where
    K: Serialize + DeserializeOwned + JsonSchema,
    V: Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "table",
            keyed: true,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": {
                    "key": schema_of::<K>(),
                    "value": schema_of::<V>(),
                },
                "required": ["key", "value"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        match &q.key {
            Some(raw) => match parse_key::<K>(raw) {
                None => {
                    ViewRead::BadRequest(format!("cannot parse {raw:?} as this table's key type"))
                }
                Some(key) => match self.get(&key) {
                    Some(value) => ViewRead::Data(json!({ "key": key, "value": value })),
                    None => ViewRead::NotFound,
                },
            },
            None => {
                let items: Vec<Value> = self
                    .iter()
                    .skip(q.offset)
                    .take(q.limit)
                    .map(|(key, value)| json!({ "key": key, "value": value }))
                    .collect();
                ViewRead::Data(Value::Array(items))
            }
        }
    }
}

impl<R: Readable> Views for StatsReader<'_, R> {
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "stats",
            keyed: false,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": {
                    "count": { "type": "integer" },
                    "sum": { "type": "number" },
                    "mean": { "type": ["number", "null"] },
                    "variance": { "type": ["number", "null"] },
                    "stddev": { "type": ["number", "null"] },
                },
                "required": ["count", "sum", "mean", "variance", "stddev"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        ViewRead::Data(json!({
            "count": self.count(),
            "sum": self.sum(),
            "mean": self.mean(),
            "variance": self.variance(),
            "stddev": self.stddev(),
        }))
    }
}

impl<R, T> Views for HistogramReader<'_, R, T>
where
    R: Readable,
    T: Score + Serialize + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "histogram",
            keyed: false,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": {
                    "total": { "type": "integer" },
                    "buckets": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "bucket": schema_of::<T>(),
                                "count": { "type": "integer" },
                            },
                            "required": ["bucket", "count"],
                        },
                    },
                },
                "required": ["total", "buckets"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        if q.key.is_some() {
            return ViewRead::BadRequest("histogram views have no key lookup".into());
        }
        let row = |(bucket, count): (T, i64)| json!({ "bucket": bucket, "count": count });
        // iter() is ordered by bucket; ?desc=true walks it from the top
        let buckets: Vec<Value> = if q.desc {
            self.iter().rev().skip(q.offset).take(q.limit).map(row).collect()
        } else {
            self.iter().skip(q.offset).take(q.limit).map(row).collect()
        };
        ViewRead::Data(json!({ "total": self.total(), "buckets": buckets }))
    }
}

impl<R, S, V> Views for RankedReader<'_, R, S, V>
where
    R: Readable,
    S: Score + Serialize + JsonSchema,
    V: Clone + Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "ranked",
            keyed: false,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": {
                    "score": schema_of::<S>(),
                    "value": schema_of::<V>(),
                    "count": { "type": "integer" },
                },
                "required": ["score", "value", "count"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        if q.key.is_some() {
            return ViewRead::BadRequest("ranked views have no key lookup".into());
        }
        let row = |(scored, count): (fold::pipeline::Scored<S, V>, i64)| {
            json!({ "score": scored.score, "value": scored.val, "count": count })
        };
        // score-ordered; ?desc=true lists best-first (the top-n read)
        let items: Vec<Value> = if q.desc {
            self.iter().rev().skip(q.offset).take(q.limit).map(row).collect()
        } else {
            self.iter().skip(q.offset).take(q.limit).map(row).collect()
        };
        ViewRead::Data(Value::Array(items))
    }
}

impl<R, K, V> Views for MultimapReader<'_, R, K, V>
where
    R: Readable,
    K: Serialize + DeserializeOwned + JsonSchema,
    V: Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "multimap",
            keyed: true,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": {
                    "key": schema_of::<K>(),
                    "values": { "type": "array", "items": schema_of::<V>() },
                },
                "required": ["key", "values"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        match &q.key {
            None => ViewRead::BadRequest(format!(
                "multimap views are point lookups: GET /views/{view}/{{key}}"
            )),
            Some(raw) => match parse_key::<K>(raw) {
                None => ViewRead::BadRequest(format!("cannot parse {raw:?} as this view's key type")),
                // set semantics: an absent key and an empty posting list are
                // the same thing, so this is a 200 with [], not a 404
                Some(key) => ViewRead::Data(json!({ "key": key, "values": self.get(&key) })),
            },
        }
    }
}

impl<R, K, V> Views for InvertedIndexReader<'_, R, K, V>
where
    R: Readable,
    K: Serialize + DeserializeOwned + JsonSchema,
    V: Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(ViewSpec {
            name: self.name().to_string(),
            kind: "inverted_index",
            keyed: true,
            search: None,
            item_schema: json!({
                "type": "object",
                "properties": {
                    "value": schema_of::<V>(),
                    "keys": { "type": "array", "items": schema_of::<K>() },
                },
                "required": ["value", "keys"],
            }),
        });
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        if q.searching {
            return ViewRead::BadRequest("this view is not searchable".into());
        }
        match &q.key {
            None => ViewRead::BadRequest(format!(
                "inverted index views are point lookups: GET /views/{view}/{{value}}"
            )),
            // the path segment here is a posted *value*, not a key
            Some(raw) => match parse_key::<V>(raw) {
                None => {
                    ViewRead::BadRequest(format!("cannot parse {raw:?} as this view's value type"))
                }
                Some(value) => {
                    let keys: Vec<K> = self.search(&value);
                    ViewRead::Data(json!({ "value": value, "keys": keys }))
                }
            },
        }
    }
}

// Fan-out branches: readers of a tuple pipeline are a tuple of readers.
// Try each element in order; the first non-NotFound answer wins (sink
// names are unique pipeline-wide, so at most one element ever answers).
// Arities match fold's tuple Push impls (fold/src/pipeline/tuple.rs).
macro_rules! impl_views_tuple {
    ($($name:ident $idx:tt),+) => {
        impl<$($name: Views),+> Views for ($($name,)+) {
            fn specs(&self, out: &mut Vec<ViewSpec>) {
                $(self.$idx.specs(out);)+
            }
            fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
                $(
                    match self.$idx.read(view, q) {
                        ViewRead::NotFound => {}
                        hit => return hit,
                    }
                )+
                ViewRead::NotFound
            }
        }
    };
}

impl_views_tuple!(A 0);
impl_views_tuple!(A 0, B 1);
impl_views_tuple!(A 0, B 1, C 2);
impl_views_tuple!(A 0, B 1, C 2, D 3);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12, N 13);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12, N 13, O 14);
impl_views_tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11, M 12, N 13, O 14, P 15);

#[cfg(test)]
mod tests {
    use super::parse_key;

    #[test]
    fn keys_parse_as_json_then_bare_string() {
        assert_eq!(parse_key::<u64>("7"), Some(7));
        assert_eq!(parse_key::<String>("alice"), Some("alice".to_string()));
        // JSON wins when it parses: a quoted segment is the string inside
        assert_eq!(parse_key::<String>("\"alice\""), Some("alice".to_string()));
        assert_eq!(parse_key::<u64>("alice"), None);
    }
}
