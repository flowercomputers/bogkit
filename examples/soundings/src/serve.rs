//! Soundings — a live embedding-lens writing instrument. `cargo run -p soundings -- serve`
//!
//! Transport is the chat example's shape, doubled: two plain threads each own
//! a fold stream (doc + canon), talk to the axum side over channels.
//!
//!   ws client -> mpsc -> DOC thread (KeyedStream at data/doc.db) -> watch -> ws
//!   /voice    -> mpsc -> CANON thread (KeyedStream at data/trial.db) -> oneshot
//!
//! The split matters: the doc stream opens in milliseconds (kill -9 the
//! process and relaunch — your document is back before the canon finishes
//! reshelving), while the canon HNSW rebuilds in the background.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use anny::metric::Cosine;
use axum::{
    Json, Router,
    extract::Query, extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Html,
    routing::get,
};
use fold::pipeline::{Keyed, Map, Meter, MeterRegistry, MeterSnap, terminal};
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

use crate::gale::{self, GaleState, Ledger, LedgerEntry, NaiveMsg, NaiveState};
use crate::lens::{self, LensDef, LensRegistry};

pub const DIM: usize = ese::DIMENSIONS;

/// Every Nth canon sentence goes into the index (SOUNDINGS_CANON_STEP,
/// default 1 = the full 129,097-sentence canon, matching the other three
/// libraries and every published number; set 10 for a fast dev subsample).
/// With the graph snapshot the reopen cost no longer scales with this,
/// only the one-time build does.
pub fn canon_step() -> usize {
    std::env::var("SOUNDINGS_CANON_STEP").ok().and_then(|s| s.parse().ok()).unwrap_or(1).max(1)
}

// Sentence anchors for the concrete——abstract axis (the G4 finding: anchor
// with example sentences, never words).
const CONCRETE: [&str; 4] = [
    "She wiped the counter and stacked the blue ceramic bowls.",
    "The bus driver counted quarters into a paper cup.",
    "Rain dripped from the fire escape onto the trash bags below.",
    "He tied his boots and zipped the canvas jacket to his chin.",
];
const ABSTRACT: [&str; 4] = [
    "Freedom is the capacity to author one's own life.",
    "Progress depends on institutions that outlive their founders.",
    "Meaning arises when suffering is given a purpose.",
    "Truth survives only where inquiry is unafraid.",
];

const SEED: [&str; 10] = [
    "The fog rolled in over Twin Peaks just after dark, and the city went quiet the way it does before bad news.",
    "Outside the taqueria on Mission, a driver sat with his hazards on, doing the arithmetic of a night that owed him money.",
    "Four dollars, fifteen minutes.",
    "He took the ride anyway.",
    "He was standing in the poetry aisle of the bookstore on Valencia, holding her umbrella like an apology.",
    "She had rehearsed this conversation for three weeks, in showers and stairwells and the long fluorescent silence of the night bus, and now every word of it was gone.",
    "I have watched this city fall down and pick itself up four times in ten years.",
    "Resilience is not a virtue here; it is a rent we pay.",
    "What worries me is not the falling — it is who we ask to do the catching, and what we pay them for it.",
    "The optimal strategy was to leverage the moment efficiently.",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Score {
    pub words: u32,
    pub t: f32, // concrete 0 —— 1 abstract
}

/// Sentence values pack paragraph and order: `"<para>\u{1}<ord>\u{1}<text>"`.
/// Bare strings decode as `(0, None, text)` and the older two-part pack as
/// `(para, None, text)` — `None` means "order by id", so documents written
/// before paragraphs or ordering existed load unchanged.
pub fn enc_val(para: u32, ord: f64, text: &str) -> String {
    format!("{para}\u{0001}{ord}\u{0001}{text}")
}
pub fn dec_val(val: &str) -> (u32, Option<f64>, &str) {
    let mut it = val.splitn(3, '\u{0001}');
    match (it.next(), it.next(), it.next()) {
        (Some(p), Some(o), Some(t)) => (p.parse().unwrap_or(0), o.parse().ok(), t),
        (Some(p), Some(t), None) => (p.parse().unwrap_or(0), None, t),
        _ => (0, None, val),
    }
}

/// The concrete——abstract axis: mean anchor embeddings, projection onto the
/// A→B segment. ONE scoring function for both arms — the fold pipeline's
/// `Map` and the naive arm's full rescan (gale.rs) call the same code.
#[derive(Clone)]
pub struct Axis {
    a: Vec<f32>,
    ab: Vec<f32>,
    ab2: f32,
}
impl Axis {
    pub fn concrete_abstract() -> Axis {
        Axis::from_anchors(&CONCRETE, &ABSTRACT)
    }
    /// Any axis from anchor SENTENCES: mean of the A anchors → mean of the
    /// B anchors. Runtime lenses (lens.rs) use one sentence per pole.
    pub fn from_anchors(a: &[&str], b: &[&str]) -> Axis {
        let a = crate::mean(&crate::embed_all(a));
        let b = crate::mean(&crate::embed_all(b));
        let ab: Vec<f32> = b.iter().zip(&a).map(|(x, y)| x - y).collect();
        let ab2 = crate::dot(&ab, &ab).max(1e-9);
        Axis { a, ab, ab2 }
    }
    /// Score from an already-computed embedding of `text`.
    pub fn score_vec(&self, text: &str, x: &[f32]) -> Score {
        let xa: Vec<f32> = x.iter().zip(&self.a).map(|(v, w)| v - w).collect();
        let t = (crate::dot(&xa, &self.ab) / self.ab2).clamp(0.0, 1.0);
        Score { words: text.split_whitespace().count() as u32, t }
    }
    pub fn score(&self, text: &str) -> Score {
        self.score_vec(text, &crate::embed(text))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: u32,
    /// paragraph this sentence belongs to
    pub para: u32,
    /// position within the document (fractional so inserts never renumber)
    pub ord: f64,
    pub text: String,
    pub words: u32,
    pub t: f32,
    /// Delta-maintained voice cache: refreshed only for the edited key.
    pub voice: Option<VoiceHit>,
    /// Projection onto each user-authored lens, aligned with `DocState.lenses`
    /// (from the `lens_t` view of the lens stream, key `(lens_id, id)`).
    pub lens_t: Vec<f32>,
    /// Length percentile vs the full canon (Histogram sink, see pct.rs);
    /// None until the canon thread has published the distribution.
    pub pct: Option<f32>,
}

/// One pipeline stage as the Meter ops counted it during a single wtx.
/// `deltas` is Σ|delta| through the tap; `us` is time spent below it.
#[derive(Debug, Clone, Default, Serialize)]
struct Stage {
    name: String,
    deltas: u64,
    inserts: u64,
    retracts: u64,
    us: u64,
}

/// The HUD line for one op. `stages` come from Meter taps *inside* the fold
/// pipeline; `wall_us` is a stopwatch around the wtx and is labelled as such.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Hud {
    op: String,
    /// sentence id the op touched (None for seed/resume)
    key: Option<u32>,
    keys: u32,
    wall_us: u64,
    /// stopwatch around the one HNSW voice query for the edited key (0 when
    /// the canon is not open); a read, not a push, so no Meter can count it
    voice_us: u64,
    rows: usize,
    stages: Vec<Stage>,
    /// monotonically increasing op number for the ledger
    seq: u64,
    note: String,
}

/// Diff registry snapshots taken around one wtx into HUD stages:
/// `keys` (deltas in, whole-subtree µs) → `embed` (the Map's own cost:
/// embed − scored) → `scored` (rows written to the scores view). Each µs
/// figure is push + commit time below the tap — sinks write at commit.
fn stages_from(before: &[MeterSnap], after: &[MeterSnap]) -> Vec<Stage> {
    let diff = |name: &str| -> MeterSnap {
        let a = after.iter().find(|s| s.name == name).cloned().unwrap_or_default();
        let b = before.iter().find(|s| s.name == name).cloned().unwrap_or_default();
        a.diff(&b)
    };
    let keys = diff("keys");
    let embed = diff("embed");
    let scored = diff("scored");
    let mut out = vec![
        stage("keys", &keys, keys.ns_total()),
        stage("embed", &embed, embed.ns_total().saturating_sub(scored.ns_total())),
        stage("scored", &scored, scored.ns_total()),
    ];
    // when user lenses exist, every sentence op also flows through the lens
    // stream's taps — show that work too, but only when it happened
    let lk = diff("lens_keys");
    if lk.pushes > 0 {
        out.push(stage("lens_keys", &lk, lk.ns_total()));
    }
    out
}

/// The lens stream's taps for a lens op (author/retract): `lens_keys`
/// (deltas in, embed+project cost) → `lens_scored` (rows into the lens_t view).
fn lens_stages_from(before: &[MeterSnap], after: &[MeterSnap]) -> Vec<Stage> {
    let diff = |name: &str| -> MeterSnap {
        let a = after.iter().find(|s| s.name == name).cloned().unwrap_or_default();
        let b = before.iter().find(|s| s.name == name).cloned().unwrap_or_default();
        a.diff(&b)
    };
    let lk = diff("lens_keys");
    let ls = diff("lens_scored");
    vec![
        stage("lens_keys", &lk, lk.ns_total().saturating_sub(ls.ns_total())),
        stage("lens_scored", &ls, ls.ns_total()),
    ]
}

fn stage(name: &str, s: &MeterSnap, ns: u64) -> Stage {
    Stage {
        name: name.into(),
        deltas: s.weight,
        inserts: s.inserts,
        retracts: s.retracts,
        us: ns / 1000,
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DocState {
    pub rows: Vec<Row>,
    pub hud: Hud,
    /// Edits applied since this process started (seed/resume = 0).
    pub seq: u64,
    /// FNV-1a over `(id, words, t.to_bits())` sorted by id — the referee's
    /// score digest (see gale.rs). User lenses are not part of it.
    pub digest: u64,
    /// User-authored lenses, sorted by id; `Row.lens_t` aligns with this.
    pub lenses: Vec<LensDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum EditOp {
    Edit {
        id: u32,
        text: String,
        #[serde(default)]
        para: Option<u32>,
        #[serde(default)]
        ord: Option<f64>,
    },
    Add {
        text: String,
        #[serde(default)]
        para: u32,
        #[serde(default)]
        ord: f64,
    },
    Remove { id: u32 },
    /// The un-retcon: put a retracted sentence back under its old id — an
    /// upsert on an absent key, i.e. one mutation. Same path as `Edit`,
    /// distinct verb so the HUD can say "restored".
    Restore {
        id: u32,
        text: String,
        #[serde(default)]
        para: u32,
        #[serde(default)]
        ord: f64,
    },
    /// Author a lens from two anchor sentences: one backfill wtx over the doc.
    Lens { a: String, b: String, name: String },
    /// Retract a lens: one wtx removing every `(id, sentence)` key.
    Unlens { id: u32 },
}

/// An edit on the internal channel: the op plus the cursor the gale
/// generator stamped on it (0 for a human edit) and its send time.
#[derive(Debug)]
pub struct Edit {
    pub op: EditOp,
    pub seq: u64,
    pub ts: Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceHit {
    pub id: u32,
    pub score: f32,
    pub title: String,
    pub text: String,
}

/// A request to the library thread: a nearest-voices query, or a switch to
/// another reference library.
pub enum LibReq {
    Voice(String, oneshot::Sender<Vec<VoiceHit>>),
    Switch(String),
    /// project ~1,500 library sentences onto the seeded axis (x) and an
    /// optional caller-anchored axis (y) — the constellation's grey cloud
    Cloud {
        anchors: Option<(String, String)>,
        reply: oneshot::Sender<Vec<(f32, f32, u32)>>,
    },
}

/// gist-cache keyspace ops (reader view): the LLM's reading of a paragraph
/// is an expensive derived view, so it is materialized in a fold Table
/// keyed by content hash — unchanged paragraphs are never re-read.
pub enum CacheReq {
    Get(u64, oneshot::Sender<Option<String>>),
    Put(u64, String),
}

pub fn fnv64(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn reader_cache(rx: mpsc::Receiver<CacheReq>) {
    let mut st = KeyedStream::new(
        std::path::Path::new("data/reader.db"),
        terminal::Table::<u64, String>::new("gists"),
    );
    for req in rx {
        match req {
            CacheReq::Get(h, reply) => {
                let _ = reply.send(st.rtx(|g| g.get(&h)));
            }
            CacheReq::Put(h, gist) => {
                st.wtx(|tx| tx.upsert(&h, &gist));
            }
        }
    }
}

/// A reference library: the corpus the voices, percentiles, and restyle
/// targets aim at. Config, not code — same pipeline, different shelves.
pub struct Library {
    pub id: &'static str,
    pub label: &'static str,
    pub jsonl: &'static str,
    pub db: &'static str,
    pub snap: &'static str,
    pub len_db: &'static str,
}
pub static LIBRARIES: [Library; 4] = [
    Library { id: "canon", label: "novel", jsonl: "data/canon.jsonl", db: "data/trial.db", snap: "data/trial.hnswsnap", len_db: "data/canon_len.db" },
    Library { id: "screen", label: "screenplay", jsonl: "data/screen.jsonl", db: "data/lib-screen.db", snap: "data/lib-screen.hnswsnap", len_db: "data/len-screen.db" },
    Library { id: "contracts", label: "contract", jsonl: "data/contracts.jsonl", db: "data/lib-contracts.db", snap: "data/lib-contracts.hnswsnap", len_db: "data/len-contracts.db" },
    // the personal shelf: prep_self.py over your own writing — never
    // committed, never uploaded; ese embeds it inside the binary's own math
    Library { id: "self", label: "your voice", jsonl: "data/self.jsonl", db: "data/lib-self.db", snap: "data/lib-self.hnswsnap", len_db: "data/len-self.db" },
];
pub fn lib_by_id(id: &str) -> Option<&'static Library> {
    LIBRARIES.iter().find(|l| l.id == id)
}

#[derive(Clone)]
struct AppState {
    edit_tx: mpsc::Sender<Edit>,
    doc_rx: watch::Receiver<DocState>,
    status_rx: watch::Receiver<String>,
    voice_tx: mpsc::Sender<LibReq>,
    naive_tx: mpsc::Sender<NaiveMsg>,
    naive_rx: watch::Receiver<NaiveState>,
    gale_tx: watch::Sender<GaleState>,
    gale_rx: watch::Receiver<GaleState>,
    ledger: Ledger,
    gale_running: Arc<Mutex<bool>>,
    len_rx: watch::Receiver<Option<Arc<crate::pct::LenCdf>>>,
    lib_rx: watch::Receiver<(String, String)>,
    reader_tx: mpsc::Sender<CacheReq>,
}

/// Load `./.env` and `examples/soundings/.env` (KEY=VALUE, quotes stripped,
/// `#` comments) into the process environment — real env vars win. Runs
/// before any thread spawns, so the unsafe set_var is sound.
fn load_dotenv() {
    for path in ["./.env", "examples/soundings/.env"] {
        let Ok(txt) = std::fs::read_to_string(path) else { continue };
        for line in txt.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let (k, v) = (k.trim(), v.trim().trim_matches('"').trim_matches('\''));
                if !k.is_empty() && std::env::var(k).is_err() {
                    unsafe { std::env::set_var(k, v) };
                }
            }
        }
    }
}

pub fn run() {
    load_dotenv();
    let (edit_tx, edit_rx) = mpsc::channel::<Edit>();
    let (doc_tx, doc_rx) = watch::channel(DocState::default());
    let (status_tx, status_rx) = watch::channel("opening the library…".to_string());
    let (voice_tx, voice_rx) = mpsc::channel::<LibReq>();
    let (lib_tx, lib_rx) = watch::channel(("canon".to_string(), "novel".to_string()));
    let (reader_tx, reader_rx) = mpsc::channel::<CacheReq>();
    std::thread::spawn(move || reader_cache(reader_rx));
    let (naive_tx, naive_rx_ch) = mpsc::channel::<NaiveMsg>();
    let (naive_state_tx, naive_rx) = watch::channel(NaiveState::default());
    let (gale_tx, gale_rx) = watch::channel(GaleState { phase: "idle".into(), ..Default::default() });
    let ledger: Ledger = Arc::new(Mutex::new(VecDeque::new()));
    // canon sentence-length distribution: built/opened by the canon thread,
    // read by the doc thread per snapshot (Row.pct)
    let (len_tx, len_rx) = watch::channel::<Option<Arc<crate::pct::LenCdf>>>(None);

    {
        let (voice_tx, status_rx, ledger, len_rx) = (voice_tx.clone(), status_rx.clone(), ledger.clone(), len_rx.clone());
        std::thread::spawn(move || doc_ingest(edit_rx, doc_tx, voice_tx, status_rx, ledger, len_rx));
    }
    std::thread::spawn(move || library_ingest(voice_rx, status_tx, len_tx, lib_tx));
    {
        let ledger = ledger.clone();
        std::thread::spawn(move || gale::naive_ingest(naive_rx_ch, naive_state_tx, ledger, canon_step()));
    }
    serve_http(AppState {
        edit_tx,
        doc_rx,
        status_rx,
        voice_tx,
        naive_tx,
        naive_rx,
        gale_tx,
        gale_rx,
        ledger,
        gale_running: Arc::new(Mutex::new(false)),
        len_rx,
        lib_rx,
        reader_tx,
    });
}

// ---------- doc thread ----------

// The lens stream is a separate store, so its read below is a second
// snapshot, not the same transaction as the doc read — the two can differ by
// at most the op in flight on this thread, which owns both.
macro_rules! doc_snapshot {
    ($st:expr, $lens_st:expr, $defs:expr, $hud:expr, $voices:expr, $seq:expr, $len:expr) => {
        $st.rtx(|(sents, scores)| {
            let cdf = $len.borrow().clone();
            let mut rows: Vec<Row> = sents
                .iter()
                .map(|(id, val)| {
                    let (para, ord, text) = dec_val(&val);
                    let text = text.to_string();
                    let s: Score = scores.get(&id).unwrap_or_default();
                    let pct = cdf.as_ref().map(|c| c.percentile(s.words));
                    Row { id, para, ord: ord.unwrap_or(id as f64), text, words: s.words, t: s.t, voice: $voices.get(&id).cloned(), lens_t: Vec::new(), pct }
                })
                .collect();
            rows.sort_by(|a, b| (a.para, a.ord, a.id).partial_cmp(&(b.para, b.ord, b.id)).unwrap());
            let defs: Vec<LensDef> = $defs.clone();
            if !defs.is_empty() {
                $lens_st.rtx(|lt| {
                    for r in rows.iter_mut() {
                        r.lens_t = defs.iter().map(|d| lt.get(&(d.id, r.id)).unwrap_or(0.5)).collect();
                    }
                });
            }
            let mut hud: Hud = $hud;
            hud.rows = rows.len();
            let mut tuple: Vec<(u32, u32, f32)> = rows.iter().map(|r| (r.id, r.words, r.t)).collect();
            let digest = gale::score_digest(&mut tuple);
            DocState { rows, hud, seq: $seq, digest, lenses: defs }
        })
    };
}

fn doc_ingest(
    rx: mpsc::Receiver<Edit>,
    state_tx: watch::Sender<DocState>,
    voice_tx: mpsc::Sender<LibReq>,
    status_rx: watch::Receiver<String>,
    ledger: Ledger,
    len_rx: watch::Receiver<Option<Arc<crate::pct::LenCdf>>>,
) {
    // axis anchors, computed once; the Map closure below captures them, so
    // scores stay a pure function of the sentence text (fold's contract)
    let axis = Axis::concrete_abstract();

    // three Meter taps: deltas in (`keys`), the embed+score Map (`embed`,
    // whose own cost is embed − scored), rows into the scores view (`scored`).
    // The HUD reads these, not a stopwatch — fold narrates itself.
    let reg = MeterRegistry::new();
    let t0 = Instant::now();
    let mut st = KeyedStream::new(
        std::path::Path::new("data/doc.db"),
        Meter::new(
            &reg,
            "keys",
            (
                terminal::Table::<u32, String>::new("sents"),
                Meter::new(
                    &reg,
                    "embed",
                    Map::new(
                        move |d: &Keyed<u32, String>| Keyed::new(d.key, axis.score(dec_val(&d.val).2)),
                        Meter::new(&reg, "scored", terminal::Table::<u32, Score>::new("scores")),
                    ),
                ),
            ),
        ),
    );
    let open_us = t0.elapsed().as_micros() as u64;

    // ---- user lenses (lens.rs): keys, not code ----
    // defs table: id -> LensDef. Loaded first so the registry is complete
    // before the lens stream can see a push.
    let mut defs_st = KeyedStream::new(
        std::path::Path::new("data/lensdefs.db"),
        terminal::Table::<u32, LensDef>::new("defs"),
    );
    let mut lens_defs: Vec<LensDef> = defs_st.rtx(|defs| defs.iter().map(|(_, d)| d).collect());
    lens_defs.sort_by_key(|d| d.id);
    let registry: LensRegistry = Arc::new(RwLock::new(HashMap::new()));
    for d in &lens_defs {
        registry.write().unwrap().insert(d.id, lens::axis_for(d));
    }
    // lens stream: (lens_id, sentence_id) -> t, through the SAME MeterRegistry
    // so the HUD narrates lens work from inside the pipeline too. Its
    // `lens_t` view is persisted — a relaunch needs no recompute (the resume
    // beat holds for lenses).
    let reg_lens = registry.clone();
    let mut lens_st = KeyedStream::new(
        std::path::Path::new("data/lens.db"),
        Meter::new(
            &reg,
            "lens_keys",
            Map::new(
                move |d: &Keyed<(u32, u32), String>| Keyed::new(d.key, lens::lens_t(&reg_lens, d.key.0, &d.val)),
                Meter::new(&reg, "lens_scored", terminal::Table::<(u32, u32), f32>::new("lens_t")),
            ),
        ),
    );

    let mut seq: u64 = 0;
    let existing = st.rtx(|(sents, _)| sents.iter().count());
    // A fresh document starts EMPTY — the ⚡ demo-paragraph button seeds live,
    // one passage at a time. SOUNDINGS_SEED=1 restores the old tour doc
    // (which the ?demo=retract / chips dry-run scripts still assume).
    let no_seed = std::env::var("SOUNDINGS_SEED").is_err();
    let hud = if existing == 0 && !no_seed {
        let before = reg.snapshot();
        let t = Instant::now();
        st.wtx(|tx| {
            for (i, s) in SEED.iter().enumerate() {
                let para = match i { 0..=3 => 0, 4..=5 => 1, _ => 2 };
                tx.upsert(&(i as u32), &enc_val(para, i as f64, s));
            }
        });
        if !lens_defs.is_empty() {
            lens_st.wtx(|tx| {
                for d in &lens_defs {
                    for (i, s) in SEED.iter().enumerate() {
                        tx.upsert(&(d.id, i as u32), &s.to_string());
                    }
                }
            });
        }
        Hud {
            op: "seed".into(),
            key: None,
            keys: SEED.len() as u32,
            wall_us: t.elapsed().as_micros() as u64,
            voice_us: 0,
            rows: 0,
            stages: stages_from(&before, &reg.snapshot()),
            seq,
            note: String::new(),
        }
    } else {
        // the kill-9 beat: nothing to replay, the doc is simply there —
        // no deltas flowed, so every stage reads 0 and the meters say so
        Hud {
            op: "resume".into(),
            key: None,
            keys: existing as u32,
            wall_us: open_us,
            voice_us: 0,
            rows: 0,
            stages: stages_from(&[], &reg.snapshot()),
            seq,
            note: "0 deltas replayed — views were on disk".into(),
        }
    };
    // Voice cache keyed by sentence id — the fold arm's per-row nearest
    // voice. Only the EDITED key is refreshed on each edit (one HNSW query);
    // every other row's voice is left exactly as it was. That is the delta
    // story the naive arm (gale.rs) contrasts against: it rescans every row.
    let mut voices: HashMap<u32, VoiceHit> = HashMap::new();
    let snap = doc_snapshot!(st, lens_st, lens_defs, hud, voices, seq, len_rx);
    gale::ledger_push(&ledger, LedgerEntry { seq, digest: snap.digest, voices: vec![], wall_us: 0 });
    let _ = state_tx.send(snap);

    for edit in rx {
        let before = reg.snapshot();
        let t = Instant::now();
        let op = edit.op;
        // (verb, touched sentence id, how to mirror this op into the lens
        // stream: Some(text) = upsert (lens, id) for every lens, None = remove)
        let mut mirror: Option<(u32, Option<String>)> = None;
        let mut lens_hud: Option<(u32, String)> = None; // (keys, note) for lens ops
        let (name, key) = match &op {
            EditOp::Edit { id, text, para, ord } => {
                // an edit keeps its sentence's place unless the op moves it
                let (sp, so) = st
                    .rtx(|(sents, _)| sents.get(id).map(|v| { let d = dec_val(&v); (d.0, d.1) }))
                    .unwrap_or((0, None));
                let para = para.unwrap_or(sp);
                let ord = ord.or(so).unwrap_or(*id as f64);
                st.wtx(|tx| tx.upsert(id, &enc_val(para, ord, text)));
                mirror = Some((*id, Some(text.clone())));
                ("upsert", Some(*id))
            }
            EditOp::Add { text, para, ord } => {
                let id = st.rtx(|(sents, _)| sents.iter().map(|(id, _)| id).max().map_or(0, |m| m + 1));
                let ord = if *ord == 0.0 { id as f64 } else { *ord };
                st.wtx(|tx| tx.upsert(&id, &enc_val(*para, ord, text)));
                mirror = Some((id, Some(text.clone())));
                ("insert", Some(id))
            }
            EditOp::Remove { id } => {
                st.wtx(|tx| tx.remove(id));
                mirror = Some((*id, None));
                ("retract", Some(*id))
            }
            EditOp::Lens { a, b, name: lname } => {
                // a NEW id every time — definitions are immutable (fold's
                // determinism contract for the lens Map; see lens.rs)
                let id = lens_defs.iter().map(|d| d.id).max().map_or(1, |m| m + 1);
                let def = LensDef { id, name: lname.clone(), a: a.clone(), b: b.clone() };
                // projections first, definition LAST: a crash in between
                // leaves invisible orphan projections, never a persisted
                // lens whose scores all read 0.5 (external review P1)
                registry.write().unwrap().insert(id, lens::axis_for(&def));
                let sents: Vec<(u32, String)> = st.rtx(|(sents, _)| sents.iter().collect());
                lens_st.wtx(|tx| {
                    for (sid, val) in &sents {
                        tx.upsert(&(id, *sid), &dec_val(val).2.to_string());
                    }
                });
                defs_st.wtx(|tx| tx.upsert(&id, &def));
                lens_defs.push(def);
                lens_defs.sort_by_key(|d| d.id);
                lens_hud = Some((sents.len() as u32, format!("{lname} · {} keys → 1 wtx", sents.len())));
                ("lens", None)
            }
            EditOp::Unlens { id } => {
                // retract every (id, sentence) key while the axis is still
                // registered, THEN drop the definition and the axis
                let sids: Vec<u32> = st.rtx(|(sents, _)| sents.iter().map(|(sid, _)| sid).collect());
                let name_of = lens_defs.iter().find(|d| d.id == *id).map(|d| d.name.clone()).unwrap_or_default();
                // definition first: a crash mid-retract leaves orphan
                // projections (invisible), never a half-defined lens
                defs_st.wtx(|tx| tx.remove(id));
                lens_st.wtx(|tx| {
                    for sid in &sids {
                        tx.remove(&(*id, *sid));
                    }
                });
                lens_defs.retain(|d| d.id != *id);
                registry.write().unwrap().remove(id);
                lens_hud = Some((sids.len() as u32, format!("{name_of} · {} keys retracted → 1 wtx", sids.len())));
                ("unlens", None)
            }
            EditOp::Restore { id, text, para, ord } => {
                let ord = if *ord == 0.0 { *id as f64 } else { *ord };
                st.wtx(|tx| tx.upsert(id, &enc_val(*para, ord, text)));
                mirror = Some((*id, Some(text.clone())));
                ("restore", Some(*id))
            }
        };
        // mirror sentence ops into every user lens's (lens, sentence) key
        if let Some((sid, text)) = &mirror
            && !lens_defs.is_empty()
        {
            lens_st.wtx(|tx| {
                for d in &lens_defs {
                    match text {
                        Some(t) => {
                            tx.upsert(&(d.id, *sid), t);
                        }
                        None => {
                            tx.remove(&(d.id, *sid));
                        }
                    }
                }
            });
        }
        let wall_us = t.elapsed().as_micros() as u64;
        let after = reg.snapshot();
        let stages = if lens_hud.is_some() { lens_stages_from(&before, &after) } else { stages_from(&before, &after) };
        // refresh the voice for the edited key only — one HNSW query, when
        // the canon is open (never block on a shelf that's still reshelving)
        let touched: Option<(u32, &str)> = match &op {
            EditOp::Edit { id, text, .. } | EditOp::Restore { id, text, .. } => Some((*id, text.as_str())),
            EditOp::Add { text, .. } => st
                .rtx(|(sents, _)| sents.iter().map(|(id, _)| id).max())
                .map(|id| (id, text.as_str())),
            EditOp::Remove { id } => {
                voices.remove(id);
                None
            }
            EditOp::Lens { .. } | EditOp::Unlens { .. } => None,
        };
        let mut voice_us = 0u64;
        if let Some((key, text)) = touched {
            let tv = Instant::now();
            if status_rx.borrow().contains("ready ·") {
                let (otx, orx) = oneshot::channel();
                if voice_tx.send(LibReq::Voice(text.to_string(), otx)).is_ok()
                    && let Ok(mut hits) = orx.blocking_recv()
                        && !hits.is_empty() {
                            voices.insert(key, hits.remove(0));
                        }
            }
            voice_us = tv.elapsed().as_micros() as u64;
        }
        seq += 1;
        if edit.seq != 0 && edit.seq != seq {
            eprintln!("gale cursor drift: generator seq {} vs doc seq {}", edit.seq, seq);
        }
        let (keys, note) = lens_hud.unwrap_or((1, String::new()));
        let hud = Hud {
            op: name.into(),
            key,
            keys,
            wall_us,
            voice_us,
            rows: 0,
            stages,
            seq,
            note,
        };
        let snap = doc_snapshot!(st, lens_st, lens_defs, hud, voices, seq, len_rx);
        gale::ledger_push(
            &ledger,
            LedgerEntry {
                seq,
                digest: snap.digest,
                voices: snap.rows.iter().filter_map(|r| r.voice.as_ref().map(|v| (r.id, v.id))).collect(),
                wall_us: snap.hud.wall_us + snap.hud.voice_us,
            },
        );
        let _ = state_tx.send(snap);
    }
}

// ---------- the library thread ----------
// Owns whichever reference library is active. Voice queries answer from it;
// a Switch drops the stream and opens another — first use builds the index
// and checkpoints the HNSW graph, every later open is a snapshot fast-load.
// The doc and lens streams never notice: writing continues during a switch.

fn library_ingest(
    rx: mpsc::Receiver<LibReq>,
    status: watch::Sender<String>,
    len_tx: watch::Sender<Option<Arc<crate::pct::LenCdf>>>,
    lib_tx: watch::Sender<(String, String)>,
) {
    let mut next = "canon".to_string();
    'lib: loop {
        let lib = lib_by_id(&next).unwrap_or(&LIBRARIES[0]);
        let _ = lib_tx.send((lib.id.to_string(), lib.label.to_string()));

        let db = std::path::Path::new(lib.db);
        let snap = std::path::PathBuf::from(lib.snap);
        let marker = format!("{}/.soundings-complete", lib.db);
        let mut fresh = !db.exists();
        if !fresh && !std::path::Path::new(&marker).exists() {
            // a kill mid-build left a partial shelf that would otherwise be
            // accepted as complete forever (external review P1)
            let _ = status.send(format!("the {} shelf was interrupted — rebuilding…", lib.label));
            let _ = std::fs::remove_dir_all(db);
            let _ = std::fs::remove_file(&snap);
            fresh = true;
        }
        if fresh && !std::path::Path::new(lib.jsonl).exists() {
            let _ = status.send(format!("no {} library yet — run its prep script", lib.label));
            while let Ok(req) = rx.recv() {
                match req {
                    LibReq::Voice(_, reply) => {
                        let _ = reply.send(vec![]);
                    }
                    LibReq::Switch(id) => {
                        if id != lib.id && lib_by_id(&id).is_some() {
                            next = id;
                            continue 'lib;
                        }
                    }
                    LibReq::Cloud { reply, .. } => {
                        let _ = reply.send(vec![]);
                    }
                }
            }
            return;
        }

        // the length distribution first: seconds to build, ms to reopen, and
        // the doc thread paints this library's percentiles as soon as it lands
        crate::pct::open_or_build(lib.label, lib.len_db, lib.jsonl, &status, &len_tx);

        // graph snapshot (Séance's contribution, bogkit PR #6): reopen adopts
        // the serialized HNSW instead of re-inserting every row
        let pipeline = || {
            (
                Map::new(
                    |d: &Keyed<u32, (String, String)>| Keyed::new(d.key, ese::encode_single(&d.val.1)),
                    terminal::search::Hnsw::<u32, f32, Cosine, DIM>::new("vecs", Cosine, 42)
                        .with_graph_snapshot(snap.clone()),
                ),
                terminal::Table::<u32, (String, String)>::new("docs"),
            )
        };

        let t0 = Instant::now();
        let st = if fresh {
            let raw = std::fs::read_to_string(lib.jsonl).unwrap_or_default();
            // the canon honors SOUNDINGS_CANON_STEP; prepped libraries are
            // already sampled to size
            let step = if lib.id == "canon" { canon_step() } else { 1 };
            let rows: Vec<(String, String)> = raw
                .lines()
                .step_by(step)
                .filter_map(|l| serde_json::from_str::<crate::CanonRow>(l).ok())
                .map(|r| (r.title, r.text))
                .collect();
            let n = rows.len();
            let mut st = KeyedStream::new(db, pipeline());
            for (i, chunk) in rows.chunks(2048).enumerate() {
                let base = i * 2048;
                st.wtx(|tx| {
                    for (j, r) in chunk.iter().enumerate() {
                        tx.upsert(&((base + j) as u32), r);
                    }
                });
                let _ = status.send(format!("shelving the {} library · {}/{n}…", lib.label, base + chunk.len()));
            }
            st
        } else {
            let _ = status.send(format!("reshelving the {} library…", lib.label));
            KeyedStream::new(db, pipeline())
        };
        let opened = t0.elapsed();
        if fresh || opened.as_secs_f64() > 2.0 {
            let _ = status.send(format!("checkpointing the {} graph…", lib.label));
            let _ = st.rtx(|(vecs, _)| vecs.save_graph());
        }
        let n = st.rtx(|(_, docs)| docs.iter().count());
        let _ = std::fs::write(&marker, n.to_string());
        let _ = status.send(format!(
            "{} ready · {n} sentences · {:.2}s{}",
            lib.label,
            opened.as_secs_f64(),
            if opened.as_secs_f64() < 2.0 { " (snapshot fast-load)" } else { "" }
        ));

        while let Ok(req) = rx.recv() {
            match req {
                LibReq::Voice(text, reply) => {
                    let hits = st.rtx(|(vecs, docs)| {
                        vecs.search(&ese::encode_single(&text))
                            .into_iter()
                            .take(3)
                            .map(|h| {
                                let (title, text) = docs.get(&h.val).unwrap_or_default();
                                VoiceHit { id: h.val, score: h.score, title, text }
                            })
                            .collect::<Vec<_>>()
                    });
                    let _ = reply.send(hits);
                }
                LibReq::Switch(id) => {
                    if id != lib.id && lib_by_id(&id).is_some() {
                        next = id;
                        continue 'lib;
                    }
                }
                LibReq::Cloud { anchors, reply } => {
                    let axis = Axis::concrete_abstract();
                    let user = anchors.map(|(a, b)| Axis::from_anchors(&[a.as_str()], &[b.as_str()]));
                    let pts = st.rtx(|(_, docs)| {
                        let n = docs.iter().count().max(1);
                        let step = (n / 1500).max(1);
                        docs.iter()
                            .enumerate()
                            .filter(|(i, _)| i % step == 0)
                            .map(|(_, (_id, (_title, text)))| {
                                let v = crate::embed(&text);
                                let x = axis.score_vec(&text, &v).t;
                                let y = user.as_ref().map(|u| u.score_vec(&text, &v).t).unwrap_or(-1.0);
                                (x, y, text.split_whitespace().count() as u32)
                            })
                            .collect::<Vec<_>>()
                    });
                    let _ = reply.send(pts);
                }
            }
        }
        return;
    }
}

// ---------- http ----------

/// Local-instrument origin policy: a browser context must be same-origin
/// (or an explicit localhost origin); non-browser clients (curl — no Origin,
/// no Sec-Fetch-Site) pass. Blocks cross-site WebSocket hijacking of the
/// document and <img>-triggered state changes (external review P1).
fn local_origin(headers: &axum::http::HeaderMap) -> bool {
    if let Some(sfs) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok())
        && !matches!(sfs, "same-origin" | "none")
    {
        return false;
    }
    match headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => o.starts_with("http://localhost:") || o.starts_with("http://127.0.0.1:"),
    }
}

#[tokio::main]
async fn serve_http(state: AppState) {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .route("/voice", get(voice))
        .route("/restyle", axum::routing::post(restyle_route))
        .route("/pct", get(pct))
        .route("/library", get(library))
        .route("/cloud", get(cloud))
        .route("/engine", get(engine))
        .route("/engine-info", get(engine_info))
        .route("/reader", axum::routing::post(reader))
        .route("/gale", get(gale_start))
        .with_state(state);

    let port: u16 = std::env::var("SOUNDINGS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(4600);
    println!("soundings running on http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// `/pct?words=N` — length percentile of an N-word sentence against the
/// full canon, straight from the Histogram sink's distribution.
async fn pct(State(st): State<AppState>, Query(params): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    let words: u32 = params.get("words").and_then(|w| w.parse().ok()).unwrap_or(0);
    match st.len_rx.borrow().as_ref() {
        Some(cdf) => Json(serde_json::json!({ "words": words, "pct": cdf.percentile(words), "total": cdf.total, "buckets": cdf.buckets.len() })),
        None => Json(serde_json::json!({ "words": words, "pct": null, "total": 0, "status": "loading" })),
    }
}

/// `/library` — the reference-library selector: `?set=<id>` switches (the
/// library thread does the shelving; status streams over the ws), bare GET
/// reports what's on offer.
async fn library(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    if !local_origin(&headers) {
        return Json(serde_json::json!({ "status": "forbidden" }));
    }
    if let Some(id) = params.get("set") {
        let _ = app.voice_tx.send(LibReq::Switch(id.clone()));
    }
    let (id, label) = app.lib_rx.borrow().clone();
    let libs: Vec<serde_json::Value> = LIBRARIES
        .iter()
        .map(|l| {
            serde_json::json!({
                "id": l.id,
                "label": l.label,
                "prepped": std::path::Path::new(l.jsonl).exists() || std::path::Path::new(l.db).exists(),
            })
        })
        .collect();
    Json(serde_json::json!({ "active": { "id": id, "label": label }, "libs": libs }))
}

/// `/cloud?a=…&b=…` — the constellation's grey backdrop: the active
/// library projected on the seeded axis (x) and, if anchors are given, the
/// caller's authored axis (y). ~1,500 points, computed on demand (µs each).
async fn cloud(State(app): State<AppState>, Query(params): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    if !app.status_rx.borrow().contains("ready ·") {
        return Json(serde_json::json!({ "status": "loading", "pts": [] }));
    }
    let anchors = match (params.get("a"), params.get("b")) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => Some((a.clone(), b.clone())),
        _ => None,
    };
    let (otx, orx) = oneshot::channel();
    if app.voice_tx.send(LibReq::Cloud { anchors, reply: otx }).is_err() {
        return Json(serde_json::json!({ "status": "gone", "pts": [] }));
    }
    match orx.await {
        Ok(pts) => Json(serde_json::json!({ "status": "ok", "pts": pts })),
        Err(_) => Json(serde_json::json!({ "status": "gone", "pts": [] })),
    }
}

#[derive(Deserialize)]
struct ReaderIn {
    paras: Vec<String>,
}

const READER_SYS: &str = "You are a reader, not an editor. In ONE sentence, plainly say what you took from the paragraph — only what is on the page. No advice, no praise, no commentary about writing quality.";

/// `/reader` — the AI performs readership: one gist per paragraph, cached
/// by content hash in a fold Table (unchanged paragraphs are never
/// re-read). Fidelity = cosine(gist, paragraph) via ese; seams = cosine
/// between consecutive gists.
async fn reader(State(app): State<AppState>, Json(inp): Json<ReaderIn>) -> Json<serde_json::Value> {
    let key = crate::restyle::api_key();
    let model = crate::restyle::model_name();
    let mut gists: Vec<Option<String>> = Vec::new();
    let mut cached_flags: Vec<bool> = Vec::new();
    for text in inp.paras.iter().take(16) {
        let h = fnv64(text);
        let (otx, orx) = oneshot::channel();
        let _ = app.reader_tx.send(CacheReq::Get(h, otx));
        if let Ok(Some(g)) = orx.await {
            gists.push(Some(g));
            cached_flags.push(true);
            continue;
        }
        cached_flags.push(false);
        let Some(k) = key.as_deref() else {
            gists.push(None);
            continue;
        };
        match crate::restyle::draft(k, &model, READER_SYS, &format!("Paragraph:\n{text}")).await {
            Ok(g) => {
                let _ = app.reader_tx.send(CacheReq::Put(h, g.clone()));
                gists.push(Some(g));
            }
            Err(_) => gists.push(None),
        }
    }
    let cos = |a: &[f32], b: &[f32]| crate::dot(a, b) / (crate::norm(a) * crate::norm(b)).max(1e-9);
    let gvecs: Vec<Option<Vec<f32>>> = gists.iter().map(|g| g.as_ref().map(|g| crate::embed(g))).collect();
    let paras_out: Vec<serde_json::Value> = inp
        .paras
        .iter()
        .take(16)
        .zip(gists.iter())
        .zip(gvecs.iter())
        .zip(cached_flags.iter())
        .map(|(((text, gist), gv), cached)| match (gist, gv) {
            (Some(g), Some(v)) => {
                let fid = cos(v, &crate::embed(text));
                serde_json::json!({ "gist": g, "fidelity": fid, "cached": cached })
            }
            _ => serde_json::json!({ "gist": null, "cached": cached }),
        })
        .collect();
    let seams: Vec<serde_json::Value> = gvecs
        .windows(2)
        .map(|w| match (&w[0], &w[1]) {
            (Some(a), Some(b)) => serde_json::json!(cos(a, b)),
            _ => serde_json::json!(null),
        })
        .collect();
    let hit = cached_flags.iter().filter(|c| **c).count();
    Json(serde_json::json!({
        "status": if key.is_some() { "ok" } else { "no-provider" },
        "paras": paras_out,
        "seams": seams,
        "cached": hit,
        "reread": cached_flags.len() - hit,
    }))
}

async fn engine() -> Html<&'static str> {
    Html(include_str!("engine.html"))
}

/// `/engine-info` — the machine room's shelf inventory, read straight from
/// the filesystem the instrument maintains: completion markers carry row
/// counts, snapshots report their bytes.
async fn engine_info(State(app): State<AppState>) -> Json<serde_json::Value> {
    let (active, _) = app.lib_rx.borrow().clone();
    let libs: Vec<serde_json::Value> = LIBRARIES
        .iter()
        .map(|l| {
            let sentences = std::fs::read_to_string(format!("{}/.soundings-complete", l.db))
                .ok()
                .and_then(|t| t.trim().parse::<u64>().ok());
            let snap_bytes = std::fs::metadata(l.snap).map(|m| m.len()).unwrap_or(0);
            serde_json::json!({
                "id": l.id,
                "label": l.label,
                "active": l.id == active,
                "sentences": sentences,
                "snap_bytes": snap_bytes,
                "prepped": std::path::Path::new(l.jsonl).exists() || std::path::Path::new(l.db).exists(),
            })
        })
        .collect();
    Json(serde_json::json!({ "libs": libs, "model": crate::restyle::model_name(), "anchors": crate::anchors::bench() }))
}

async fn index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

async fn restyle_route(Json(req): Json<crate::restyle::RestyleReq>) -> Json<crate::restyle::RestyleResp> {
    Json(crate::restyle::run(req).await)
}

async fn voice(
    State(app): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let text = params.get("text").cloned().unwrap_or_default();
    if text.is_empty() || !app.status_rx.borrow().contains("ready ·") {
        return Json(serde_json::json!({ "status": "loading", "hits": [] }));
    }
    let (otx, orx) = oneshot::channel();
    if app.voice_tx.send(LibReq::Voice(text, otx)).is_err() {
        return Json(serde_json::json!({ "status": "gone", "hits": [] }));
    }
    match orx.await {
        Ok(hits) => Json(serde_json::json!({ "status": "ok", "hits": hits })),
        Err(_) => Json(serde_json::json!({ "status": "gone", "hits": [] })),
    }
}

/// `/gale?rate=60&secs=10&seed=42` — start one gale; refuses while one runs
/// or before both the canon and the naive arm are ready.
async fn gale_start(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    if !local_origin(&headers) {
        return Json(serde_json::json!({ "status": "forbidden" }));
    }
    if app.doc_rx.borrow().rows.is_empty() {
        return Json(serde_json::json!({ "status": "empty document" }));
    }
    let num = |k: &str, d: u64| params.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(d);
    let p = gale::GaleParams {
        rate: num("rate", 60).clamp(1, 500) as u32,
        secs: num("secs", 10).clamp(1, 120) as u32,
        seed: num("seed", 42),
    };
    if !app.status_rx.borrow().contains("ready ·") || !app.naive_rx.borrow().ready {
        return Json(serde_json::json!({ "status": "loading" }));
    }
    {
        let mut running = app.gale_running.lock().unwrap();
        if *running {
            return Json(serde_json::json!({ "status": "busy" }));
        }
        *running = true;
    }
    let (rate, secs, seed) = (p.rate, p.secs, p.seed);
    let a = app.clone();
    std::thread::spawn(move || {
        // a panicking gale must never wedge the flag (external review P2)
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gale::run_gale(p, a.edit_tx.clone(), a.naive_tx.clone(), a.doc_rx.clone(), a.naive_rx.clone(), a.ledger.clone(), a.gale_tx.clone());
        }));
        *a.gale_running.lock().unwrap() = false;
        if r.is_err() {
            eprintln!("gale run panicked — running flag cleared");
        }
    });
    Json(serde_json::json!({ "status": "started", "rate": rate, "secs": secs, "seed": seed }))
}

async fn ws_upgrade(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !local_origin(&headers) {
        return (axum::http::StatusCode::FORBIDDEN, "cross-origin denied").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state)).into_response()
}

async fn handle_socket(mut socket: WebSocket, app: AppState) {
    let AppState { edit_tx, mut doc_rx, mut status_rx, mut naive_rx, mut gale_rx, mut lib_rx, .. } = app;
    let combined = |d: &DocState, c: &str, n: &NaiveState, g: &GaleState, l: &(String, String)| {
        serde_json::json!({ "doc": d, "canon": c, "naive": n, "gale": g, "lib": { "id": l.0, "label": l.1 } }).to_string()
    };
    let hello = combined(
        &doc_rx.borrow_and_update(),
        &status_rx.borrow_and_update(),
        &naive_rx.borrow_and_update(),
        &gale_rx.borrow_and_update(),
        &lib_rx.borrow_and_update(),
    );
    if socket.send(Message::text(hello)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            changed = doc_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow_and_update(), &status_rx.borrow(), &naive_rx.borrow(), &gale_rx.borrow(), &lib_rx.borrow());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            changed = status_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow(), &status_rx.borrow_and_update(), &naive_rx.borrow(), &gale_rx.borrow(), &lib_rx.borrow());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            changed = naive_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow(), &status_rx.borrow(), &naive_rx.borrow_and_update(), &gale_rx.borrow(), &lib_rx.borrow());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            changed = gale_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow(), &status_rx.borrow(), &naive_rx.borrow(), &gale_rx.borrow_and_update(), &lib_rx.borrow());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            changed = lib_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow(), &status_rx.borrow(), &naive_rx.borrow(), &gale_rx.borrow(), &lib_rx.borrow_and_update());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            incoming = socket.recv() => {
                let Some(Ok(Message::Text(line))) = incoming else { return; };
                if let Ok(op) = serde_json::from_str::<EditOp>(&line)
                    && edit_tx.send(Edit { op, seq: 0, ts: Instant::now() }).is_err() { return; }
            }
        }
    }
}
