//! A direct, fair, reproducible benchmark: bog (fold + ese + anny) vs
//! SQLite (FTS5 + sqlite-vec), both driven natively from one Rust process
//! over the identical corpus, queries, and churn schedule.
//!
//! Design rules, so the numbers survive scrutiny:
//!
//!   - Both contenders are native. No interpreted baseline anywhere; the
//!     SQLite side gets FTS5, the sqlite-vec (vec0) extension with cosine
//!     distance, WAL, prepared statements, and batched transactions.
//!   - ese embeds documents at write time and queries at query time on
//!     BOTH sides — embedding cost is charged symmetrically.
//!   - Semantic recall@10 is measured against exact cosine ground truth,
//!     computed untimed by the harness. HNSW is approximate; a latency win
//!     without a recall number is not a result.
//!   - Everything flows from one seed: same corpus, same queries, same
//!     churn ops, both systems.
//!
//! Run: cargo run --release -p sqlite-bench [-- --scale 100000 --seed 42]
//! JSONL to sqlite-bench.jsonl, markdown summary to stdout, progress to
//! stderr.

mod bog;
mod corpus;
mod report;
mod sqlite;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use bog::{Bog, DIM};
use corpus::{Rng, gen_doc, gen_query};
use report::{IngestRow, QueryRow, Report, Series};
use sqlite::Sqlite;

/// Result size for every search operation, both systems.
const K: usize = 10;
/// Page/block cache budget, identical for both contenders. fjall defaults
/// to 32 MiB and SQLite to 2 MiB — both far too small for a ~100k-doc
/// store and neither what a careful operator would ship.
pub const CACHE_BYTES: u64 = 1024 * 1024 * 1024;
/// Iterations for the (very fast) stats read.
const STATS_REPS: usize = 200;
/// RRF constant from the original paper.
const RRF_K: f64 = 60.0;

/// Reciprocal-rank fusion of a keyword and a semantic hit list — the same
/// function fuses for both contenders.
pub fn rrf(keyword: &[u64], semantic: &[u64], k: usize) -> Vec<u64> {
    let mut fused: HashMap<u64, f64> = HashMap::new();
    for (rank, id) in keyword.iter().enumerate() {
        *fused.entry(*id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    for (rank, id) in semantic.iter().enumerate() {
        *fused.entry(*id).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    let mut fused: Vec<(u64, f64)> = fused.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    fused.truncate(k);
    fused.into_iter().map(|(id, _)| id).collect()
}

fn timed<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_nanos() as u64)
}

fn cosine_dist(a: &[f32; DIM], b: &[f32; DIM]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..DIM {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    1.0 - dot / (na.sqrt() * nb.sqrt()).max(f32::EPSILON)
}

/// Exact top-k by cosine over the live corpus: the recall ground truth.
fn exact_topk(live: &HashMap<u64, [f32; DIM]>, q: &[f32; DIM], k: usize) -> HashSet<u64> {
    let mut all: Vec<(f32, u64)> = live.iter().map(|(id, v)| (cosine_dist(q, v), *id)).collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    all.truncate(k);
    all.into_iter().map(|(_, id)| id).collect()
}

/// Time keyword/semantic/hybrid/stats for one contender over one query
/// set. Duck-typed macro: both drivers expose the same methods.
macro_rules! measure_system {
    ($report:expr, $phase:expr, $docs:expr, $name:literal, $drv:expr, $queries:expr, $gt:expr) => {{
        // warmup: first calls pay one-time costs (page cache, statement
        // caches) that steady-state numbers shouldn't include
        for q in $queries.iter().take(2) {
            $drv.keyword(q, K);
            $drv.semantic(q, K);
            $drv.hybrid(q, K);
        }

        let mut s = Series::new();
        for q in $queries.iter() {
            let (_, ns) = timed(|| $drv.keyword(q, K));
            s.push(ns);
        }
        $report.query(QueryRow {
            phase: $phase, docs: $docs, system: $name, op: "keyword",
            p50_us: s.pct_us(50.0), p99_us: s.pct_us(99.0), recall: None, n: $queries.len(),
        });

        let mut s = Series::new();
        let mut recall_sum = 0.0;
        for (q, gt) in $queries.iter().zip($gt.iter()) {
            let (hits, ns) = timed(|| $drv.semantic(q, K));
            s.push(ns);
            recall_sum += hits.iter().filter(|h| gt.contains(h)).count() as f64 / K as f64;
        }
        $report.query(QueryRow {
            phase: $phase, docs: $docs, system: $name, op: "semantic",
            p50_us: s.pct_us(50.0), p99_us: s.pct_us(99.0),
            recall: Some(recall_sum / $queries.len() as f64), n: $queries.len(),
        });

        let mut s = Series::new();
        for q in $queries.iter() {
            let (_, ns) = timed(|| $drv.hybrid(q, K));
            s.push(ns);
        }
        $report.query(QueryRow {
            phase: $phase, docs: $docs, system: $name, op: "hybrid",
            p50_us: s.pct_us(50.0), p99_us: s.pct_us(99.0), recall: None, n: $queries.len(),
        });

        let mut s = Series::new();
        for _ in 0..STATS_REPS {
            let (_, ns) = timed(|| $drv.stats());
            s.push(ns);
        }
        $report.query(QueryRow {
            phase: $phase, docs: $docs, system: $name, op: "stats",
            p50_us: s.pct_us(50.0), p99_us: s.pct_us(99.0), recall: None, n: STATS_REPS,
        });
    }};
}

struct Args {
    scale: usize,
    queries: usize,
    churn_ops: usize,
    batch: usize,
    churn_batch: usize,
    seed: u64,
    out: String,
}

fn parse_args() -> Args {
    let mut args = Args {
        scale: 100_000,
        queries: 40,
        churn_ops: 4_000,
        batch: 1_000,
        churn_batch: 10,
        seed: 42,
        out: "sqlite-bench.jsonl".to_string(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().expect("flag needs a value");
        match flag.as_str() {
            "--scale" => args.scale = val().parse().unwrap(),
            "--queries" => args.queries = val().parse().unwrap(),
            "--churn-ops" => args.churn_ops = val().parse().unwrap(),
            "--batch" => args.batch = val().parse().unwrap(),
            "--churn-batch" => args.churn_batch = val().parse().unwrap(),
            "--seed" => args.seed = val().parse().unwrap(),
            "--out" => args.out = val(),
            other => panic!("unknown flag {other}"),
        }
    }
    args
}

/// One churn transaction's worth of ops; ids are disjoint within a batch
/// so upsert/remove ordering can't matter.
struct Batch {
    upserts: Vec<(u64, String)>,
    removes: Vec<u64>,
}

fn gen_churn(
    rng: &mut Rng,
    live_ids: &mut Vec<u64>,
    next_id: &mut u64,
    ops: usize,
    batch: usize,
) -> Vec<Batch> {
    let mut batches = Vec::new();
    let mut done = 0;
    while done < ops {
        let n = batch.min(ops - done);
        let mut used: HashSet<u64> = HashSet::new();
        let mut b = Batch { upserts: Vec::new(), removes: Vec::new() };
        for _ in 0..n {
            let roll = rng.below(100);
            if roll < 25 || live_ids.is_empty() {
                // insert a brand-new document
                let id = *next_id;
                *next_id += 1;
                live_ids.push(id);
                used.insert(id);
                b.upserts.push((id, gen_doc(rng)));
            } else if roll < 75 {
                // edit an existing document (resample on within-batch reuse)
                let mut idx = rng.below(live_ids.len());
                for _ in 0..8 {
                    if !used.contains(&live_ids[idx]) {
                        break;
                    }
                    idx = rng.below(live_ids.len());
                }
                let id = live_ids[idx];
                if used.insert(id) {
                    b.upserts.push((id, gen_doc(rng)));
                }
            } else {
                let mut idx = rng.below(live_ids.len());
                for _ in 0..8 {
                    if !used.contains(&live_ids[idx]) {
                        break;
                    }
                    idx = rng.below(live_ids.len());
                }
                let id = live_ids[idx];
                if used.insert(id) {
                    live_ids.swap_remove(idx);
                    b.removes.push(id);
                }
            }
        }
        done += n;
        batches.push(b);
    }
    batches
}

fn main() {
    let args = parse_args();
    let start = Instant::now();
    eprintln!(
        "sqlite-bench: scale={} queries/checkpoint={} churn_ops={} batch={} seed={} (ese dim {DIM})",
        args.scale, args.queries, args.churn_ops, args.batch, args.seed
    );

    let base = std::env::temp_dir().join("sqlite-bench");
    std::fs::create_dir_all(&base).unwrap();
    let mut bog = Bog::open(&base.join("bog"));
    let mut sq = Sqlite::open(&base.join("sqlite.db"));
    let mut report = Report::new(std::path::Path::new(&args.out));
    let mut rng = Rng::new(args.seed);

    // the harness's mirror of the live corpus, for recall ground truth
    let mut live: HashMap<u64, [f32; DIM]> = HashMap::new();
    let mut next_id: u64 = 0;

    let mut checkpoints: Vec<usize> = [1_000, 10_000, 100_000, 1_000_000]
        .into_iter()
        .filter(|&c| c <= args.scale)
        .collect();
    if checkpoints.last() != Some(&args.scale) {
        checkpoints.push(args.scale);
    }

    // ── phase 1: ingest with checkpoints ────────────────────────────────
    let mut prev = 0;
    for &cp in &checkpoints {
        let docs: Vec<(u64, String)> = (prev..cp)
            .map(|_| {
                let id = next_id;
                next_id += 1;
                (id, gen_doc(&mut rng))
            })
            .collect();

        for (system, series) in [("bog", {
            let mut s = Series::new();
            for chunk in docs.chunks(args.batch) {
                let (_, ns) = timed(|| bog.apply(chunk, &[]));
                s.push(ns);
            }
            s
        }), ("sqlite", {
            let mut s = Series::new();
            for chunk in docs.chunks(args.batch) {
                let (_, ns) = timed(|| sq.apply(chunk, &[]));
                s.push(ns);
            }
            s
        })] {
            report.ingest(IngestRow {
                docs_to: cp,
                system,
                docs_per_sec: docs.len() as f64 / series.total_secs().max(1e-9),
                batch_p50_ms: series.pct_us(50.0) / 1_000.0,
                batch_p99_ms: series.pct_us(99.0) / 1_000.0,
            });
        }

        for (id, text) in &docs {
            live.insert(*id, ese::encode_single(text));
        }
        eprintln!("[{:>6.1}s] ingested to {cp} docs; measuring", start.elapsed().as_secs_f64());

        let queries: Vec<String> = (0..args.queries).map(|_| gen_query(&mut rng)).collect();
        let gt: Vec<HashSet<u64>> = queries
            .iter()
            .map(|q| exact_topk(&live, &ese::encode_single(q), K))
            .collect();
        measure_system!(report, "checkpoint", cp, "bog", bog, queries, gt);
        measure_system!(report, "checkpoint", cp, "sqlite", sq, queries, gt);
        prev = cp;
    }

    report.disk("after ingest", "bog", bog.disk_bytes());
    report.disk("after ingest", "sqlite", sq.disk_bytes());

    // ── phase 2: churn — edits, inserts, deletes, queried mid-storm ─────
    let mut live_ids: Vec<u64> = live.keys().copied().collect();
    live_ids.sort_unstable(); // HashMap order is nondeterministic; the op schedule must not be
    let batches = gen_churn(&mut rng, &mut live_ids, &mut next_id, args.churn_ops, args.churn_batch);
    eprintln!("[{:>6.1}s] churn: {} ops in {} batches", start.elapsed().as_secs_f64(), args.churn_ops, batches.len());

    let quarter = batches.len().div_ceil(4);
    let (mut bog_w, mut sq_w) = (Series::new(), Series::new());
    let mut applied = 0;
    for (i, b) in batches.iter().enumerate() {
        let (_, ns) = timed(|| bog.apply(&b.upserts, &b.removes));
        bog_w.push(ns);
        let (_, ns) = timed(|| sq.apply(&b.upserts, &b.removes));
        sq_w.push(ns);
        for (id, text) in &b.upserts {
            live.insert(*id, ese::encode_single(text));
        }
        for id in &b.removes {
            live.remove(id);
        }
        applied += b.upserts.len() + b.removes.len();

        if (i + 1) % quarter == 0 || i + 1 == batches.len() {
            let queries: Vec<String> = (0..args.queries).map(|_| gen_query(&mut rng)).collect();
            let gt: Vec<HashSet<u64>> = queries
                .iter()
                .map(|q| exact_topk(&live, &ese::encode_single(q), K))
                .collect();
            measure_system!(report, "churn", applied, "bog", bog, queries, gt);
            measure_system!(report, "churn", applied, "sqlite", sq, queries, gt);
            eprintln!("[{:>6.1}s] churn measured at {applied} ops", start.elapsed().as_secs_f64());
        }
    }
    report.ingest(IngestRow {
        docs_to: applied,
        system: "bog-churn",
        docs_per_sec: applied as f64 / bog_w.total_secs().max(1e-9),
        batch_p50_ms: bog_w.pct_us(50.0) / 1_000.0,
        batch_p99_ms: bog_w.pct_us(99.0) / 1_000.0,
    });
    report.ingest(IngestRow {
        docs_to: applied,
        system: "sqlite-churn",
        docs_per_sec: applied as f64 / sq_w.total_secs().max(1e-9),
        batch_p50_ms: sq_w.pct_us(50.0) / 1_000.0,
        batch_p99_ms: sq_w.pct_us(99.0) / 1_000.0,
    });

    report.disk("after churn", "bog", bog.disk_bytes());
    report.disk("after churn", "sqlite", sq.disk_bytes());

    report.summarize();
    eprintln!(
        "done in {:.1}s — JSONL in {}, corpus {} live docs",
        start.elapsed().as_secs_f64(),
        args.out,
        live.len()
    );
}
