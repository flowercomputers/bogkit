use crate::{JsonDocument, Query, Result, evaluate, extracted_text, invalid};
use bog_definition::{Action, Definition, Resource, Terminal};
use fold::{
    pipeline::{
        Keyed, Push, Scored,
        terminal::{self, search},
    },
    stream::{PipelineInitCtx, Readable, WriteTx},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
type Doc = Keyed<String, JsonDocument>;
type VectorIndex = search::Hnsw<String, f32, anny::metric::Cosine, 512, 32, 50, 100>;
type VectorReader<'tx, R> =
    search::HnswReader<'tx, R, String, f32, anny::metric::Cosine, 512, 32, 50, 100, 80, 16>;
pub(crate) enum Sink {
    Table(terminal::Table<String, JsonDocument>),
    Count(terminal::Count),
    Stats(terminal::Stats<f64, fn(&f64) -> f64>),
    Ranked(terminal::Ranked<f64, String>),
    Bm25(search::Bm25<String, String>),
    Semantic(VectorIndex),
}
type TextReader<'tx, R> = search::Bm25Reader<'tx, R, String, fn(&str, &mut Vec<u8>)>;
pub(crate) enum Reader<'tx, R: Readable> {
    Table(terminal::TableReader<'tx, R, String, JsonDocument>),
    Count(terminal::CountReader<'tx, R>),
    Stats(terminal::StatsReader<'tx, R>),
    Ranked(terminal::RankedReader<'tx, R, f64, String>, bool),
    Bm25(TextReader<'tx, R>),
    Semantic(VectorReader<'tx, R>),
}
struct Branch {
    resource: Resource,
    sink: Sink,
}
pub(crate) struct Pipeline {
    source: terminal::Table<String, JsonDocument>,
    branches: BTreeMap<String, Branch>,
}
pub(crate) struct Readers<'tx, R: Readable> {
    pub source: terminal::TableReader<'tx, R, String, JsonDocument>,
    pub resources: BTreeMap<String, Reader<'tx, R>>,
}

impl Pipeline {
    pub fn new(definition: &Definition) -> Self {
        let branches = definition
            .resources
            .iter()
            .map(|(name, r)| {
                let name_sink = format!("resource_{name}");
                let sink = match &r.terminal {
                    Terminal::Table => Sink::Table(terminal::Table::new(name_sink)),
                    Terminal::Count => Sink::Count(terminal::Count::new(name_sink)),
                    Terminal::Stats { .. } => Sink::Stats(terminal::Stats::new(name_sink, |n| *n)),
                    Terminal::Ranked { .. } => Sink::Ranked(terminal::Ranked::new(name_sink)),
                    Terminal::Bm25 { .. } => {
                        Sink::Bm25(search::Bm25::with_tokenizer(name_sink, search::tokenize))
                    }
                    Terminal::Semantic { .. } => {
                        Sink::Semantic(VectorIndex::new(name_sink, anny::metric::Cosine, 42))
                    }
                };
                (
                    name.clone(),
                    Branch {
                        resource: r.clone(),
                        sink,
                    },
                )
            })
            .collect();
        Self {
            source: terminal::Table::new("runtime_source"),
            branches,
        }
    }
}
impl Push<Doc> for Pipeline {
    type Reader<'tx, R: Readable + 'tx> = Readers<'tx, R>;
    fn init(&mut self, ctx: &mut PipelineInitCtx<'_>) {
        self.source.init(ctx);
        for branch in self.branches.values_mut() {
            match &mut branch.sink {
                Sink::Table(s) => s.init(ctx),
                Sink::Count(s) => <terminal::Count as Push<Doc>>::init(s, ctx),
                Sink::Stats(s) => s.init(ctx),
                Sink::Ranked(s) => s.init(ctx),
                Sink::Bm25(s) => s.init(ctx),
                Sink::Semantic(s) => s.init(ctx),
            }
        }
    }
    fn push(&mut self, tx: &mut WriteTx<'_>, data: &Doc, delta: isize) {
        self.source.push(tx, data, delta);
        for branch in self.branches.values_mut() {
            let Some(value) = evaluate(&branch.resource, data.val.as_value())
                .expect("validated before transaction")
            else {
                continue;
            };
            match (&mut branch.sink, &branch.resource.terminal) {
                (Sink::Table(s), _) => s.push(
                    tx,
                    &Keyed::new(
                        data.key.clone(),
                        JsonDocument::try_from_value(value).expect("projection remains object"),
                    ),
                    delta,
                ),
                (Sink::Count(s), _) => s.push(tx, data, delta),
                (Sink::Stats(s), Terminal::Stats { field }) => {
                    s.push(tx, &value.pointer(field).unwrap().as_f64().unwrap(), delta)
                }
                (Sink::Ranked(s), Terminal::Ranked { field, .. }) => s.push(
                    tx,
                    &Scored::new(
                        value.pointer(field).unwrap().as_f64().unwrap(),
                        data.key.clone(),
                    ),
                    delta,
                ),
                (Sink::Bm25(s), Terminal::Bm25 { fields, .. }) => s.push(
                    tx,
                    &Keyed::new(
                        data.key.clone(),
                        extracted_text(&value, fields).unwrap().unwrap(),
                    ),
                    delta,
                ),
                (Sink::Semantic(s), Terminal::Semantic { fields, .. }) => {
                    let text = extracted_text(&value, fields).unwrap().unwrap();
                    let embedding = ese::encode_single(&text);
                    s.push(tx, &Keyed::new(data.key.clone(), embedding), delta)
                }
                _ => unreachable!(),
            }
        }
    }
    fn commit(&mut self, tx: &mut WriteTx<'_>) {
        self.source.commit(tx);
        for branch in self.branches.values_mut() {
            match &mut branch.sink {
                Sink::Table(s) => s.commit(tx),
                Sink::Count(s) => <terminal::Count as Push<Doc>>::commit(s, tx),
                Sink::Stats(s) => s.commit(tx),
                Sink::Ranked(s) => s.commit(tx),
                Sink::Bm25(s) => s.commit(tx),
                Sink::Semantic(s) => s.commit(tx),
            }
        }
    }
    fn abort(&mut self) {
        self.source.abort();
        for branch in self.branches.values_mut() {
            match &mut branch.sink {
                Sink::Table(s) => s.abort(),
                Sink::Count(s) => <terminal::Count as Push<Doc>>::abort(s),
                Sink::Stats(s) => s.abort(),
                Sink::Ranked(s) => s.abort(),
                Sink::Bm25(s) => s.abort(),
                Sink::Semantic(s) => s.abort(),
            }
        }
    }
    fn reader<'tx, R: Readable>(&self, tx: &'tx R) -> Self::Reader<'tx, R> {
        Readers {
            source: self.source.reader(tx),
            resources: self
                .branches
                .iter()
                .map(|(name, b)| {
                    let reader = match &b.sink {
                        Sink::Table(s) => Reader::Table(s.reader(tx)),
                        Sink::Count(s) => {
                            Reader::Count(<terminal::Count as Push<Doc>>::reader(s, tx))
                        }
                        Sink::Stats(s) => Reader::Stats(s.reader(tx)),
                        Sink::Ranked(s) => Reader::Ranked(
                            s.reader(tx),
                            matches!(
                                b.resource.terminal,
                                Terminal::Ranked {
                                    ascending: true,
                                    ..
                                }
                            ),
                        ),
                        Sink::Bm25(s) => Reader::Bm25(s.reader(tx)),
                        Sink::Semantic(s) => Reader::Semantic(s.reader(tx)),
                    };
                    (name.clone(), reader)
                })
                .collect(),
        }
    }
}
impl<R: Readable> Reader<'_, R> {
    pub fn validate_state(&self) -> Result<()> {
        if let Self::Stats(s) = self
            && (!s.sum().is_finite()
                || s.mean().is_some_and(|x| !x.is_finite())
                || s.variance().is_some_and(|x| !x.is_finite()))
        {
            return Err(invalid("numeric aggregate exceeds finite range"));
        }
        Ok(())
    }
    pub fn vector_count(&self) -> usize {
        match self {
            Self::Semantic(s) => s.len(),
            _ => 0,
        }
    }
    pub fn query(&self, action: Action, q: &Query) -> Result<Value> {
        let limit = q
            .limit
            .unwrap_or(if action == Action::Search { 10 } else { 100 });
        let offset = q.offset.unwrap_or(0);
        match (self, action) {
            (Self::Table(s), Action::Get) => Ok(s
                .get(q.key.as_ref().ok_or_else(|| invalid("key required"))?)
                .map(|v| v.as_value().clone())
                .unwrap_or(Value::Null)),
            (Self::Table(s), Action::List) => {
                let mut rows = Vec::new();
                let mut bytes = 0;
                let ordered: std::collections::BTreeSet<_> = s.iter().map(|(key, _)| key).collect();
                for k in ordered
                    .into_iter()
                    .filter(|k| {
                        q.after.as_ref().is_none_or(|a| k > a)
                            && q.before.as_ref().is_none_or(|b| k < b)
                    })
                    .skip(offset)
                    .take(limit)
                {
                    let row = json!({"key":k,"value":s.get(&k)});
                    bytes += row.to_string().len();
                    if bytes > 4 * 1024 * 1024 - 4096 {
                        return Err(invalid("response exceeds 4 MiB; reduce page size"));
                    }
                    rows.push(row)
                }
                Ok(Value::Array(rows))
            }
            (Self::Count(s), Action::Read) => Ok(json!(s.get())),
            (Self::Stats(s), Action::Read) => Ok(
                json!({"count":s.count(),"sum":s.sum(),"mean":s.mean(),"variance":s.variance(),"stddev":s.stddev()}),
            ),
            (Self::Ranked(s, ascending), Action::Top) => {
                let rows = if *ascending {
                    s.bottom(limit + offset)
                } else {
                    s.top(limit + offset)
                };
                Ok(Value::Array(
                    rows.into_iter()
                        .skip(offset)
                        .map(|s| json!({"key":s.val,"score":s.score}))
                        .collect(),
                ))
            }
            (Self::Bm25(s), Action::Search) => {
                if q.vector.is_some() {
                    return Err(invalid("BM25 does not accept a vector"));
                }
                let text = q.query.as_ref().ok_or_else(|| invalid("query required"))?;
                Ok(Value::Array(
                    s.search(text, limit + offset)
                        .into_iter()
                        .skip(offset)
                        .map(|s| json!({"key":s.val,"score":s.score}))
                        .collect(),
                ))
            }
            (Self::Semantic(s), Action::Search) => {
                let vector = match (&q.query, &q.vector) {
                    (Some(text), None) => ese::encode_single(text),
                    (None, Some(v)) => v
                        .clone()
                        .try_into()
                        .map_err(|_| invalid("vector must have 512 dimensions"))?,
                    _ => return Err(invalid("provide exactly one query or vector")),
                };
                validate_vector(&vector)?;
                Ok(Value::Array(
                    s.search(&vector)
                        .into_iter()
                        .filter(|hit| q.max_distance.is_none_or(|max| f64::from(hit.score) <= max))
                        .skip(offset)
                        .take(limit)
                        .map(|s| json!({"key":s.val,"distance":s.score,"score":1.0 - f64::from(s.score)}))
                        .collect(),
                ))
            }
            _ => Err(invalid("operation is incompatible with resource")),
        }
    }
}
pub(crate) fn validate_vector(v: &[f32]) -> Result<()> {
    if v.len() != 512 || v.iter().any(|x| !x.is_finite()) || !v.iter().any(|x| *x != 0.0) {
        Err(invalid(
            "vector must contain 512 finite values with nonzero norm",
        ))
    } else {
        let norm: f32 = v.iter().map(|x| *x * *x).sum();
        if !norm.is_finite() || norm <= 0.0 {
            return Err(invalid("vector norm exceeds f32 range"));
        }
        Ok(())
    }
}
