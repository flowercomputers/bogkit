//! G5 — the gale and the naive contrast arm.
//!
//! Two things live here, both fed by the same scripted edits on the same
//! clock:
//!
//! - the **gale generator**: a thread that replays deterministic word-level
//!   mutations at `rate` edits/s for `secs`, sending every edit to BOTH the
//!   fold doc thread (`serve::doc_ingest`) and the naive arm below;
//! - the **naive arm**: the honest strawman — *same scoring code, no deltas,
//!   no index*. On every edit it re-embeds and re-scores every row and
//!   recomputes every row's nearest voice by a linear cosine scan over the
//!   canon vectors held in memory. When it falls behind it drains its queue,
//!   applies the skipped edits to its doc copy (so its text is right) but
//!   does not recompute for them: those are the **dropped** updates.
//!
//! The **referee** keeps both arms honest: when the naive arm finishes edit N
//! it hashes its rows the same way the doc thread hashes fold's rows at
//! seq N and compares. Scores must match exactly (same function, same
//! inputs). Voices are compared separately as an *agreement* fraction — the
//! fold arm asks the HNSW (approximate), the naive arm scans (exact), so a
//! disagreement there is a fact about ANN, not a bug.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anny::metric::{Cosine, Metric};
use serde::Serialize;
use tokio::sync::watch;

use crate::serve::{Axis, DocState, Edit, EditOp, VoiceHit, DIM};

// ---------- shared types ----------

/// One entry per applied edit, written by the doc thread and read by the
/// naive arm: `(seq, score digest, per-row voice ids, wall µs)`.
pub type Ledger = Arc<Mutex<VecDeque<LedgerEntry>>>;

#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub seq: u64,
    pub digest: u64,
    pub voices: Vec<(u32, u32)>,
    pub wall_us: u64,
}

pub const LEDGER_CAP: usize = 4096;

pub fn ledger_push(ledger: &Ledger, e: LedgerEntry) {
    let mut l = ledger.lock().unwrap();
    if l.len() >= LEDGER_CAP {
        l.pop_front();
    }
    l.push_back(e);
}

/// FNV-1a over rows sorted by id of `(id, words, t.to_bits())` — the score
/// digest both arms compute identically.
pub fn score_digest(rows: &mut [(u32, u32, f32)]) -> u64 {
    rows.sort_by_key(|r| r.0);
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for (id, words, t) in rows.iter() {
        feed(&id.to_le_bytes());
        feed(&words.to_le_bytes());
        feed(&t.to_bits().to_le_bytes());
    }
    h
}

/// What the naive arm receives.
pub enum NaiveMsg {
    /// Gale start: replace the doc copy, reset run statistics.
    /// Replace the naive arm's doc copy. `reset` clears the run counters
    /// (start of a gale); without it the counters survive (post-run restore).
    Sync { rows: Vec<(u32, String)>, seq: u64, reset: bool },
    Edit(Edit),
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NaiveRow {
    pub id: u32,
    pub text: String,
    pub words: u32,
    pub t: f32,
    pub voice: Option<VoiceHit>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct NaiveState {
    pub rows: Vec<NaiveRow>,
    pub seq_rendered: u64,
    pub completed: u64,
    pub dropped: u64,
    pub behind_ms: u64,
    pub behind_ms_max: u64,
    pub stride_us: u64,
    pub stride_p50_us: u64,
    pub referee_checks: u64,
    pub referee_matches: u64,
    pub voice_agree_num: u64,
    pub voice_agree_den: u64,
    pub canon_n: usize,
    pub ready: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    pub fold_completed: u64,
    pub naive_completed: u64,
    pub naive_dropped: u64,
    pub fold_stride_p50_us: u64,
    pub naive_stride_p50_us: u64,
    pub referee_checks: u64,
    pub referee_matches: u64,
    pub voice_agree: (u64, u64),
    pub behind_ms_max: u64,
    pub canon_n: usize,
    pub rate: u32,
    pub secs: u32,
    pub seed: u64,
    pub sent: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GaleState {
    pub running: bool,
    /// "idle" | "sending" | "draining" | "done"
    pub phase: String,
    pub rate: u32,
    pub secs: u32,
    pub seed: u64,
    /// fold's `doc.seq` when the gale started — completed fold updates are
    /// `doc.seq - base`.
    pub base: u64,
    pub sent: u64,
    pub total: u64,
    pub elapsed_ms: u64,
    pub finished: Option<Summary>,
}

// ---------- naive arm ----------

/// Loads the canon in the same order the canon thread indexes it (so ids
/// agree), embeds every sentence, and returns `(vectors, titles)`.
fn load_canon(step: usize) -> (Vec<[f32; DIM]>, Vec<String>) {
    let Ok(raw) = std::fs::read_to_string("data/canon.jsonl") else {
        return (vec![], vec![]);
    };
    let mut vecs = Vec::new();
    let mut titles = Vec::new();
    for l in raw.lines().step_by(step) {
        if let Ok(r) = serde_json::from_str::<crate::CanonRow>(l) {
            vecs.push(ese::encode_single(&r.text));
            titles.push(r.title);
        }
    }
    (vecs, titles)
}

/// Exact nearest neighbour by the same `Cosine` metric the HNSW uses.
fn nearest(q: &[f32; DIM], canon: &[[f32; DIM]]) -> Option<(u32, f32)> {
    let mut best: Option<(u32, f32)> = None;
    for (i, v) in canon.iter().enumerate() {
        let d = <Cosine as Metric<f32>>::distance(&q[..], &v[..]);
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i as u32, d));
        }
    }
    best
}

fn p50(v: &mut [u64]) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

pub fn naive_ingest(
    rx: mpsc::Receiver<NaiveMsg>,
    state_tx: watch::Sender<NaiveState>,
    ledger: Ledger,
    step: usize,
) {
    let axis = Axis::concrete_abstract();
    let (canon, titles) = load_canon(step);
    let mut st = NaiveState { canon_n: canon.len(), ready: true, ..Default::default() };
    let _ = state_tx.send(st.clone());

    let mut doc: Vec<(u32, String)> = Vec::new();
    let mut strides: Vec<u64> = Vec::new();

    let apply = |doc: &mut Vec<(u32, String)>, op: &EditOp| match op {
        EditOp::Edit { id, text, .. } | EditOp::Restore { id, text, .. } => {
            if let Some(r) = doc.iter_mut().find(|r| r.0 == *id) {
                r.1 = text.clone();
            } else {
                doc.push((*id, text.clone()));
            }
        }
        EditOp::Add { text, .. } => {
            let id = doc.iter().map(|r| r.0).max().map_or(0, |m| m + 1);
            doc.push((id, text.clone()));
        }
        EditOp::Remove { id } => doc.retain(|r| r.0 != *id),
        // user lenses live in the fold arm only — the naive arm rescans the
        // seeded axis, as its footnote says
        EditOp::Lens { .. } | EditOp::Unlens { .. } => {}
    };

    for msg in &rx {
        // a Sync paints the starting doc once but is not an "update"
        let mut is_sync = false;
        let (mut seq, mut ts) = match msg {
            NaiveMsg::Sync { rows, seq, reset } => {
                doc = rows;
                is_sync = true;
                if reset {
                    strides.clear();
                    st = NaiveState { canon_n: canon.len(), ready: true, seq_rendered: seq, ..Default::default() };
                } else {
                    st.seq_rendered = seq;
                }
                (seq, Instant::now())
            }
            NaiveMsg::Edit(e) => {
                apply(&mut doc, &e.op);
                (e.seq, e.ts)
            }
        };
        // Behind? Take everything queued, apply it, recompute once — the
        // skipped recomputes are the dropped updates.
        if !is_sync {
            while let Ok(NaiveMsg::Edit(e)) = rx.try_recv() {
                apply(&mut doc, &e.op);
                seq = e.seq;
                ts = e.ts;
                st.dropped += 1;
            }
        }

        // full recompute: every row, from scratch, no index
        let t0 = Instant::now();
        let mut rows: Vec<NaiveRow> = Vec::with_capacity(doc.len());
        for (id, text) in &doc {
            let x = crate::embed(text);
            let s = axis.score_vec(text, &x);
            let mut q = [0f32; DIM];
            q.copy_from_slice(&x);
            let voice = if canon.is_empty() {
                None
            } else {
                nearest(&q, &canon).map(|(i, d)| VoiceHit {
                    id: i,
                    score: d,
                    title: titles[i as usize].clone(),
                    text: String::new(),
                })
            };
            rows.push(NaiveRow { id: *id, text: text.clone(), words: s.words, t: s.t, voice });
        }
        rows.sort_by_key(|r| r.id);
        let stride = t0.elapsed().as_micros() as u64;
        if is_sync {
            st.rows = rows;
            let _ = state_tx.send(st.clone());
            continue;
        }
        strides.push(stride);

        // referee: same digest as the doc thread, at the same cursor
        let mut tuple: Vec<(u32, u32, f32)> = rows.iter().map(|r| (r.id, r.words, r.t)).collect();
        let digest = score_digest(&mut tuple);
        let entry = ledger.lock().unwrap().iter().find(|e| e.seq == seq).cloned();
        if let Some(e) = entry {
            st.referee_checks += 1;
            if e.digest == digest {
                st.referee_matches += 1;
            }
            for (rid, fold_voice) in &e.voices {
                if let Some(r) = rows.iter().find(|r| r.id == *rid)
                    && let Some(v) = &r.voice {
                        st.voice_agree_den += 1;
                        if v.id == *fold_voice {
                            st.voice_agree_num += 1;
                        }
                    }
            }
        }

        st.rows = rows;
        st.seq_rendered = seq;
        st.completed += 1;
        st.stride_us = stride;
        st.stride_p50_us = p50(&mut strides.clone());
        st.behind_ms = ts.elapsed().as_millis() as u64;
        st.behind_ms_max = st.behind_ms_max.max(st.behind_ms);
        let _ = state_tx.send(st.clone());
    }
}

// ---------- gale generator ----------

pub struct GaleParams {
    pub rate: u32,
    pub secs: u32,
    pub seed: u64,
}

const WORDS: [&str; 40] = [
    "quiet", "wet", "late", "cold", "amber", "hollow", "borrowed", "slow", "sudden", "plain",
    "salt", "iron", "paper", "glass", "rain", "smoke", "ledger", "corner", "window", "receipt",
    "again", "almost", "barely", "still", "twice", "once", "somewhere", "here", "north", "downhill",
    "hummed", "waited", "counted", "folded", "leaned", "watched", "paid", "kept", "lost", "returned",
];

struct XorShift(u64);
impl XorShift {
    fn new(seed: u64) -> Self {
        XorShift(seed.max(1) ^ 0x9E3779B97F4A7C15)
    }
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(text: &str, rng: &mut XorShift) -> String {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let w = WORDS[rng.below(WORDS.len())];
    match rng.below(3) {
        0 => words.push(w),
        1 if !words.is_empty() => {
            let i = rng.below(words.len());
            words[i] = w;
        }
        _ if words.len() > 3 => {
            let i = rng.below(words.len());
            words.remove(i);
        }
        _ => words.push(w),
    }
    words.join(" ")
}

/// Runs one gale to completion on the calling thread. `edit_tx` feeds the
/// fold doc thread; `naive_tx` feeds the naive arm; both get identical
/// `Edit`s with the same `seq` and `ts`.
pub fn run_gale(
    p: GaleParams,
    edit_tx: mpsc::Sender<Edit>,
    naive_tx: mpsc::Sender<NaiveMsg>,
    doc_rx: watch::Receiver<DocState>,
    naive_rx: watch::Receiver<NaiveState>,
    ledger: Ledger,
    gale_tx: watch::Sender<GaleState>,
) {
    let total = (p.rate as u64) * (p.secs as u64);
    let mut rng = XorShift::new(p.seed);

    // both arms start from the same cursor: fold's current seq is the base
    let (rows, base) = {
        let d = doc_rx.borrow();
        (d.rows.iter().map(|r| (r.id, r.text.clone())).collect::<Vec<_>>(), d.seq)
    };
    let originals: Vec<(u32, String)> = rows.clone();
    let _ = naive_tx.send(NaiveMsg::Sync { rows: rows.clone(), seq: base, reset: true });

    let mut gs = GaleState {
        running: true,
        phase: "sending".into(),
        rate: p.rate,
        secs: p.secs,
        seed: p.seed,
        base,
        sent: 0,
        total,
        elapsed_ms: 0,
        finished: None,
    };
    let _ = gale_tx.send(gs.clone());

    let start = Instant::now();
    let period = Duration::from_secs_f64(1.0 / p.rate.max(1) as f64);
    let n = rows.len().max(1);
    let mut current: Vec<(u32, String)> = rows.clone();
    for k in 0..total {
        let idx = (k as usize) % n;
        let (id, ref orig) = originals[idx];
        let text = if k % 7 == 6 { orig.clone() } else { mutate(&current[idx].1, &mut rng) };
        current[idx].1 = text.clone();
        let e = Edit { op: EditOp::Edit { id, text, para: None, ord: None }, seq: base + k + 1, ts: Instant::now() };
        let e2 = Edit { op: e.op.clone(), seq: e.seq, ts: e.ts };
        if edit_tx.send(e).is_err() {
            break;
        }
        let _ = naive_tx.send(NaiveMsg::Edit(e2));
        gs.sent = k + 1;
        gs.elapsed_ms = start.elapsed().as_millis() as u64;
        let _ = gale_tx.send(gs.clone());
        let due = start + period * (k as u32 + 1);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
    }

    // draining: give the naive arm up to 60s to reach the last cursor
    gs.phase = "draining".into();
    let _ = gale_tx.send(gs.clone());
    let last_seq = base + gs.sent;
    let drain_start = Instant::now();
    while naive_rx.borrow().seq_rendered < last_seq && drain_start.elapsed() < Duration::from_secs(60) {
        gs.elapsed_ms = start.elapsed().as_millis() as u64;
        let _ = gale_tx.send(gs.clone());
        std::thread::sleep(Duration::from_millis(50));
    }

    // summary — every number is a measurement from the run just finished
    let ns = naive_rx.borrow().clone();
    let (fold_completed, fold_p50) = {
        let l = ledger.lock().unwrap();
        let mut walls: Vec<u64> = l
            .iter()
            .filter(|e| e.seq > base && e.seq <= last_seq)
            .map(|e| e.wall_us)
            .collect();
        (walls.len() as u64, p50(&mut walls))
    };
    gs.phase = "done".into();
    gs.running = false;
    gs.elapsed_ms = start.elapsed().as_millis() as u64;
    gs.finished = Some(Summary {
        fold_completed,
        naive_completed: ns.completed,
        naive_dropped: ns.dropped,
        fold_stride_p50_us: fold_p50,
        naive_stride_p50_us: ns.stride_p50_us,
        referee_checks: ns.referee_checks,
        referee_matches: ns.referee_matches,
        voice_agree: (ns.voice_agree_num, ns.voice_agree_den),
        behind_ms_max: ns.behind_ms_max,
        canon_n: ns.canon_n,
        rate: p.rate,
        secs: p.secs,
        seed: p.seed,
        sent: gs.sent,
    });
    let _ = gale_tx.send(gs);

    // put the document back the way the writer left it. The restores go
    // through the fold arm as ordinary edits (so its ledger stays honest —
    // they land after the timed window and are not in the Summary) and reach
    // the naive arm as one Sync, which repaints without counting an update.
    let mut restore_seq = last_seq;
    for (idx, (id, orig)) in originals.iter().enumerate() {
        if current[idx].1 != *orig {
            restore_seq += 1;
            let e = Edit { op: EditOp::Edit { id: *id, text: orig.clone(), para: None, ord: None }, seq: restore_seq, ts: Instant::now() };
            if edit_tx.send(e).is_err() {
                break;
            }
        }
    }
    let _ = naive_tx.send(NaiveMsg::Sync { rows: originals, seq: restore_seq, reset: false });
}
