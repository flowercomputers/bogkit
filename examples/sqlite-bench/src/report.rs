//! Timing series, percentiles, and the JSONL + markdown reporting.

use std::io::Write;

/// A series of per-operation wall times, in nanoseconds.
pub struct Series {
    ns: Vec<u64>,
}

impl Series {
    pub fn new() -> Self {
        Series { ns: Vec::new() }
    }

    pub fn push(&mut self, ns: u64) {
        self.ns.push(ns);
    }

    /// Nearest-rank percentile, in microseconds.
    pub fn pct_us(&self, p: f64) -> f64 {
        if self.ns.is_empty() {
            return 0.0;
        }
        let mut sorted = self.ns.clone();
        sorted.sort_unstable();
        let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        sorted[idx] as f64 / 1_000.0
    }

    pub fn total_secs(&self) -> f64 {
        self.ns.iter().sum::<u64>() as f64 / 1e9
    }
}

/// One measured (system, operation) cell at a given corpus size.
pub struct QueryRow {
    pub phase: &'static str,
    pub docs: usize,
    pub system: &'static str,
    pub op: &'static str,
    pub p50_us: f64,
    pub p99_us: f64,
    pub recall: Option<f64>,
    pub n: usize,
}

pub struct IngestRow {
    pub docs_to: usize,
    pub system: &'static str,
    pub docs_per_sec: f64,
    pub batch_p50_ms: f64,
    pub batch_p99_ms: f64,
}

pub struct Report {
    pub queries: Vec<QueryRow>,
    pub ingest: Vec<IngestRow>,
    pub disk: Vec<(String, &'static str, u64)>,
    jsonl: std::fs::File,
}

impl Report {
    pub fn new(path: &std::path::Path) -> Self {
        Report {
            queries: Vec::new(),
            ingest: Vec::new(),
            disk: Vec::new(),
            jsonl: std::fs::File::create(path).expect("create jsonl output"),
        }
    }

    pub fn query(&mut self, row: QueryRow) {
        let recall = row
            .recall
            .map_or("null".to_string(), |r| format!("{r:.4}"));
        writeln!(
            self.jsonl,
            "{{\"phase\":\"{}\",\"docs\":{},\"system\":\"{}\",\"op\":\"{}\",\"p50_us\":{:.2},\"p99_us\":{:.2},\"recall\":{},\"n\":{}}}",
            row.phase, row.docs, row.system, row.op, row.p50_us, row.p99_us, recall, row.n
        )
        .unwrap();
        self.queries.push(row);
    }

    pub fn ingest(&mut self, row: IngestRow) {
        writeln!(
            self.jsonl,
            "{{\"phase\":\"ingest\",\"docs_to\":{},\"system\":\"{}\",\"docs_per_sec\":{:.0},\"batch_p50_ms\":{:.2},\"batch_p99_ms\":{:.2}}}",
            row.docs_to, row.system, row.docs_per_sec, row.batch_p50_ms, row.batch_p99_ms
        )
        .unwrap();
        self.ingest.push(row);
    }

    pub fn disk(&mut self, label: &str, system: &'static str, bytes: u64) {
        writeln!(
            self.jsonl,
            "{{\"phase\":\"disk\",\"label\":\"{label}\",\"system\":\"{system}\",\"bytes\":{bytes}}}"
        )
        .unwrap();
        self.disk.push((label.to_string(), system, bytes));
    }

    /// The final human-readable summary: markdown tables on stdout.
    pub fn summarize(&self) {
        println!("\n## Query latency by corpus size (p50 / p99, µs; recall@10 where applicable)\n");
        for op in ["keyword", "semantic", "hybrid", "stats"] {
            println!("### {op}\n");
            println!("| docs | bog p50 | bog p99 | sqlite p50 | sqlite p99 | p50 speedup | bog recall | sqlite recall |");
            println!("|---|---|---|---|---|---|---|---|");
            let mut sizes: Vec<usize> = self
                .queries
                .iter()
                .filter(|r| r.op == op && r.phase == "checkpoint")
                .map(|r| r.docs)
                .collect();
            sizes.sort_unstable();
            sizes.dedup();
            for docs in sizes {
                let cell = |sys: &str| {
                    self.queries.iter().find(|r| {
                        r.op == op && r.docs == docs && r.system == sys && r.phase == "checkpoint"
                    })
                };
                if let (Some(b), Some(s)) = (cell("bog"), cell("sqlite")) {
                    let rc = |r: Option<f64>| r.map_or("—".into(), |v| format!("{v:.3}"));
                    println!(
                        "| {docs} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1}× | {} | {} |",
                        b.p50_us,
                        b.p99_us,
                        s.p50_us,
                        s.p99_us,
                        s.p50_us / b.p50_us.max(0.001),
                        rc(b.recall),
                        rc(s.recall),
                    );
                }
            }
            println!();
        }

        println!("## Churn: query latency mid-write-storm (p50 / p99, µs)\n");
        println!("| ops applied | system | op | p50 | p99 | recall@10 |");
        println!("|---|---|---|---|---|---|");
        for r in self.queries.iter().filter(|r| r.phase == "churn") {
            let rc = r.recall.map_or("—".into(), |v| format!("{v:.3}"));
            println!(
                "| {} | {} | {} | {:.1} | {:.1} | {} |",
                r.docs, r.system, r.op, r.p50_us, r.p99_us, rc
            );
        }

        println!("\n## Ingest (SQLite's row to win)\n");
        println!("| docs reached | system | docs/sec | batch p50 ms | batch p99 ms |");
        println!("|---|---|---|---|---|");
        for r in &self.ingest {
            println!(
                "| {} | {} | {:.0} | {:.2} | {:.2} |",
                r.docs_to, r.system, r.docs_per_sec, r.batch_p50_ms, r.batch_p99_ms
            );
        }

        println!("\n## Disk\n");
        println!("| stage | system | MB |");
        println!("|---|---|---|");
        for (label, system, bytes) in &self.disk {
            println!("| {label} | {system} | {:.1} |", *bytes as f64 / 1e6);
        }
    }
}
