//! Sundew: a live, retractable briefing for the next coding agent.
//!
//! Alinery's filesystem is the database. This crate is the query engine
//! that filesystem never had: one KeyedStream of Alinery-shaped chunks
//! (ticket, artifact, comment, wiki) fans out to BM25, ESE+HNSW, and a
//! doc table. The product is not another search box — it is a packed,
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
use std::sync::mpsc;

use anny::metric::Cosine;
use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Html,
    routing::get,
};
use fold::pipeline::{Keyed, Map, Scored, terminal};
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

const DIM: usize = ese::DIMENSIONS;
const RRF_K: f64 = 60.0;
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
}

#[derive(Debug, Clone, Serialize)]
struct Briefing {
    query: String,
    lines: Vec<PackedLine>,
    tokens: usize,
    budget: usize,
    pack_us: u64,
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
struct Snapshot {
    swamp: Vec<SwampItem>,
    briefing: Briefing,
    kinds: Vec<KindCount>,
    live: LiveFlags,
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
}

enum Ingest {
    Ask(String),
    Comment,
    Retract,
    Reset,
}

macro_rules! snapshot {
    ($st:expr, $query:expr) => {{
        let query: String = $query;
        $st.rtx(|(bm25, vecs, docs)| {
            let mut chunks: Vec<Chunk> = docs.iter().map(|(_, c)| c).collect();
            chunks.sort_by(|a, b| a.id.cmp(&b.id));

            let started = std::time::Instant::now();
            let keyword = bm25.search(&query, 10);
            let semantic = vecs.search(&ese::encode_single(&query));
            let briefing = pack(&query, &keyword, &semantic, &chunks);
            let pack_us = started.elapsed().as_micros() as u64;
            let briefing = Briefing {
                pack_us,
                ..briefing
            };

            let mut counts: HashMap<&str, usize> = HashMap::new();
            for c in &chunks {
                *counts.entry(c.kind.as_str()).or_default() += 1;
            }
            let mut kinds: Vec<KindCount> = counts
                .into_iter()
                .map(|(kind, n)| KindCount {
                    kind: kind.to_string(),
                    n,
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
            Snapshot {
                swamp,
                briefing,
                kinds,
                live: LiveFlags {
                    comment_landed: ids.contains(&COMMENT_ID),
                    wiki_retracted: !ids.contains(&STALE_WIKI_ID),
                },
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
    () => {{
        let db_path = std::env::temp_dir().join("bog-kit-sundew.db");
        let _ = std::fs::remove_dir_all(&db_path);
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
                terminal::Table::new("docs"),
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
    let mut st = open_db!();
    seed(&mut st);
    let snap = snapshot!(st, query.to_string());
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
    for line in snap.briefing.lines.iter().take(5) {
        println!(
            "  {:>6.3}  {:>8}  {}",
            line.score,
            line.kind.as_str(),
            line.title
        );
    }
    println!();
}

fn script() {
    let mut st = open_db!();
    seed(&mut st);
    let q = DEFAULT_QUERY.to_string();
    print_snap("seed", &snapshot!(st, q.clone()));

    let comment = correcting_comment();
    st.wtx(|tx| {
        tx.upsert(&comment.id, &comment);
    });
    print_snap("after human comment", &snapshot!(st, q.clone()));

    st.wtx(|tx| {
        tx.remove(&STALE_WIKI_ID.to_string());
    });
    print_snap("after retracting stale wiki", &snapshot!(st, q));
}

fn serve() {
    let (tx, rx) = mpsc::channel::<Ingest>();
    let (state_tx, state_rx) = watch::channel(empty_snapshot());
    std::thread::spawn(move || ingest(rx, state_tx));
    serve_http(tx, state_rx);
}

fn ingest(rx: mpsc::Receiver<Ingest>, state_tx: watch::Sender<Snapshot>) {
    let mut st = open_db!();
    seed(&mut st);
    let mut query = DEFAULT_QUERY.to_string();
    let _ = state_tx.send(snapshot!(st, query.clone()));

    for msg in rx {
        match msg {
            Ingest::Ask(q) => {
                if !q.trim().is_empty() {
                    query = q;
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
        let _ = state_tx.send(snapshot!(st, query.clone()));
    }
}

fn empty_snapshot() -> Snapshot {
    Snapshot {
        swamp: vec![],
        briefing: Briefing {
            query: DEFAULT_QUERY.to_string(),
            lines: vec![],
            tokens: 0,
            budget: TOKEN_BUDGET,
            pack_us: 0,
        },
        kinds: vec![],
        live: LiveFlags {
            comment_landed: false,
            wiki_retracted: false,
        },
    }
}

fn pack(
    query: &str,
    keyword: &[Scored<f64, String>],
    semantic: &[Scored<f32, String>],
    chunks: &[Chunk],
) -> Briefing {
    let by_id: HashMap<&str, &Chunk> = chunks.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut fused: HashMap<String, f64> = HashMap::new();
    for (rank, hit) in keyword.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    for (rank, hit) in semantic.iter().enumerate() {
        *fused.entry(hit.val.clone()).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }

    let mut ranked: Vec<(String, f64)> = fused
        .into_iter()
        .filter_map(|(id, score)| {
            let chunk = by_id.get(id.as_str())?;
            let mut score = score * chunk.kind.prior();
            let hay = chunk.search_text().to_ascii_lowercase();
            if hay.contains("rejected")
                || hay.contains("do not")
                || hay.contains("don't")
                || hay.contains("decided")
            {
                score *= 1.25;
            }
            Some((id, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut lines = Vec::new();
    let mut tokens = 0usize;
    for (id, score) in ranked {
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
        });
        if tokens >= TOKEN_BUDGET {
            break;
        }
    }

    Briefing {
        query: query.to_string(),
        lines,
        tokens,
        budget: TOKEN_BUDGET,
        pack_us: 0,
    }
}

fn chunk(
    id: &str,
    kind: Kind,
    task: &str,
    phase: &str,
    title: &str,
    body: &str,
) -> Chunk {
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
        "Resume tokens for refresh live in alineryd memory only. There is no durable \
         store. If the daemon restarts, the user pastes a token again. This is the \
         accepted decision for how we store refresh tokens.",
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
             (2) SQLite — HumanLayer does this; we rejected a SQL store for v1. \
             (3) Redis — out of process, survives the daemon, still local if we run it ourselves. \
             (4) a file beside sessions/<id>.meta.json — closest to filesystem-as-database.",
        ),
        chunk(
            "auth-refresh/artifact/03-design",
            Kind::Artifact,
            "auth-refresh",
            "design",
            "03-design.md",
            "Design: keep refresh tokens in process memory as a HashMap on alineryd keyed by \
             session id. No extra service. Resume reads the map. This is simple and we can \
             ship it this week.",
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
             sandbox policy with session resume or token storage.",
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
            "The filesystem is the database. Repo data lives under <repo>/.alinery/. \
             Add a query index only when a directory scan of the board is measurably slow. \
             Agents and MCP read files by path; do not hide the files behind a SQL store.",
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
                    "ask" => Ingest::Ask(msg.q),
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
