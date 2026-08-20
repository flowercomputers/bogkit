//! The `search` example, served: text search three ways over one keyed
//! stream of documents, generated straight from the pipeline by bog-serve.
//! A good base for a shared agent memory reachable over HTTP.
//!
//! Run it (`cargo run -p search-server`, or `bogkit dev -p search-server`),
//! then:
//!
//!   # remember, correct, forget — every index updates atomically
//!   curl -X PUT localhost:7877/docs/1 -H 'content-type: application/json' \
//!        -d '"the postgres database was slow because of a missing index"'
//!   curl -X DELETE localhost:7877/docs/1
//!
//!   # search: keyword, semantic, hybrid
//!   curl 'localhost:7877/views/bm25/search?q=postgres+slow'
//!   curl 'localhost:7877/views/vecs/search?q=database+performance'
//!   curl 'localhost:7877/search/hybrid?q=database+performance'
//!
//!   # write-if-absent, atomically (409 if the id is taken)
//!   curl -X POST localhost:7877/remember_once -H 'content-type: application/json' \
//!        -d '{"id": 7, "text": "the standup moved to 9:30"}'
//!
//!   # everything else
//!   curl localhost:7877/openapi.json
//!   curl -N localhost:7877/watch
//!
//! The pipeline is the search example's, unchanged: BM25 over the text, an
//! HNSW graph over ese embeddings (computed inside the pipeline by a Map,
//! so retraction cancels cleanly), and an id -> text table. `TextQuery`
//! hands the server the same encoder for query time, which is what turns
//! `?q=` on into the vector view.

use anny::metric::Cosine;
use bog_serve::{KeyedApp, TextQuery};
use fold::pipeline::{Keyed, Map, terminal};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

const DIM: usize = ese::DIMENSIONS;

/// Reciprocal-rank-fusion constant: dampens the head so one list can't
/// dominate. 60 is the value from the original RRF paper.
const RRF_K: f64 = 60.0;

fn main() {
    KeyedApp::stream(
        bog_serve::data_dir(),
        (
            // keyword: tokenized text, ranked by BM25 relevance
            terminal::search::Bm25::new("bm25"),
            // semantic: ese embeds the text right here in the pipeline;
            // TextQuery registers the same encoding for query time
            Map::new(
                |d: &Keyed<u64, String>| Keyed::new(d.key, ese::encode_single(&d.val)),
                TextQuery::new(
                    terminal::search::Hnsw::<u64, f32, Cosine, DIM>::new("vecs", Cosine, 42),
                    |q| ese::encode_single(q),
                ),
            ),
            // id -> text, for showing hits
            terminal::Table::new("docs"),
        ),
    )
    // reciprocal rank fusion of both indexes — logic that lives outside any
    // sink, which is exactly what custom routes are for. The readers are
    // the same tuple an rtx closure gets, on one consistent snapshot; the
    // params and response types put real schemas in /openapi.json.
    .get("/search/hybrid", |(bm25, vecs, docs), p: HybridParams| {
        // rank-based fusion sidesteps the incomparable score scales
        // (BM25 relevance vs cosine distance)
        let mut fused: HashMap<u64, f64> = HashMap::new();
        for (rank, hit) in bm25.search(&p.q, 10).iter().enumerate() {
            *fused.entry(hit.val).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
        for (rank, hit) in vecs.inner().search(&vecs.encode(&p.q)).iter().enumerate() {
            *fused.entry(hit.val).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
        }
        let mut fused: Vec<(u64, f64)> = fused.into_iter().collect();
        fused.sort_by(|a, b| b.1.total_cmp(&a.1));
        fused.truncate(p.k);

        Ok(fused
            .into_iter()
            .map(|(id, score)| HybridHit {
                id,
                score,
                text: docs.get(&id),
            })
            .collect::<Vec<_>>())
    })
    // a custom write: remember a fact only if its id is free. Err rolls the
    // whole transaction back, so racing agents can't clobber each other —
    // atomic check-and-set over HTTP.
    .post("/remember_once", |tx, m: Memory| {
        if tx.contains(&m.id) {
            return Err((409, format!("memory {} already exists", m.id)));
        }
        tx.upsert(&m.id, &m.text);
        Ok(json!({ "remembered": m.id }))
    })
    .run()
}

/// Query for `/search/hybrid`. Field types become the documented (and
/// enforced) query parameters: `?q=...&k=5`.
#[derive(Deserialize, JsonSchema)]
struct HybridParams {
    q: String,
    #[serde(default = "default_k")]
    k: usize,
}

fn default_k() -> usize {
    3
}

/// One fused hit; the schema of `/search/hybrid`'s response items.
#[derive(Serialize, JsonSchema)]
struct HybridHit {
    id: u64,
    score: f64,
    text: Option<String>,
}

/// Body for `POST /remember_once`.
#[derive(Deserialize, JsonSchema)]
struct Memory {
    id: u64,
    text: String,
}
