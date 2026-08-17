//! Sundew: a live, retractable briefing for the next coding agent.
//!
//! Alinery's filesystem is the database. This crate is the query engine
//! that filesystem never had: one KeyedStream of Alinery-shaped chunks
//! (ticket, artifact, comment, wiki) fans out to BM25, ESE+HNSW,
//! Potion+HNSW, and a doc table. The product is not another search box — it is a packed,
//! token-budgeted briefing that resticks when a human comment lands or a
//! stale decision is retracted.
//!
//! Same Chunk schema a later Alinery sidecar would ingest from
//! `.alinery/` and `docs/wiki/`. Files stay the source of truth; Fold
//! maintains the views.
//!
//!   cargo run -p sundew -- --probe "how should we store refresh tokens?"
//!   cargo run -p sundew   # http://localhost:3000

use std::collections::HashMap;
use std::sync::{Arc, mpsc};

use anny::metric::Cosine;
use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::header,
    response::{Html, IntoResponse},
    routing::get,
};
use fold::pipeline::{Aggregate, KeyBy, Keyed, Map, Scored, Unkey, terminal};
use fold::stream::KeyedStream;
use model2vec_rs::model::StaticModel;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

const DIM: usize = ese::DIMENSIONS;
const POTION_DIM: usize = 256;
const POTION_MODEL: &str = "minishlab/potion-code-16M-v2";
const RRF_K: f64 = 60.0;
const BM25_WEIGHT: f64 = 1.0;
const TOKEN_BUDGET: usize = 1800;
const DEFAULT_QUERY: &str = "how should we store refresh tokens?";

const COMMENT_ID: &str = "auth-refresh/comment/03-design-reject";
const STALE_WIKI_ID: &str = "wiki/session-lifecycle";

/// The shared record. Sundew seeds these from a fixture. An Alinery
/// sidecar would upsert the same shape from files.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Chunk {
    id: String,
    kind: Kind,
    task: String,
    phase: String,
    title: String,
    body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Ticket,
    Artifact,
    Comment,
    Wiki,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Goal {
    #[default]
    Balanced,
    Locate,
    Explain,
}

impl Goal {
    fn weights(self) -> (f64, f64) {
        match self {
            Goal::Balanced => (0.5, 0.5),
            Goal::Locate => (0.2, 0.8),
            Goal::Explain => (0.8, 0.2),
        }
    }
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Ticket => "ticket",
            Kind::Artifact => "artifact",
            Kind::Comment => "comment",
            Kind::Wiki => "wiki",
        }
    }

    fn prior(self) -> f64 {
        match self {
            Kind::Comment => 1.45,
            Kind::Wiki => 1.15,
            Kind::Artifact => 1.0,
            Kind::Ticket => 0.85,
        }
    }
}

impl Chunk {
    fn search_text(&self) -> String {
        format!("{} {} {}", self.title, self.kind.as_str(), self.body)
    }

    fn tokens(&self) -> usize {
        self.search_text().len().div_ceil(4).max(1)
    }
}

fn load_potion() -> Arc<StaticModel> {
    Arc::new(
        StaticModel::from_pretrained(POTION_MODEL, None, Some(true), None)
            .unwrap_or_else(|error| panic!("load {POTION_MODEL}: {error}")),
    )
}

fn potion_encode(model: &StaticModel, text: &str) -> [f32; POTION_DIM] {
    model
        .encode_single(text)
        .try_into()
        .unwrap_or_else(|vector: Vec<f32>| {
            panic!(
                "{POTION_MODEL} dimension is {}, expected {POTION_DIM}",
                vector.len()
            )
        })
}

#[derive(Debug, Clone, Serialize)]
struct PackedLine {
    id: String,
    kind: Kind,
    task: String,
    phase: String,
    title: String,
    body: String,
    score: f64,
    tokens: usize,
    why: String,
}

#[derive(Debug, Clone, Serialize)]
struct Briefing {
    query: String,
    goal: Goal,
    models: Vec<ModelTrace>,
    lines: Vec<PackedLine>,
    tokens: usize,
    budget: usize,
    pack_us: u64,
    seed: String,
}

#[derive(Debug, Clone, Serialize)]
struct ModelTrace {
    key: &'static str,
    name: &'static str,
    weight: f64,
    top: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SwampItem {
    id: String,
    kind: Kind,
    task: String,
    title: String,
}

#[derive(Debug, Clone, Serialize)]
struct KindCount {
    kind: String,
    n: usize,
}

#[derive(Debug, Clone, Serialize)]
struct RetractGhost {
    id: String,
    title: String,
    left: String,
}

#[derive(Debug, Clone, Serialize)]
struct Snapshot {
    swamp: Vec<SwampItem>,
    briefing: Briefing,
    kinds: Vec<KindCount>,
    live: LiveFlags,
    ghost: Option<RetractGhost>,
}

#[derive(Debug, Clone, Serialize)]
struct LiveFlags {
    comment_landed: bool,
    wiki_retracted: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ClientMsg {
    op: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    goal: Option<Goal>,
}

enum Ingest {
    Ask(String, Option<Goal>),
    Comment,
    Retract,
    Reset,
}

macro_rules! snapshot {
    ($st:expr, $potion:expr, $query:expr, $goal:expr) => {{
        let query: String = $query;
        $st.rtx(|(bm25, ese_vecs, potion_vecs, docs, kind_counts)| {
            let mut chunks: Vec<Chunk> = docs.iter().map(|(_, c)| c).collect();
            chunks.sort_by(|a, b| a.id.cmp(&b.id));

            let started = std::time::Instant::now();
            let keyword = bm25.search(&query, 10);
            let semantic = ese_vecs.search(&ese::encode_single(&query));
            let code = potion_vecs.search(&potion_encode(($potion).as_ref(), &query));
            let briefing = pack(&query, $goal, &keyword, &semantic, &code, &chunks);
            let pack_us = started.elapsed().as_micros() as u64;
            let briefing = Briefing {
                pack_us,
                ..briefing
            };

            let mut kinds: Vec<KindCount> = kind_counts
                .iter()
                .map(|(kind, n)| KindCount {
                    kind,
                    n: n.max(0) as usize,
                })
                .collect();
            kinds.sort_by(|a, b| a.kind.cmp(&b.kind));

            let swamp = chunks
                .iter()
                .map(|c| SwampItem {
                    id: c.id.clone(),
                    kind: c.kind,
                    task: c.task.clone(),
                    title: c.title.clone(),
                })
                .collect();

            let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
            let wiki_retracted = !ids.contains(&STALE_WIKI_ID);
            Snapshot {
                swamp,
                briefing,
                kinds,
                live: LiveFlags {
                    comment_landed: ids.contains(&COMMENT_ID),
                    wiki_retracted,
                },
                ghost: wiki_retracted.then(|| RetractGhost {
                    id: STALE_WIKI_ID.to_string(),
                    title: "session-lifecycle".to_string(),
                    left: "BM25 + ESE + Potion + table".to_string(),
                }),
            }
        })
    }};
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--probe") => {
            let query = args.collect::<Vec<_>>().join(" ");
            let query = if query.is_empty() {
                DEFAULT_QUERY.to_string()
            } else {
                query
            };
            probe(&query);
        }
        Some("--script") => {
            script();
        }
        Some("--help") | Some("-h") => {
            eprintln!(
                "sundew\n  cargo run -p sundew -- --probe [query]\n  cargo run -p sundew -- --script\n  cargo run -p sundew\n  SUNDEW_PORT=3000"
            );
        }
        Some(other) => {
            eprintln!("unknown arg {other:?}; try --probe or no args");
            std::process::exit(2);
        }
        None => serve(),
    }
}

macro_rules! open_db {
    ($potion:expr) => {{
        let db_path = std::env::temp_dir().join("bog-kit-sundew.db");
        let _ = std::fs::remove_dir_all(&db_path);
        let potion_index = Arc::clone(&$potion);
        KeyedStream::new(
            &db_path,
            (
                Map::new(
                    |d: &Keyed<String, Chunk>| Keyed::new(d.key.clone(), d.val.search_text()),
                    terminal::search::Bm25::new("bm25"),
                ),
                Map::new(
                    |d: &Keyed<String, Chunk>| {
                        Keyed::new(d.key.clone(), ese::encode_single(&d.val.search_text()))
                    },
                    terminal::search::Hnsw::<String, f32, Cosine, DIM>::new("vecs", Cosine, 42),
                ),
                Map::new(
                    move |d: &Keyed<String, Chunk>| {
                        Keyed::new(
                            d.key.clone(),
                            potion_encode(&potion_index, &d.val.search_text()),
                        )
                    },
                    terminal::search::Hnsw::<String, f32, Cosine, POTION_DIM>::new(
                        "vecs_potion",
                        Cosine,
                        43,
                    ),
                ),
                terminal::Table::new("docs"),
                Unkey::new(KeyBy::new(
                    |c: &Chunk| c.kind.as_str().to_string(),
                    Aggregate::new(
                        "by_kind",
                        |acc: &mut i64, _c: &Chunk, delta| *acc += delta as i64,
                        terminal::Table::new("kind_counts"),
                    ),
                )),
            ),
        )
    }};
}

fn seed(st: &mut KeyedStream<String, Chunk, impl fold::pipeline::Push<Keyed<String, Chunk>>>) {
    let chunks = seed_chunks();
    st.wtx(|tx| {
        for chunk in &chunks {
            tx.upsert(&chunk.id, chunk);
        }
    });
}

fn probe(query: &str) {
    let potion = load_potion();
    let mut st = open_db!(potion);
    seed(&mut st);
    let snap = snapshot!(st, potion, query.to_string(), Goal::Balanced);
    println!("query: {}", snap.briefing.query);
    println!(
        "pack: {} tokens / {}  ({} µs)",
        snap.briefing.tokens, snap.briefing.budget, snap.briefing.pack_us
    );
    println!(
        "swamp: {} chunks  comment_landed={}  wiki_retracted={}",
        snap.swamp.len(),
        snap.live.comment_landed,
        snap.live.wiki_retracted
    );
    for line in &snap.briefing.lines {
        println!(
            "  {:>6.3}  {:>8}  [{:>4} tok]  {} — {}",
            line.score,
            line.kind.as_str(),
            line.tokens,
            line.id,
            line.title
        );
        for body_line in line.body.lines().take(3) {
            println!("           {body_line}");
        }
    }
}

fn print_snap(label: &str, snap: &Snapshot) {
    println!("== {label} ==");
    println!(
        "pack {} tok / {}  {} µs  comment={}  wiki_retracted={}",
        snap.briefing.tokens,
        snap.briefing.budget,
        snap.briefing.pack_us,
        snap.live.comment_landed,
        snap.live.wiki_retracted
    );
    let kinds = snap
        .kinds
        .iter()
        .map(|k| format!("{} {}", k.kind, k.n))
        .collect::<Vec<_>>()
        .join(" · ");
    println!("kinds: {kinds}");
    println!(
        "models: {}",
        snap.briefing
            .models
            .iter()
            .map(|model| format!(
                "{} {:.1} → {}",
                model.name,
                model.weight,
                model.top.as_deref().unwrap_or("no hit")
            ))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    if let Some(ghost) = &snap.ghost {
        println!("ghost: retracted: {} · {}", ghost.title, ghost.left);
    }
    for line in snap.briefing.lines.iter().take(5) {
        println!(
            "  {:>6.3}  {:>8}  {} — {}",
            line.score,
            line.kind.as_str(),
            line.id,
            line.title
        );
        println!("           {}", line.why);
    }
    println!();
}

fn script() {
    let potion = load_potion();
    let mut st = open_db!(potion);
    seed(&mut st);
    let q = DEFAULT_QUERY.to_string();
    let snap = snapshot!(st, potion, q.clone(), Goal::Balanced);
    assert!(
        snap.briefing
            .lines
            .iter()
            .any(|line| line.why.contains("Potion #")),
        "Potion must contribute to the briefing"
    );
    print_snap("seed", &snap);

    let comment = correcting_comment();
    st.wtx(|tx| {
        tx.upsert(&comment.id, &comment);
    });
    print_snap(
        "after human comment",
        &snapshot!(st, potion, q.clone(), Goal::Balanced),
    );

    st.wtx(|tx| {
        tx.remove(&STALE_WIKI_ID.to_string());
    });
    print_snap(
        "after retracting stale wiki",
        &snapshot!(st, potion, q, Goal::Balanced),
    );
}

fn serve() {
    let (tx, rx) = mpsc::channel::<Ingest>();
    let (state_tx, state_rx) = watch::channel(empty_snapshot());
    std::thread::spawn(move || ingest(rx, state_tx));
    serve_http(tx, state_rx);
}

fn ingest(rx: mpsc::Receiver<Ingest>, state_tx: watch::Sender<Snapshot>) {
    let potion = load_potion();
    let mut st = open_db!(potion);
    seed(&mut st);
    let mut query = DEFAULT_QUERY.to_string();
    let mut goal = Goal::Balanced;
    let _ = state_tx.send(snapshot!(st, potion, query.clone(), goal));

    for msg in rx {
        match msg {
            Ingest::Ask(q, next_goal) => {
                if !q.trim().is_empty() {
                    query = q;
                }
                if let Some(next_goal) = next_goal {
                    goal = next_goal;
                }
            }
            Ingest::Comment => {
                let chunk = correcting_comment();
                st.wtx(|tx| {
                    tx.upsert(&chunk.id, &chunk);
                });
            }
            Ingest::Retract => {
                st.wtx(|tx| {
                    tx.remove(&STALE_WIKI_ID.to_string());
                });
            }
            Ingest::Reset => {
                st.wtx(|tx| {
                    tx.remove(&COMMENT_ID.to_string());
                });
                let wiki = stale_wiki();
                st.wtx(|tx| {
                    tx.upsert(&wiki.id, &wiki);
                });
                query = DEFAULT_QUERY.to_string();
            }
        }
        let _ = state_tx.send(snapshot!(st, potion, query.clone(), goal));
    }
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        swamp: vec![],
        briefing: Briefing {
            query: DEFAULT_QUERY.to_string(),
            goal: Goal::Balanced,
            models: vec![],
            lines: vec![],
            tokens: 0,
            budget: TOKEN_BUDGET,
            pack_us: 0,
            seed: String::new(),
        },
        kinds: vec![],
        live: LiveFlags {
            comment_landed: false,
            wiki_retracted: false,
        },
        ghost: None,
    }
}

fn pack(
    query: &str,
    goal: Goal,
    keyword: &[Scored<f64, String>],
    semantic: &[Scored<f32, String>],
    code: &[Scored<f32, String>],
    chunks: &[Chunk],
) -> Briefing {
    let by_id: HashMap<&str, &Chunk> = chunks.iter().map(|c| (c.id.as_str(), c)).collect();
    let (ese_weight, potion_weight) = goal.weights();
    let models = vec![
        model_trace("bm25", "BM25", BM25_WEIGHT, keyword, &by_id),
        model_trace("ese", "ESE", ese_weight, semantic, &by_id),
        model_trace("potion", "Potion Code", potion_weight, code, &by_id),
    ];
    let mut fused: HashMap<String, f64> = HashMap::new();
    let mut bm25_at: HashMap<String, usize> = HashMap::new();
    let mut ese_at: HashMap<String, usize> = HashMap::new();
    let mut potion_at: HashMap<String, usize> = HashMap::new();
    for (rank, hit) in keyword.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += BM25_WEIGHT / (RRF_K + rank as f64 + 1.0);
        bm25_at.entry(hit.val.clone()).or_insert(rank);
    }
    for (rank, hit) in semantic.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += ese_weight / (RRF_K + rank as f64 + 1.0);
        ese_at.entry(hit.val.clone()).or_insert(rank);
    }
    for (rank, hit) in code.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += potion_weight / (RRF_K + rank as f64 + 1.0);
        potion_at.entry(hit.val.clone()).or_insert(rank);
    }

    let mut ranked: Vec<(String, f64, bool)> = fused
        .into_iter()
        .filter_map(|(id, score)| {
            let chunk = by_id.get(id.as_str())?;
            let mut score = score * chunk.kind.prior();
            let hay = chunk.search_text().to_ascii_lowercase();
            let phrase = hay.contains("rejected")
                || hay.contains("do not")
                || hay.contains("don't")
                || hay.contains("decided");
            if phrase {
                score *= 1.25;
            }
            Some((id, score, phrase))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut lines = Vec::new();
    let mut tokens = 0usize;
    for (id, score, phrase) in ranked {
        let Some(chunk) = by_id.get(id.as_str()) else {
            continue;
        };
        let t = chunk.tokens();
        if !lines.is_empty() && tokens + t > TOKEN_BUDGET {
            continue;
        }
        tokens += t;
        lines.push(PackedLine {
            id: chunk.id.clone(),
            kind: chunk.kind,
            task: chunk.task.clone(),
            phase: chunk.phase.clone(),
            title: chunk.title.clone(),
            body: chunk.body.clone(),
            score,
            tokens: t,
            why: why_stuck(
                chunk,
                bm25_at.get(&id).copied(),
                ese_at.get(&id).copied(),
                potion_at.get(&id).copied(),
                phrase,
            ),
        });
        if tokens >= TOKEN_BUDGET {
            break;
        }
    }

    let seed = seed_text(query, tokens, &lines);
    Briefing {
        query: query.to_string(),
        goal,
        models,
        lines,
        tokens,
        budget: TOKEN_BUDGET,
        pack_us: 0,
        seed,
    }
}

fn model_trace<T>(
    key: &'static str,
    name: &'static str,
    weight: f64,
    hits: &[Scored<T, String>],
    by_id: &HashMap<&str, &Chunk>,
) -> ModelTrace {
    let top = hits.first().and_then(|hit| {
        by_id.get(hit.val.as_str()).map(|chunk| {
            if chunk.task.is_empty() {
                chunk.title.clone()
            } else {
                format!("{} · {}", chunk.task, chunk.title)
            }
        })
    });
    ModelTrace {
        key,
        name,
        weight,
        top,
    }
}

fn why_stuck(
    chunk: &Chunk,
    bm25: Option<usize>,
    ese: Option<usize>,
    potion: Option<usize>,
    phrase: bool,
) -> String {
    let mut parts = Vec::new();
    if let Some(rank) = bm25 {
        parts.push(format!("BM25 #{}", rank + 1));
    }
    if let Some(rank) = ese {
        parts.push(format!("ESE #{}", rank + 1));
    }
    if let Some(rank) = potion {
        parts.push(format!("Potion #{}", rank + 1));
    }
    parts.push(format!(
        "{} ×{:.2}",
        chunk.kind.as_str(),
        chunk.kind.prior()
    ));
    if phrase {
        parts.push("phrase".to_string());
    }
    parts.join(" · ")
}

fn seed_text(query: &str, tokens: usize, lines: &[PackedLine]) -> String {
    let mut out =
        format!("# next-session seed\n# query: {query}\n# {tokens} / {TOKEN_BUDGET} tokens\n\n");
    for line in lines {
        out.push_str(&format!(
            "## {} · {}\n{}\n\n",
            line.kind.as_str(),
            line.title,
            line.body
        ));
    }
    out
}

fn chunk(id: &str, kind: Kind, task: &str, phase: &str, title: &str, body: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        kind,
        task: task.to_string(),
        phase: phase.to_string(),
        title: title.to_string(),
        body: body.to_string(),
    }
}

fn correcting_comment() -> Chunk {
    chunk(
        COMMENT_ID,
        Kind::Comment,
        "auth-refresh",
        "design",
        "Human comment on 03-design.md",
        "Rejected in-memory JWT refresh. Use Redis. Do not keep tokens in process. \
         The daemon can die; in-process state dies with it. Comments are the product loop — \
         the next session must read this before acting on the design.",
    )
}

fn stale_wiki() -> Chunk {
    chunk(
        STALE_WIKI_ID,
        Kind::Wiki,
        "",
        "",
        "session-lifecycle.md",
        "Accepted decision: store refresh tokens only in alineryd process memory. \
         There is no durable store for refresh tokens. If the daemon restarts, the \
         user pastes a refresh token again. Do not look elsewhere.",
    )
}

fn seed_chunks() -> Vec<Chunk> {
    vec![
        chunk(
            "auth-refresh/ticket",
            Kind::Ticket,
            "auth-refresh",
            "research",
            "Store session refresh tokens durably",
            "We need a place to keep refresh tokens so a coding-agent session can resume \
             after alineryd restarts. Constraints: local-only, no cloud, survive daemon death, \
             do not invent a second source of truth next to the filesystem.",
        ),
        chunk(
            "auth-refresh/artifact/00-ticket",
            Kind::Artifact,
            "auth-refresh",
            "research",
            "00-ticket.md",
            "Ticket: persist refresh tokens for harness resume. Investigate in-memory HashMap \
             on the daemon, a SQLite row, Redis, or a file next to the session meta.",
        ),
        chunk(
            "auth-refresh/artifact/01-research",
            Kind::Artifact,
            "auth-refresh",
            "research",
            "01-research.md",
            "Options considered: (1) in-process HashMap on alineryd — fast, dies with the process. \
             (2) SQLite — HumanLayer does this; we set a SQL layer aside for v1. \
             (3) Redis — out of process, survives the daemon, still local if we run it ourselves. \
             (4) a file beside sessions/<id>.meta.json — closest to filesystem-as-database.",
        ),
        chunk(
            "auth-refresh/artifact/03-design",
            Kind::Artifact,
            "auth-refresh",
            "design",
            "03-design.md",
            "Design: store refresh tokens as in-memory JWTs — a HashMap on alineryd keyed \
             by session id. No extra service. Resume reads that map. This is how we store \
             refresh tokens this week.",
        ),
        stale_wiki(),
        chunk(
            "wiki/wiki-and-distill",
            Kind::Wiki,
            "",
            "",
            "wiki-and-distill.md",
            "Distill-to-wiki is proposal-first. The harness writes potential-wiki-changes-NNN.md \
             and must not edit docs/wiki/ until the human uses Commit wiki changes. Wiki pages \
             hold durable why: decisions, rejected alternatives, local conventions.",
        ),
        chunk(
            "wiki/sandboxing",
            Kind::Wiki,
            "",
            "",
            "sandboxing.md",
            "Alinery can target a local sandbox. Network stays off by default. Do not confuse \
             sandbox policy with session resume or credential handling.",
        ),
        chunk(
            "daemon-locks/ticket",
            Kind::Ticket,
            "daemon-locks",
            "tdd",
            "Two windows must not share a repo",
            "GUI ownership lock at .alinery/.alinery-app.lock is un-namespaced so dev and \
             production cannot mutate the same working tree.",
        ),
        chunk(
            "daemon-locks/artifact/03-design",
            Kind::Artifact,
            "daemon-locks",
            "design",
            "03-design.md",
            "Use flock, not a pid file. require_repo_owned claims before any FS mutation. \
             A foreign window sees repo_busy, never a takeover button.",
        ),
        chunk(
            "daemon-locks/comment/no-pid",
            Kind::Comment,
            "daemon-locks",
            "design",
            "Human comment on 03-design.md",
            "Decided: product ownership is flock-only. Do not add a pid file or owner marker. \
             We already burned a week on stale pid files.",
        ),
        chunk(
            "wiki/filesystem",
            Kind::Wiki,
            "",
            "",
            "index.md",
            "The board is a directory. Task cards, artifacts, comments, and wiki pages \
             live under <repo>/.alinery/. Agents and MCP open them by path. A later \
             query index is allowed only when listing the board is measurably slow. \
             Do not hide the files behind a SQL layer.",
        ),
        chunk(
            "phase-complete/artifact/04-tdd",
            Kind::Artifact,
            "phase-complete",
            "tdd",
            "04-tdd.md",
            "Phase completion is explicit. Only an authenticated OMP alinery_phase_complete \
             event plus a non-empty artifact can checkpoint. Artifact age is never completion.",
        ),
        chunk(
            "phase-complete/comment/no-infer",
            Kind::Comment,
            "phase-complete",
            "tdd",
            "Human comment on 04-tdd.md",
            "Rejected inferring completion from PTY text. Do not treat a quiet terminal as done.",
        ),
    ]
}

#[tokio::main]
async fn serve_http(tx: mpsc::Sender<Ingest>, state_rx: watch::Receiver<Snapshot>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/icon.png", get(icon_png))
        .route("/favicon.png", get(favicon_png))
        .route("/ws", get(ws_upgrade))
        .with_state((tx, state_rx));

    let port: u16 = std::env::var("SUNDEW_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    println!("sundew  http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

type AppState = (mpsc::Sender<Ingest>, watch::Receiver<Snapshot>);

async fn ws_upgrade(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, (tx, mut state_rx): AppState) {
    let encode = |s: &Snapshot| serde_json::to_string(s).unwrap();
    let hello = encode(&state_rx.borrow_and_update());
    if socket.send(Message::text(hello)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = state_rx.changed() => {
                if changed.is_err() {
                    return;
                }
                let update = encode(&state_rx.borrow_and_update());
                if socket.send(Message::text(update)).await.is_err() {
                    return;
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(Message::Text(line))) = incoming else {
                    return;
                };
                let Ok(msg) = serde_json::from_str::<ClientMsg>(&line) else {
                    continue;
                };
                let cmd = match msg.op.as_str() {
                    "ask" => Ingest::Ask(msg.q, msg.goal),
                    "comment" => Ingest::Comment,
                    "retract" => Ingest::Retract,
                    "reset" => Ingest::Reset,
                    _ => continue,
                };
                if tx.send(cmd).is_err() {
                    return;
                }
            }
        }
    }
}

async fn index() -> Html<&'static str> {
    Html(include_str!("ui.html"))
}

fn png(bytes: &'static [u8]) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
}

async fn icon_png() -> impl IntoResponse {
    png(include_bytes!("icon.png"))
}

async fn favicon_png() -> impl IntoResponse {
    png(include_bytes!("favicon.png"))
}
