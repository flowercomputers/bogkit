//! Pattern drafting on fold: measurement profiles are the deltas, drafted
//! patterns are the materialized view.
//!
//! The pipeline stores every profile in a bag and, on the same write, maps
//! it through the trouser draft into a table of rendered SVGs — the "do the
//! work at write time" trick: by the time anyone reads, the pattern is
//! already drawn. The web side is the chat example's shape:
//!
//!   ws clients -> mpsc -> ingest thread (owns the fold Stream) -> watch -> ws clients

mod garment;
mod garments;
mod geometry;
mod measurements;
mod svg;

use std::collections::HashMap;
use std::sync::mpsc;

use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Html,
    routing::get,
};
use fold::pipeline::{Keyed, Map, terminal};
use fold::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use garment::Garment;
use garments::trouser::Trouser;
use measurements::{Measurements, Profile};
use svg::render_svg;

/// Draft a profile end to end; used by the pipeline's map node, so it must
/// be deterministic — retraction re-drafts the profile and must cancel.
fn draft_svg(p: &Profile) -> String {
    render_svg(&Trouser.draft(&p.m))
}

/// What every client sees: all profiles plus the pre-drafted SVG per
/// profile, read from one fold snapshot.
#[derive(Debug, Clone, Default, Serialize)]
struct Snapshot {
    profiles: Vec<Profile>,
    /// `[profile id, svg document]` pairs.
    drafted: Vec<(u64, String)>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Cmd {
    Upsert { profile: Profile },
    Delete { id: u64 },
}

macro_rules! snapshot {
    ($st:expr) => {
        $st.rtx(|(profiles, drafted)| {
            let mut profiles: Vec<Profile> = profiles.iter().map(|(p, _)| p).collect();
            profiles.sort_by_key(|p| p.id);
            Snapshot {
                profiles,
                drafted: drafted.iter().collect(),
            }
        })
    };
}

fn main() {
    // patterns persist across restarts; delete this dir for a fresh start
    let db_path = std::env::temp_dir().join("bog-kit-patterns.db");

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (state_tx, state_rx) = watch::channel(Snapshot::default());
    std::thread::spawn(move || ingest(&db_path, cmd_rx, state_tx));

    serve(cmd_tx, state_rx);
}

/// Owns the fold stream: applies profile upserts/deletes, republishes
/// snapshots. Keeps the live profiles in memory so an upsert knows exactly
/// what to retract.
fn ingest(
    db_path: &std::path::Path,
    rx: mpsc::Receiver<Cmd>,
    state_tx: watch::Sender<Snapshot>,
) {
    let mut st = Stream::new(
        db_path,
        (
            // the durable profile store
            terminal::Bag::<Profile>::new("profiles"),
            // the materialized view: profile id -> rendered pattern.
            // Table is last-writer-wins per key, so retract-then-insert
            // within one transaction lands on the fresh draft.
            Map::new(
                |p: &Profile| Keyed::new(p.id, draft_svg(p)),
                terminal::Table::<u64, String>::new("drafted"),
            ),
        ),
    );

    let mut live: HashMap<u64, Profile> = st.rtx(|(profiles, _)| {
        profiles.iter().map(|(p, _): (Profile, _)| (p.id, p)).collect()
    });

    // first boot: seed a default profile so the app opens with a draft
    if live.is_empty() {
        let p = Profile {
            id: 1,
            name: "me".to_string(),
            m: Measurements::default(),
        };
        st.wtx(|tx| tx.insert(&p));
        live.insert(p.id, p);
    }

    let _ = state_tx.send(snapshot!(st));

    for cmd in rx {
        match cmd {
            Cmd::Upsert { profile } => {
                st.wtx(|tx| {
                    if let Some(old) = live.get(&profile.id) {
                        tx.remove(old);
                    }
                    tx.insert(&profile);
                });
                live.insert(profile.id, profile);
            }
            Cmd::Delete { id } => {
                if let Some(old) = live.remove(&id) {
                    st.wtx(|tx| tx.remove(&old));
                }
            }
        }
        let _ = state_tx.send(snapshot!(st));
    }
}

#[tokio::main]
async fn serve(cmd_tx: mpsc::Sender<Cmd>, state_rx: watch::Receiver<Snapshot>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state((cmd_tx, state_rx));

    let port: u16 = std::env::var("PATTERNS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = format!("0.0.0.0:{port}");
    println!("patterns running on http://localhost:{port} (websocket at /ws)");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

type AppState = (mpsc::Sender<Cmd>, watch::Receiver<Snapshot>);

async fn ws_upgrade(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Per-client task: push every new snapshot down, feed every incoming JSON
/// command into the ingest thread.
async fn handle_socket(mut socket: WebSocket, (cmd_tx, mut state_rx): AppState) {
    let state_json = |s: &Snapshot| serde_json::to_string(s).unwrap();

    let hello = state_json(&state_rx.borrow_and_update());
    if socket.send(Message::text(hello)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = state_rx.changed() => {
                if changed.is_err() {
                    return; // ingest thread gone
                }
                let update = state_json(&state_rx.borrow_and_update());
                if socket.send(Message::text(update)).await.is_err() {
                    return;
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(Message::Text(line))) = incoming else {
                    return; // client closed or errored
                };
                let Ok(cmd) = serde_json::from_str::<Cmd>(&line) else {
                    continue; // ignore malformed commands
                };
                if cmd_tx.send(cmd).is_err() {
                    return;
                }
            }
        }
    }
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}
