//! fen: a fuller-featured bog chat. Same shape as `examples/chat` — one
//! plain thread owns the fold stream, websocket clients feed it commands
//! and receive snapshots — but the stream is a `KeyedStream`, and the
//! pipeline fans every message out to search indexes as it arrives:
//!
//!   ws clients -> mpsc<Cmd> -> ingest thread (owns KeyedStream) -> watch -> ws clients
//!
//! Each `Keyed { key: id, val: ChatMsg }` delta materializes four views:
//!
//!   - a message table (id -> ChatMsg), the log clients render
//!   - a BM25 full-text index over message bodies
//!   - an HNSW vector index over ese embeddings of the bodies
//!   - an incrementally-maintained count per author
//!
//! Search is hybrid: BM25 and HNSW hit lists fused by reciprocal rank.
//! Because the stream is keyed, deleting a message retracts it from every
//! view atomically — the log, both indexes (the HNSW node is genuinely
//! removed, not tombstoned), and the author counts.
//!
//! Unlike the chat example, the database persists across restarts.
//! `cargo run -p fen`, then open http://localhost:3001 (phone-friendly).

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anny::metric::Cosine;
use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Html,
    routing::get,
};
use fold::pipeline::{Aggregate, KeyBy, Keyed, Map, Scored, terminal};
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

const DIM: usize = ese::DIMENSIONS;

/// Reciprocal-rank-fusion constant (from the original RRF paper).
const RRF_K: f64 = 60.0;

/// Semantic hits farther than this cosine distance are noise, not recall.
const SEM_CUTOFF: f32 = 0.8;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMsg {
    id: u64,
    at_ms: u64,
    author: String,
    body: String,
}

/// What every client renders: the full log plus per-author message counts.
#[derive(Debug, Clone, Default, Serialize)]
struct ChatState {
    messages: Vec<ChatMsg>,
    author_counts: Vec<(String, i64)>,
}

/// One hybrid search hit, annotated with which index(es) surfaced it.
#[derive(Debug, Clone, Serialize)]
struct Hit {
    score: f64,
    keyword: bool,
    semantic: bool,
    #[serde(flatten)]
    msg: ChatMsg,
}

/// Everything a client can ask the ingest thread to do.
enum Cmd {
    Post { author: String, body: String },
    Delete { id: u64 },
    Search { query: String, reply: oneshot::Sender<Vec<Hit>> },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Post { author: String, body: String },
    Delete { id: u64 },
    Search { query: String },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg<'a> {
    State(&'a ChatState),
    Results { query: String, hits: Vec<Hit> },
}

/// Read one consistent snapshot into a `ChatState`. A macro because the
/// pipeline type contains closures and can't be written out.
macro_rules! snapshot {
    ($st:expr) => {
        $st.rtx(|(messages, _, _, author_counts)| {
            let mut messages: Vec<ChatMsg> = messages.iter().map(|(_, m)| m).collect();
            messages.sort_by_key(|m| m.id);

            let mut author_counts: Vec<(String, i64)> = author_counts.iter().collect();
            author_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

            ChatState {
                messages,
                author_counts,
            }
        })
    };
}

fn main() {
    // persistent on purpose: restart the server and the fen remembers.
    // FEN_DB picks the location (default lives in the OS temp dir, which
    // on many linuxes is wiped at boot — set FEN_DB for real deployments)
    let db_path = std::env::var_os("FEN_DB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("bog-kit-fen.db"));

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (state_tx, state_rx) = watch::channel(ChatState::default());
    std::thread::spawn(move || ingest(&db_path, cmd_rx, state_tx));

    serve(cmd_tx, state_rx);
}

/// Owns the fold stream: applies commands, republishes snapshots, answers
/// searches — all against one consistent view of the data.
fn ingest(db_path: &std::path::Path, rx: mpsc::Receiver<Cmd>, state_tx: watch::Sender<ChatState>) {
    let mut st = KeyedStream::new(
        db_path,
        (
            // the log: id -> message, for rendering and id allocation
            terminal::Table::<u64, ChatMsg>::new("messages"),
            // keyword: bodies tokenized into a BM25 index
            Map::new(
                |d: &Keyed<u64, ChatMsg>| Keyed::new(d.key, d.val.body.clone()),
                terminal::search::Bm25::new("bm25"),
            ),
            // semantic: ese embeds the body right here in the pipeline, so
            // one upsert (re)indexes everywhere and one remove retracts
            // everywhere — including deleting the node from the HNSW graph
            Map::new(
                |d: &Keyed<u64, ChatMsg>| Keyed::new(d.key, ese::encode_single(&d.val.body)),
                terminal::search::Hnsw::<u64, f32, Cosine, DIM>::new("vecs", Cosine, 42),
            ),
            // fold flavor: message count per author, maintained incrementally
            KeyBy::new(
                |d: &Keyed<u64, ChatMsg>| d.val.author.clone(),
                Aggregate::new(
                    "by_author",
                    |acc: &mut i64, _d: &Keyed<u64, ChatMsg>, delta| *acc += delta as i64,
                    terminal::Table::new("author_counts"),
                ),
            ),
        ),
    );

    let mut next_id = st.rtx(|(messages, _, _, _)| {
        messages.iter().map(|(id, _)| id).max().map_or(0, |m| m + 1)
    });

    // a few residents so search has something to find on first open
    if next_id == 0 {
        let seeds = seed_messages();
        let (now, n) = (now_ms(), seeds.len() as u64);
        st.wtx(|tx| {
            for (i, (author, body)) in seeds.into_iter().enumerate() {
                let msg = ChatMsg {
                    id: i as u64,
                    at_ms: now - (n - i as u64) * 60_000,
                    author: author.to_string(),
                    body: body.to_string(),
                };
                tx.upsert(&msg.id, &msg);
            }
        });
        next_id = n;
    }

    let _ = state_tx.send(snapshot!(st));

    for cmd in rx {
        match cmd {
            Cmd::Post { author, body } => {
                let msg = ChatMsg {
                    id: next_id,
                    at_ms: now_ms(),
                    author,
                    body,
                };
                next_id += 1;
                st.wtx(|tx| tx.upsert(&msg.id, &msg));
                let _ = state_tx.send(snapshot!(st));
            }
            Cmd::Delete { id } => {
                // one keyed removal retracts the message from the log, both
                // search indexes, and its author's count, atomically
                if st.wtx(|tx| tx.remove(&id)).is_some() {
                    let _ = state_tx.send(snapshot!(st));
                }
            }
            Cmd::Search { query, reply } => {
                let hits = st.rtx(|(messages, bm25, vecs, _)| {
                    let keyword = bm25.search(&query, 20);
                    let semantic: Vec<Scored<f32, u64>> = vecs
                        .search(&ese::encode_single(&query))
                        .into_iter()
                        .filter(|h| h.score < SEM_CUTOFF)
                        .collect();
                    hybrid(&keyword, &semantic)
                        .into_iter()
                        .filter_map(|(id, score)| {
                            Some(Hit {
                                score,
                                keyword: keyword.iter().any(|h| h.val == id),
                                semantic: semantic.iter().any(|h| h.val == id),
                                msg: messages.get(&id)?,
                            })
                        })
                        .collect()
                });
                let _ = reply.send(hits);
            }
        }
    }
}

/// Fuse a BM25 hit list and an HNSW hit list by reciprocal rank. Rank-based
/// fusion sidesteps the fact that the two scores live on incomparable
/// scales (BM25 relevance vs cosine distance).
fn hybrid(keyword: &[Scored<f64, u64>], semantic: &[Scored<f32, u64>]) -> Vec<(u64, f64)> {
    let mut fused: HashMap<u64, f64> = HashMap::new();
    for (rank, hit) in keyword.iter().enumerate() {
        *fused.entry(hit.val).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    for (rank, hit) in semantic.iter().enumerate() {
        *fused.entry(hit.val).or_default() += 1.0 / (RRF_K + rank as f64 + 1.0);
    }
    let mut fused: Vec<(u64, f64)> = fused.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1));
    fused.truncate(12);
    fused
}

#[tokio::main]
async fn serve(cmd_tx: mpsc::Sender<Cmd>, state_rx: watch::Receiver<ChatState>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state((cmd_tx, state_rx));

    let port: u16 = std::env::var("FEN_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3001);
    let addr = format!("0.0.0.0:{port}");
    println!("fen running on http://localhost:{port} (websocket at /ws)");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

type AppState = (mpsc::Sender<Cmd>, watch::Receiver<ChatState>);

async fn ws_upgrade(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Per-client task: push every new snapshot down, translate incoming JSON
/// into commands for the ingest thread. Searches round-trip through a
/// oneshot so results come from the same thread that owns the data.
async fn handle_socket(mut socket: WebSocket, (cmd_tx, mut state_rx): AppState) {
    let json = |m: &ServerMsg| serde_json::to_string(m).unwrap();

    // greet with current history
    let hello = json(&ServerMsg::State(&state_rx.borrow_and_update()));
    if socket.send(Message::text(hello)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = state_rx.changed() => {
                if changed.is_err() {
                    return; // ingest thread gone
                }
                let update = json(&ServerMsg::State(&state_rx.borrow_and_update()));
                if socket.send(Message::text(update)).await.is_err() {
                    return;
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(Message::Text(raw))) = incoming else {
                    return; // client closed or errored
                };
                let Ok(msg) = serde_json::from_str::<ClientMsg>(&raw) else {
                    continue;
                };
                match msg {
                    ClientMsg::Post { author, body } => {
                        let author = clip(author.trim(), 24, "anon");
                        let body = clip(body.trim(), 1000, "");
                        if !body.is_empty() && cmd_tx.send(Cmd::Post { author, body }).is_err() {
                            return;
                        }
                    }
                    ClientMsg::Delete { id } => {
                        if cmd_tx.send(Cmd::Delete { id }).is_err() {
                            return;
                        }
                    }
                    ClientMsg::Search { query } => {
                        let (reply, rx) = oneshot::channel();
                        if cmd_tx.send(Cmd::Search { query: query.clone(), reply }).is_err() {
                            return;
                        }
                        let Ok(hits) = rx.await else { return };
                        let frame = json(&ServerMsg::Results { query, hits });
                        if socket.send(Message::text(frame)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn clip(s: &str, max: usize, fallback: &str) -> String {
    let s = if s.is_empty() { fallback } else { s };
    s.chars().take(max).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn seed_messages() -> Vec<(&'static str, &'static str)> {
    vec![
        ("sedge", "morning all, the fen is quiet today"),
        ("tannin", "deployed the new boardwalk over the east channel on friday"),
        ("marsh", "water table dropped two inches, the sphagnum is not thrilled"),
        ("sedge", "reminder: peat preserves everything, watch what you say in here"),
        ("alder", "found the missing sensor. a heron had opinions about it"),
        ("tannin", "search should rank recent sightings higher imo"),
        ("marsh", "the midges are back. so it begins"),
        ("alder", "swapped the redis cache for an in-process lru, latency halved"),
    ]
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="theme-color" content="#0b100d">
<title>fen</title>
<style>
  :root {
    --bg: #0b100d;
    --panel: #10160f;
    --line: #1e2a1e;
    --ink: #d7e2d0;
    --dim: #6d7f6d;
    --faint: #46543f;
    --moss: #a3c76d;
    --amber: #d9a441;
    --teal: #6fbfa8;
    --danger: #c76d6d;
    --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }
  * { box-sizing: border-box; margin: 0; }
  html, body { height: 100%; }
  body {
    background:
      radial-gradient(1200px 500px at 50% -10%, #12200f66, transparent),
      var(--bg);
    color: var(--ink);
    font-family: var(--mono);
    display: flex;
    flex-direction: column;
    height: 100dvh;
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: baseline;
    gap: 0.75rem;
    padding: 0.9rem 1rem 0.5rem;
  }
  header h1 { font-size: 1.05rem; font-weight: 600; color: var(--moss); letter-spacing: 0.06em; }
  header h1::before { content: "~ "; color: var(--faint); }
  header h1::after  { content: " ~"; color: var(--faint); }
  header .sub { font-size: 0.72rem; color: var(--dim); }
  header .count { margin-left: auto; font-size: 0.72rem; color: var(--faint); white-space: nowrap; }

  .searchbar { padding: 0 1rem 0.5rem; position: relative; }
  .searchbar input {
    width: 100%;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    color: var(--ink);
    font: inherit;
    font-size: 16px;
    padding: 0.55rem 2.2rem 0.55rem 0.75rem;
    outline: none;
  }
  .searchbar input:focus { border-color: #33452c; }
  .searchbar input::placeholder { color: var(--faint); }
  .searchbar .clear {
    position: absolute; right: 1.45rem; top: 50%;
    transform: translateY(-56%);
    background: none; border: none; color: var(--dim);
    font: inherit; font-size: 1.1rem; cursor: pointer;
    padding: 0.2rem 0.4rem; display: none;
  }
  .searchbar.active .clear { display: block; }

  .chips {
    display: flex; gap: 0.4rem;
    padding: 0 1rem 0.55rem;
    overflow-x: auto;
    scrollbar-width: none;
    border-bottom: 1px solid var(--line);
    flex: none;
  }
  .chips::-webkit-scrollbar { display: none; }
  .chip {
    font-size: 0.7rem; color: var(--dim);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 0.15rem 0.6rem;
    white-space: nowrap;
    background: var(--panel);
  }
  .chip b { color: var(--moss); font-weight: 600; }

  main { flex: 1; overflow-y: auto; padding: 0.6rem 1rem; overscroll-behavior: contain; }

  .msg { padding: 0.45rem 0.5rem; border-radius: 8px; position: relative; }
  .msg + .msg { margin-top: 0.15rem; }
  .msg .meta { display: flex; align-items: baseline; gap: 0.55rem; }
  .msg .author { font-size: 0.78rem; font-weight: 600; }
  .msg .time { font-size: 0.66rem; color: var(--faint); }
  .msg .body { font-size: 0.88rem; line-height: 1.45; overflow-wrap: anywhere; color: var(--ink); }
  .msg .del {
    margin-left: auto;
    background: none; border: none; cursor: pointer;
    color: var(--faint); font: inherit; font-size: 0.8rem;
    padding: 0 0.3rem; opacity: 0.4;
  }
  .msg .del:hover, .msg .del:active { color: var(--danger); opacity: 1; }
  .msg.mine { background: #131c1244; }
  .msg.flash { animation: flash 1.6s ease-out; }
  @keyframes flash { 0% { background: #2c3e1e; } 100% { background: transparent; } }

  .results { display: none; }
  body.searching main .log { display: none; }
  body.searching main .results { display: block; }
  .results .hint { font-size: 0.72rem; color: var(--dim); padding: 0.3rem 0.5rem 0.6rem; }
  .hit {
    display: block; width: 100%; text-align: left;
    background: var(--panel); border: 1px solid var(--line);
    border-radius: 8px; padding: 0.55rem 0.65rem;
    margin-bottom: 0.45rem; cursor: pointer; font: inherit; color: var(--ink);
  }
  .hit:active { border-color: #33452c; }
  .hit .meta { display: flex; align-items: baseline; gap: 0.55rem; }
  .hit .author { font-size: 0.75rem; font-weight: 600; }
  .hit .body { font-size: 0.85rem; line-height: 1.4; overflow-wrap: anywhere; margin-top: 0.15rem; }
  .hit .tags { margin-left: auto; display: flex; gap: 0.3rem; }
  .tag { font-size: 0.6rem; padding: 0.05rem 0.35rem; border-radius: 4px; border: 1px solid; }
  .tag.kw  { color: var(--moss);  border-color: #33452c; }
  .tag.sem { color: var(--amber); border-color: #4a3c1e; }

  form {
    display: flex; gap: 0.5rem;
    padding: 0.6rem 1rem calc(0.6rem + env(safe-area-inset-bottom));
    border-top: 1px solid var(--line);
    background: var(--bg);
    flex: none;
  }
  form input {
    background: var(--panel); border: 1px solid var(--line);
    border-radius: 8px; color: var(--ink);
    font: inherit; font-size: 16px;
    padding: 0.55rem 0.7rem; outline: none; min-width: 0;
  }
  form input:focus { border-color: #33452c; }
  form input::placeholder { color: var(--faint); }
  #name { width: 6.5rem; flex: none; }
  #text { flex: 1; }
  form button {
    background: #1c2b16; color: var(--moss);
    border: 1px solid #33452c; border-radius: 8px;
    font: inherit; font-size: 0.85rem;
    padding: 0.55rem 0.9rem; cursor: pointer; flex: none;
  }
  form button:active { background: #24371c; }
</style>
</head>
<body>
<header>
  <h1>fen</h1>
  <span class="sub">a bog chat</span>
  <span class="count" id="count"></span>
</header>

<div class="searchbar" id="searchbar">
  <input id="search" type="search" placeholder="search the fen&hellip;"
         autocomplete="off" autocorrect="off" autocapitalize="off">
  <button class="clear" id="clear" aria-label="clear search">&times;</button>
</div>

<div class="chips" id="chips"></div>

<main id="main">
  <div class="log" id="log"></div>
  <div class="results" id="results"></div>
</main>

<form id="composer">
  <input id="name" placeholder="name" maxlength="24" autocomplete="off">
  <input id="text" placeholder="say something" maxlength="1000" autocomplete="off">
  <button type="submit">send</button>
</form>

<script>
const $ = (id) => document.getElementById(id);
const HUES = ["#a3c76d", "#6fbfa8", "#d9a441", "#c79ad4", "#7da8d9", "#d98f6f"];
const hue = (name) => {
  let h = 0;
  for (const c of name) h = (h * 31 + c.codePointAt(0)) >>> 0;
  return HUES[h % HUES.length];
};
const fmtTime = (ms) =>
  new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

let state = { messages: [], author_counts: [] };
let query = "";
let ws;

$("name").value = localStorage.getItem("fen-name") || "";

function connect() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/ws`);
  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data);
    if (msg.type === "state") {
      state = msg;
      renderState();
      if (query) sendSearch(); // keep open results fresh as the log changes
    } else if (msg.type === "results" && msg.query === query) {
      renderResults(msg.hits);
    }
  };
  ws.onclose = () => setTimeout(connect, 1000);
}
const send = (obj) => { if (ws.readyState === 1) ws.send(JSON.stringify(obj)); };

function renderState() {
  $("count").textContent = `${state.messages.length} in the peat`;

  $("chips").replaceChildren(...state.author_counts.map(([author, n]) => {
    const span = document.createElement("span");
    span.className = "chip";
    const b = document.createElement("b");
    b.textContent = author;
    b.style.color = hue(author);
    span.append(b, ` ${n}`);
    return span;
  }));

  const main = $("main");
  const pinned = main.scrollHeight - main.scrollTop - main.clientHeight < 60;
  const me = $("name").value.trim();

  $("log").replaceChildren(...state.messages.map((m) => {
    const div = document.createElement("div");
    div.className = "msg" + (m.author === me && me ? " mine" : "");
    div.id = `m${m.id}`;

    const meta = document.createElement("div");
    meta.className = "meta";
    const author = document.createElement("span");
    author.className = "author";
    author.textContent = m.author;
    author.style.color = hue(m.author);
    const time = document.createElement("span");
    time.className = "time";
    time.textContent = fmtTime(m.at_ms);
    const del = document.createElement("button");
    del.className = "del";
    del.textContent = "\u00d7";
    del.title = "sink into the bog";
    del.onclick = () => send({ type: "delete", id: m.id });
    meta.append(author, time, del);

    const body = document.createElement("div");
    body.className = "body";
    body.textContent = m.body;

    div.append(meta, body);
    return div;
  }));

  if (pinned && !query) main.scrollTop = main.scrollHeight;
}

function renderResults(hits) {
  const results = $("results");
  const hint = document.createElement("div");
  hint.className = "hint";
  hint.textContent = hits.length
    ? `${hits.length} dredged up \u00b7 kw = keyword match, sem = semantic neighbor`
    : "nothing surfaced. the bog keeps its secrets";

  results.replaceChildren(hint, ...hits.map((h) => {
    const btn = document.createElement("button");
    btn.className = "hit";
    btn.onclick = () => reveal(h.id);

    const meta = document.createElement("div");
    meta.className = "meta";
    const author = document.createElement("span");
    author.className = "author";
    author.textContent = h.author;
    author.style.color = hue(h.author);
    const time = document.createElement("span");
    time.className = "time";
    time.textContent = fmtTime(h.at_ms);
    const tags = document.createElement("span");
    tags.className = "tags";
    if (h.keyword) tags.append(tag("kw"));
    if (h.semantic) tags.append(tag("sem"));
    meta.append(author, time, tags);

    const body = document.createElement("div");
    body.className = "body";
    body.textContent = h.body;

    btn.append(meta, body);
    return btn;
  }));
  $("main").scrollTop = 0;
}

function tag(kind) {
  const span = document.createElement("span");
  span.className = `tag ${kind}`;
  span.textContent = kind;
  return span;
}

// jump from a search hit back to its place in the log
function reveal(id) {
  setQuery("");
  const el = $(`m${id}`);
  if (!el) return;
  el.scrollIntoView({ block: "center" });
  el.classList.add("flash");
  setTimeout(() => el.classList.remove("flash"), 1600);
}

let debounce;
function setQuery(q) {
  query = q;
  $("search").value = q;
  $("searchbar").classList.toggle("active", !!q);
  document.body.classList.toggle("searching", !!q);
  if (q) sendSearch();
}
const sendSearch = () => send({ type: "search", query });

$("search").addEventListener("input", (e) => {
  clearTimeout(debounce);
  const q = e.target.value.trim();
  debounce = setTimeout(() => setQuery(q), 150);
});
$("clear").onclick = () => { setQuery(""); $("search").focus(); };
$("search").addEventListener("keydown", (e) => {
  if (e.key === "Escape") setQuery("");
});

$("composer").addEventListener("submit", (e) => {
  e.preventDefault();
  const text = $("text");
  const body = text.value.trim();
  if (!body) return;
  const author = $("name").value.trim() || "anon";
  localStorage.setItem("fen-name", author);
  send({ type: "post", author, body });
  text.value = "";
  setQuery("");
});

connect();
</script>
</body>
</html>"##;
