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
/// default 10) — samples all 20 books. With the graph snapshot the reopen
/// cost no longer scales with this, only the one-time build does.
pub fn canon_step() -> usize {
    std::env::var("SOUNDINGS_CANON_STEP").ok().and_then(|s| s.parse().ok()).unwrap_or(10).max(1)
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

pub type VoiceReq = (String, oneshot::Sender<Vec<VoiceHit>>);

#[derive(Clone)]
struct AppState {
    edit_tx: mpsc::Sender<Edit>,
    doc_rx: watch::Receiver<DocState>,
    status_rx: watch::Receiver<String>,
    voice_tx: mpsc::Sender<VoiceReq>,
    naive_tx: mpsc::Sender<NaiveMsg>,
    naive_rx: watch::Receiver<NaiveState>,
    gale_tx: watch::Sender<GaleState>,
    gale_rx: watch::Receiver<GaleState>,
    ledger: Ledger,
    gale_running: Arc<Mutex<bool>>,
    len_rx: watch::Receiver<Option<Arc<crate::pct::LenCdf>>>,
}

pub fn run() {
    let (edit_tx, edit_rx) = mpsc::channel::<Edit>();
    let (doc_tx, doc_rx) = watch::channel(DocState::default());
    let (status_tx, status_rx) = watch::channel("opening canon…".to_string());
    let (voice_tx, voice_rx) = mpsc::channel::<VoiceReq>();
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
    std::thread::spawn(move || canon_ingest(voice_rx, status_tx, len_tx));
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
    voice_tx: mpsc::Sender<VoiceReq>,
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
    // SOUNDINGS_NO_SEED=1 starts an empty document instead of the demo doc —
    // for writing your own piece rather than touring the seeded one
    let no_seed = std::env::var("SOUNDINGS_NO_SEED").is_ok();
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
                defs_st.wtx(|tx| tx.upsert(&id, &def));
                registry.write().unwrap().insert(id, lens::axis_for(&def));
                lens_defs.push(def);
                lens_defs.sort_by_key(|d| d.id);
                // the backfill: every sentence in the doc, ONE wtx
                let sents: Vec<(u32, String)> = st.rtx(|(sents, _)| sents.iter().collect());
                lens_st.wtx(|tx| {
                    for (sid, val) in &sents {
                        tx.upsert(&(id, *sid), &dec_val(val).2.to_string());
                    }
                });
                lens_hud = Some((sents.len() as u32, format!("{lname} · {} keys → 1 wtx", sents.len())));
                ("lens", None)
            }
            EditOp::Unlens { id } => {
                // retract every (id, sentence) key while the axis is still
                // registered, THEN drop the definition and the axis
                let sids: Vec<u32> = st.rtx(|(sents, _)| sents.iter().map(|(sid, _)| sid).collect());
                let name_of = lens_defs.iter().find(|d| d.id == *id).map(|d| d.name.clone()).unwrap_or_default();
                lens_st.wtx(|tx| {
                    for sid in &sids {
                        tx.remove(&(*id, *sid));
                    }
                });
                defs_st.wtx(|tx| tx.remove(id));
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
            if status_rx.borrow().starts_with("canon ready") {
                let (otx, orx) = oneshot::channel();
                if voice_tx.send((text.to_string(), otx)).is_ok()
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

// ---------- canon thread ----------

fn canon_ingest(
    rx: mpsc::Receiver<VoiceReq>,
    status: watch::Sender<String>,
    len_tx: watch::Sender<Option<Arc<crate::pct::LenCdf>>>,
) {
    // the length histogram first: seconds to build, ms to reopen, and the
    // doc thread paints percentiles as soon as it lands
    crate::pct::open_or_build(&status, &len_tx);

    let db = std::path::Path::new("data/trial.db");
    let snap = std::path::Path::new("data/trial.hnswsnap");
    let fresh = !db.exists();
    // graph snapshot (Séance's contribution, bogkit PR #6): reopen adopts the
    // serialized HNSW instead of re-inserting every row
    let pipeline = || {
        (
            Map::new(
                |d: &Keyed<u32, (String, String)>| Keyed::new(d.key, ese::encode_single(&d.val.1)),
                terminal::search::Hnsw::<u32, f32, Cosine, DIM>::new("vecs", Cosine, 42)
                    .with_graph_snapshot(snap),
            ),
            terminal::Table::<u32, (String, String)>::new("docs"),
        )
    };

    let t0 = Instant::now();
    let st = if fresh {
        let Ok(raw) = std::fs::read_to_string("data/canon.jsonl") else {
            let _ = status.send("no canon — run scripts/prep_corpus.py".into());
            for (_, reply) in rx {
                let _ = reply.send(vec![]);
            }
            return;
        };
        let rows: Vec<(String, String)> = raw
            .lines()
            .step_by(canon_step())
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
            let _ = status.send(format!("indexing canon {}/{n}…", base + chunk.len()));
        }
        st
    } else {
        let _ = status.send("reshelving canon (reopen)…".into());
        KeyedStream::new(db, pipeline())
    };
    let opened = t0.elapsed();
    // a slow open means the snapshot was absent or stale and the graph was
    // rebuilt row-by-row — capture it so the next open is instant
    if fresh || opened.as_secs_f64() > 2.0 {
        let _ = status.send("checkpointing canon graph…".into());
        let _ = st.rtx(|(vecs, _)| vecs.save_graph());
    }
    let n = st.rtx(|(_, docs)| docs.iter().count());
    let _ = status.send(format!(
        "canon ready · {n} sentences · {:.2}s{}",
        opened.as_secs_f64(),
        if opened.as_secs_f64() < 2.0 { " (snapshot fast-load)" } else { "" }
    ));

    for (text, reply) in rx {
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
}

// ---------- http ----------

#[tokio::main]
async fn serve_http(state: AppState) {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .route("/voice", get(voice))
        .route("/restyle", axum::routing::post(restyle_route))
        .route("/pct", get(pct))
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
    if text.is_empty() || !app.status_rx.borrow().starts_with("canon ready") {
        return Json(serde_json::json!({ "status": "loading", "hits": [] }));
    }
    let (otx, orx) = oneshot::channel();
    if app.voice_tx.send((text, otx)).is_err() {
        return Json(serde_json::json!({ "status": "gone", "hits": [] }));
    }
    match orx.await {
        Ok(hits) => Json(serde_json::json!({ "status": "ok", "hits": hits })),
        Err(_) => Json(serde_json::json!({ "status": "gone", "hits": [] })),
    }
}

/// `/gale?rate=60&secs=10&seed=42` — start one gale; refuses while one runs
/// or before both the canon and the naive arm are ready.
async fn gale_start(State(app): State<AppState>, Query(params): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
    let num = |k: &str, d: u64| params.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(d);
    let p = gale::GaleParams {
        rate: num("rate", 60).clamp(1, 500) as u32,
        secs: num("secs", 10).clamp(1, 120) as u32,
        seed: num("seed", 42),
    };
    if !app.status_rx.borrow().starts_with("canon ready") || !app.naive_rx.borrow().ready {
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
        gale::run_gale(p, a.edit_tx.clone(), a.naive_tx.clone(), a.doc_rx.clone(), a.naive_rx.clone(), a.ledger.clone(), a.gale_tx.clone());
        *a.gale_running.lock().unwrap() = false;
    });
    Json(serde_json::json!({ "status": "started", "rate": rate, "secs": secs, "seed": seed }))
}

async fn ws_upgrade(State(state): State<AppState>, ws: WebSocketUpgrade) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, app: AppState) {
    let AppState { edit_tx, mut doc_rx, mut status_rx, mut naive_rx, mut gale_rx, .. } = app;
    let combined = |d: &DocState, c: &str, n: &NaiveState, g: &GaleState| {
        serde_json::json!({ "doc": d, "canon": c, "naive": n, "gale": g }).to_string()
    };
    let hello = combined(
        &doc_rx.borrow_and_update(),
        &status_rx.borrow_and_update(),
        &naive_rx.borrow_and_update(),
        &gale_rx.borrow_and_update(),
    );
    if socket.send(Message::text(hello)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            changed = doc_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow_and_update(), &status_rx.borrow(), &naive_rx.borrow(), &gale_rx.borrow());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            changed = status_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow(), &status_rx.borrow_and_update(), &naive_rx.borrow(), &gale_rx.borrow());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            changed = naive_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow(), &status_rx.borrow(), &naive_rx.borrow_and_update(), &gale_rx.borrow());
                if socket.send(Message::text(msg)).await.is_err() { return; }
            }
            changed = gale_rx.changed() => {
                if changed.is_err() { return; }
                let msg = combined(&doc_rx.borrow(), &status_rx.borrow(), &naive_rx.borrow(), &gale_rx.borrow_and_update());
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
