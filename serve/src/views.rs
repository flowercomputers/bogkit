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
    BagReader, CountReader, HistogramReader, InvertedIndexReader, KeyedRankedReader,
    MultimapReader, RankedReader, StatsReader, TableReader,
};
use fold::stream::Readable;
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// What one sink contributes to the OpenAPI doc and `/schema` fingerprint.
pub struct ViewSpec {
    pub name: String,
    pub kind: ViewKind,
    /// Whether the view supports point lookup at `/views/{name}/{key}`.
    pub keyed: bool,
    pub search: Option<SearchMode>,
    /// JSON schema of one item as this view returns it.
    pub item_schema: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Count,
    Bag,
    Table,
    Stats,
    Histogram,
    Ranked,
    KeyedRanked,
    Multimap,
    InvertedIndex,
    Bm25,
    Hnsw,
}

impl ViewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Bag => "bag",
            Self::Table => "table",
            Self::Stats => "stats",
            Self::Histogram => "histogram",
            Self::Ranked => "ranked",
            Self::KeyedRanked => "keyed_ranked",
            Self::Multimap => "multimap",
            Self::InvertedIndex => "inverted_index",
            Self::Bm25 => "bm25",
            Self::Hnsw => "hnsw",
        }
    }

    pub(crate) fn listing(self) -> Listing {
        match self {
            Self::Count | Self::Stats => Listing::Scalar,
            Self::Histogram => Listing::Object,
            Self::Bag | Self::Table | Self::Ranked => Listing::Page,
            Self::Multimap | Self::InvertedIndex | Self::KeyedRanked => Listing::PointOnly,
            Self::Bm25 | Self::Hnsw => Listing::SearchOnly,
        }
    }
}

/// How `GET /views/{name}` is documented and served.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Listing {
    /// One object, no pagination (count, stats).
    Scalar,
    /// One object whose innards paginate (histogram buckets).
    Object,
    /// A paginated array.
    Page,
    /// No listing GET; point lookup only.
    PointOnly,
    /// No listing GET; search only.
    SearchOnly,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Text,
    Vector,
    TextAndVector,
}

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Vector => "vector",
            Self::TextAndVector => "text+vector",
        }
    }

    pub(crate) fn has_text(self) -> bool {
        matches!(self, Self::Text | Self::TextAndVector)
    }

    pub(crate) fn has_vector(self) -> bool {
        matches!(self, Self::Vector | Self::TextAndVector)
    }
}

/// A read request already routed to `/views/{name}[/{key}|/search]`.
pub enum ViewQuery {
    List {
        limit: usize,
        offset: usize,
        desc: bool,
    },
    Point {
        key: String,
        limit: usize,
        offset: usize,
        desc: bool,
    },
    Search {
        q: Option<String>,
        vector: Option<Value>,
        k: usize,
    },
}

impl ViewQuery {
    pub fn list(limit: usize, offset: usize, desc: bool) -> Self {
        Self::List {
            limit,
            offset,
            desc,
        }
    }

    pub fn point(key: String) -> Self {
        Self::Point {
            key,
            limit: 100,
            offset: 0,
            desc: false,
        }
    }

    pub fn point_page(key: String, limit: usize, offset: usize, desc: bool) -> Self {
        Self::Point {
            key,
            limit,
            offset,
            desc,
        }
    }

    pub fn search(q: Option<String>, vector: Option<Value>, k: usize) -> Self {
        Self::Search { q, vector, k }
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

pub(crate) fn spec(
    name: impl Into<String>,
    kind: ViewKind,
    keyed: bool,
    search: Option<SearchMode>,
    item_schema: Value,
) -> ViewSpec {
    ViewSpec {
        name: name.into(),
        kind,
        keyed,
        search,
        item_schema,
    }
}

/// JSON schema for `T`, with all subschemas inlined.
///
/// Inlining matters: these schemas are embedded deep inside the OpenAPI
/// document, where a schemars-default `{"$ref": "#/$defs/..."}` would
/// point at the *document* root and dangle. Recursive types cannot be
/// inlined — API DTOs shouldn't be recursive.
pub(crate) fn schema_of<T: JsonSchema>() -> Value {
    let mut settings = schemars::generate::SchemaSettings::default();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
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

fn not_searchable() -> ViewRead {
    ViewRead::BadRequest("this view is not searchable".into())
}

fn page<I, T>(
    iter: I,
    offset: usize,
    limit: usize,
    desc: bool,
    row: impl Fn(T) -> Value,
) -> Vec<Value>
where
    I: DoubleEndedIterator<Item = T>,
{
    if desc {
        iter.rev().skip(offset).take(limit).map(row).collect()
    } else {
        iter.skip(offset).take(limit).map(row).collect()
    }
}

impl<R: Readable> Views for CountReader<'_, R> {
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Count,
            false,
            None,
            json!({
                "type": "object",
                "properties": { "value": { "type": "integer" } },
                "required": ["value"],
            }),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::Point { .. } => {
                ViewRead::BadRequest("count views have no key lookup".into())
            }
            ViewQuery::List { .. } => ViewRead::Data(json!({ "value": self.get() })),
        }
    }
}

impl<R: Readable, D> Views for BagReader<'_, R, D>
where
    D: Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Bag,
            false,
            None,
            json!({
                "type": "object",
                "properties": {
                    "value": schema_of::<D>(),
                    "count": { "type": "integer" },
                },
                "required": ["value", "count"],
            }),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::Point { .. } => ViewRead::BadRequest("bag views have no key lookup".into()),
            ViewQuery::List { limit, offset, .. } => {
                let items: Vec<Value> = self
                    .iter()
                    .skip(*offset)
                    .take(*limit)
                    .map(|(value, count)| json!({ "value": value, "count": count }))
                    .collect();
                ViewRead::Data(Value::Array(items))
            }
        }
    }
}

impl<R: Readable, K, V> Views for TableReader<'_, R, K, V>
where
    K: Serialize + DeserializeOwned + JsonSchema,
    V: Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Table,
            true,
            None,
            json!({
                "type": "object",
                "properties": {
                    "key": schema_of::<K>(),
                    "value": schema_of::<V>(),
                },
                "required": ["key", "value"],
            }),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::Point { key: raw, .. } => match parse_key::<K>(raw) {
                None => {
                    ViewRead::BadRequest(format!("cannot parse {raw:?} as this table's key type"))
                }
                Some(key) => match self.get(&key) {
                    Some(value) => ViewRead::Data(json!({ "key": key, "value": value })),
                    None => ViewRead::NotFound,
                },
            },
            ViewQuery::List { limit, offset, .. } => {
                let items: Vec<Value> = self
                    .iter()
                    .skip(*offset)
                    .take(*limit)
                    .map(|(key, value)| json!({ "key": key, "value": value }))
                    .collect();
                ViewRead::Data(Value::Array(items))
            }
        }
    }
}

impl<R: Readable> Views for StatsReader<'_, R> {
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Stats,
            false,
            None,
            json!({
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
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::Point { .. } => {
                ViewRead::BadRequest("stats views have no key lookup".into())
            }
            ViewQuery::List { .. } => ViewRead::Data(json!({
                "count": self.count(),
                "sum": self.sum(),
                "mean": self.mean(),
                "variance": self.variance(),
                "stddev": self.stddev(),
            })),
        }
    }
}

impl<R, T> Views for HistogramReader<'_, R, T>
where
    R: Readable,
    T: Score + Serialize + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Histogram,
            false,
            None,
            json!({
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
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::Point { .. } => {
                ViewRead::BadRequest("histogram views have no key lookup".into())
            }
            ViewQuery::List {
                limit,
                offset,
                desc,
            } => {
                let row = |(bucket, count): (T, i64)| json!({ "bucket": bucket, "count": count });
                let buckets = page(self.iter(), *offset, *limit, *desc, row);
                ViewRead::Data(json!({ "total": self.total(), "buckets": buckets }))
            }
        }
    }
}

impl<R, S, V> Views for RankedReader<'_, R, S, V>
where
    R: Readable,
    S: Score + Serialize + JsonSchema,
    V: Clone + Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Ranked,
            false,
            None,
            json!({
                "type": "object",
                "properties": {
                    "score": schema_of::<S>(),
                    "value": schema_of::<V>(),
                    "count": { "type": "integer" },
                },
                "required": ["score", "value", "count"],
            }),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::Point { .. } => {
                ViewRead::BadRequest("ranked views have no key lookup".into())
            }
            ViewQuery::List {
                limit,
                offset,
                desc,
            } => {
                let row = |(scored, count): (fold::pipeline::Scored<S, V>, i64)| json!({ "score": scored.score, "value": scored.val, "count": count });
                ViewRead::Data(Value::Array(page(self.iter(), *offset, *limit, *desc, row)))
            }
        }
    }
}

impl<R, K, S, V> Views for KeyedRankedReader<'_, R, K, S, V>
where
    R: Readable,
    K: Serialize + DeserializeOwned + JsonSchema,
    S: Score + Serialize + JsonSchema,
    V: Clone + Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::KeyedRanked,
            true,
            None,
            json!({
                "type": "object",
                "properties": {
                    "score": schema_of::<S>(),
                    "value": schema_of::<V>(),
                    "count": { "type": "integer" },
                },
                "required": ["score", "value", "count"],
            }),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::List { .. } => ViewRead::BadRequest(format!(
                "keyed ranked views are point lookups: GET /views/{view}/{{key}}"
            )),
            ViewQuery::Point {
                key: raw,
                limit,
                offset,
                desc,
            } => match parse_key::<K>(raw) {
                None => {
                    ViewRead::BadRequest(format!("cannot parse {raw:?} as this view's key type"))
                }
                Some(key) => {
                    let row = |(scored, count): (fold::pipeline::Scored<S, V>, i64)| json!({ "score": scored.score, "value": scored.val, "count": count });
                    ViewRead::Data(Value::Array(page(
                        self.iter(&key),
                        *offset,
                        *limit,
                        *desc,
                        row,
                    )))
                }
            },
        }
    }
}

impl<R, K, V> Views for MultimapReader<'_, R, K, V>
where
    R: Readable,
    K: Serialize + DeserializeOwned + JsonSchema,
    V: Serialize + DeserializeOwned + JsonSchema,
{
    fn specs(&self, out: &mut Vec<ViewSpec>) {
        out.push(spec(
            self.name(),
            ViewKind::Multimap,
            true,
            None,
            json!({
                "type": "object",
                "properties": {
                    "key": schema_of::<K>(),
                    "values": { "type": "array", "items": schema_of::<V>() },
                },
                "required": ["key", "values"],
            }),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::List { .. } => ViewRead::BadRequest(format!(
                "multimap views are point lookups: GET /views/{view}/{{key}}"
            )),
            ViewQuery::Point { key: raw, .. } => match parse_key::<K>(raw) {
                None => {
                    ViewRead::BadRequest(format!("cannot parse {raw:?} as this view's key type"))
                }
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
        out.push(spec(
            self.name(),
            ViewKind::InvertedIndex,
            true,
            None,
            json!({
                "type": "object",
                "properties": {
                    "value": schema_of::<V>(),
                    "keys": { "type": "array", "items": schema_of::<K>() },
                },
                "required": ["value", "keys"],
            }),
        ));
    }

    fn read(&self, view: &str, q: &ViewQuery) -> ViewRead {
        if view != self.name() {
            return ViewRead::NotFound;
        }
        match q {
            ViewQuery::Search { .. } => not_searchable(),
            ViewQuery::List { .. } => ViewRead::BadRequest(format!(
                "inverted index views are point lookups: GET /views/{view}/{{value}}"
            )),
            ViewQuery::Point { key: raw, .. } => match parse_key::<V>(raw) {
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
