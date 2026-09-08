//! The bog contender: one `KeyedStream` fanning out to four materialized
//! views — BM25 postings, an HNSW graph over ese embeddings, an id→text
//! table, and running stats. Everything a query reads was computed at
//! write time.
//!
//! The pipeline's `Map` and `Stats` stages take fn pointers rather than
//! closures so the pipeline type is nameable and the driver can live in a
//! struct.

use std::path::Path;

use anny::metric::Cosine;
use fold::pipeline::{Keyed, Map, terminal};
use fold::stream::KeyedStream;

use crate::rrf;

pub const DIM: usize = ese::DIMENSIONS;

type EmbedFn = fn(&Keyed<u64, String>) -> Keyed<u64, [f32; DIM]>;
type LenFn = fn(&Keyed<u64, String>) -> f64;
type Vecs = terminal::search::Hnsw<u64, f32, Cosine, DIM>;
type Pipeline = (
    terminal::search::Bm25<u64, String>,
    Map<EmbedFn, Vecs, Keyed<u64, String>, Keyed<u64, [f32; DIM]>>,
    terminal::Table<u64, String>,
    terminal::Stats<Keyed<u64, String>, LenFn>,
);

fn embed(d: &Keyed<u64, String>) -> Keyed<u64, [f32; DIM]> {
    Keyed::new(d.key, ese::encode_single(&d.val))
}

fn nchars(d: &Keyed<u64, String>) -> f64 {
    d.val.len() as f64
}

/// Maintenance cadence: major-compact the BM25 postings keyspace after
/// this many applied ops. Without it, LSM read amplification on the
/// postings degrades keyword scans by orders of magnitude at scale (the
/// séance benchmark's finding #1). The cost is charged into apply time —
/// FTS5 pays its own merge policy inside its applies too.
const COMPACT_EVERY: usize = 2_500;

pub struct Bog {
    st: KeyedStream<u64, String, Pipeline>,
    dir: std::path::PathBuf,
    ops_since_compact: usize,
}

impl Bog {
    pub fn open(dir: &Path) -> Self {
        let _ = std::fs::remove_dir_all(dir);
        let st = KeyedStream::with_cache(
            dir,
            (
                terminal::search::Bm25::new("bm25"),
                Map::new(embed as EmbedFn, Vecs::new("vecs", Cosine, 42)),
                terminal::Table::new("docs"),
                terminal::Stats::new("stats", nchars as LenFn),
            ),
            crate::CACHE_BYTES,
        );
        Bog {
            st,
            dir: dir.to_path_buf(),
            ops_since_compact: 0,
        }
    }

    /// One transaction: upserts and removes, all views maintained
    /// atomically. Periodic postings compaction happens in here so the
    /// harness charges it to write time, where it belongs.
    pub fn apply(&mut self, upserts: &[(u64, String)], removes: &[u64]) {
        self.st.wtx(|tx| {
            for (id, text) in upserts {
                tx.upsert(id, text);
            }
            for id in removes {
                tx.remove(id);
            }
        });
        self.ops_since_compact += upserts.len() + removes.len();
        if self.ops_since_compact >= COMPACT_EVERY {
            self.ops_since_compact = 0;
            let t = std::time::Instant::now();
            if let Ok(ks) = self
                .st
                .db()
                .keyspace("sink_bm25", fjall::KeyspaceCreateOptions::default)
            {
                let _ = ks.inner().major_compact();
            }
            eprintln!(
                "bog: compacted bm25 postings in {} ms",
                t.elapsed().as_millis()
            );
        }
    }

    pub fn keyword(&self, query: &str, k: usize) -> Vec<u64> {
        self.st
            .rtx(|(bm25, _, _, _)| bm25.search(query, k).into_iter().map(|h| h.val).collect())
    }

    /// Timed end to end by the harness: includes embedding the query.
    pub fn semantic(&self, query: &str, k: usize) -> Vec<u64> {
        let q = ese::encode_single(query);
        self.st.rtx(|(_, vecs, _, _)| {
            vecs.search(&q).into_iter().take(k).map(|h| h.val).collect()
        })
    }

    /// The whole product operation: embed, BM25 top-k, HNSW top-k, fuse.
    pub fn hybrid(&self, query: &str, k: usize) -> Vec<u64> {
        let q = ese::encode_single(query);
        self.st.rtx(|(bm25, vecs, _, _)| {
            let kw: Vec<u64> = bm25.search(query, k).into_iter().map(|h| h.val).collect();
            let sem: Vec<u64> = vecs.search(&q).into_iter().map(|h| h.val).collect();
            rrf(&kw, &sem, k)
        })
    }

    /// (live docs, mean length) from the materialized counters.
    pub fn stats(&self) -> (i64, f64) {
        self.st
            .rtx(|(_, _, _, stats)| (stats.count(), stats.mean().unwrap_or(0.0)))
    }

    pub fn disk_bytes(&self) -> u64 {
        dir_bytes(&self.dir)
    }
}

fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_bytes(&path);
            } else if let Ok(md) = entry.metadata() {
                total += md.len();
            }
        }
    }
    total
}
