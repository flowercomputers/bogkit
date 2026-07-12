//! HTTP + websocket surface. One inline page handles both the team-join
//! step and phone location streaming — a player just opens this same URL
//! on the phone they'll carry into the park, no separate "phone link" or
//! QR handoff needed. No static files, no template engine, matching
//! `examples/chat`'s convention.

use std::sync::mpsc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use tokio::sync::watch;

use crate::domain::Scoreboard;
use crate::protocol::ClientMsg;

type AppState = (mpsc::Sender<ClientMsg>, watch::Receiver<Scoreboard>);

#[tokio::main]
pub async fn serve(msg_tx: mpsc::Sender<ClientMsg>, state_rx: watch::Receiver<Scoreboard>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state((msg_tx, state_rx));

    let port: u16 = std::env::var("MMO_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = format!("0.0.0.0:{port}");
    println!("area denial running on http://localhost:{port} (websocket at /ws)");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn ws_upgrade(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Per-client task: push every new scoreboard down, feed every incoming
/// client message into the ingest thread.
async fn handle_socket(mut socket: WebSocket, (msg_tx, mut state_rx): AppState) {
    let state_json = |s: &Scoreboard| serde_json::to_string(s).unwrap();

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
                let Some(Ok(Message::Text(text))) = incoming else {
                    return; // client closed or errored
                };
                match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(msg) => {
                        if msg_tx.send(msg).is_err() {
                            return; // ingest thread gone
                        }
                    }
                    Err(e) => eprintln!("dropping malformed client message: {e}"),
                }
            }
        }
    }
}

async fn index() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>area denial</title>
<style>
  body { font-family: system-ui, sans-serif; max-width: 32rem; margin: 2rem auto; padding: 0 1rem; }
  h1 { font-size: 1.25rem; }
  .teams { display: flex; gap: 1rem; margin: 1rem 0; }
  .team { flex: 1; border: 1px solid #888; border-radius: 6px; padding: 0.75rem; }
  .team button { width: 100%; padding: 0.5rem; font-size: 1rem; }
  .team.mine { border-width: 3px; }
  .bar { background: #ddd; border-radius: 3px; height: 0.6rem; margin-top: 0.4rem; overflow: hidden; }
  .bar > div { background: #2a7; height: 100%; }
  #banner { padding: 0.75rem; border-radius: 6px; margin: 1rem 0; display: none; }
  #banner.show { display: block; }
  #status { color: #555; }
  #my-status { font-weight: bold; margin: 0.5rem 0; }
  #my-status.in { color: #2a7; }
  #my-status.out { color: #c33; }
  button:disabled { opacity: 0.5; }
</style>
</head>
<body>
<h1>area denial: <span id="park">connecting...</span></h1>
<div id="status"></div>
<div id="my-status"></div>
<div id="banner"></div>
<div class="teams">
  <div class="team" id="team-court_square">
    <h3>Team Court Square</h3>
    <div id="members-court_square">0 members</div>
    <div class="bar"><div id="bar-court_square" style="width:0%"></div></div>
    <div id="pct-court_square"></div>
    <button id="join-court_square">join</button>
  </div>
  <div class="team" id="team-church_ave">
    <h3>Team Church Ave</h3>
    <div id="members-church_ave">0 members</div>
    <div class="bar"><div id="bar-church_ave" style="width:0%"></div></div>
    <div id="pct-church_ave"></div>
    <button id="join-church_ave">join</button>
  </div>
</div>
<button id="start" style="display:none">start battle</button>
<p style="color:#888; font-size:0.9rem">
  Location is only sent once a battle is active, and only ever shown as a
  team-wide aggregate — never your teammates' or opponents' positions.
</p>
<script>
const PLAYER_KEY = "mmo_player_id";
const TEAM_KEY = "mmo_team";

let playerId = localStorage.getItem(PLAYER_KEY);
if (!playerId) {
  playerId = crypto.randomUUID();
  localStorage.setItem(PLAYER_KEY, playerId);
}
let myTeam = localStorage.getItem(TEAM_KEY); // "court_square" | "church_ave" | null

let latest = null; // last scoreboard from the server
let watchId = null;
let lastPingAt = 0;
let lastFix = null; // { lat, lon } from the most recent geolocation fix

const ws = new WebSocket(`ws://${location.host}/ws`);

function send(msg) { ws.send(JSON.stringify(msg)); }

document.getElementById("join-court_square").onclick = () => join("court_square");
document.getElementById("join-church_ave").onclick = () => join("church_ave");
document.getElementById("start").onclick = () => send({ type: "start_battle" });

function join(team) {
  myTeam = team;
  localStorage.setItem(TEAM_KEY, team);
  send({ type: "join", player: playerId, team });
  render();
}

function startStreaming() {
  if (watchId !== null || !navigator.geolocation) return;
  watchId = navigator.geolocation.watchPosition(
    (pos) => {
      const now = Date.now();
      lastFix = { lat: pos.coords.latitude, lon: pos.coords.longitude };
      renderMyStatus(); // update immediately, independent of the ping throttle below
      if (now - lastPingAt < 3000) return; // throttle sends to ~1 ping / 3s
      lastPingAt = now;
      send({
        type: "ping",
        player: playerId,
        lat: lastFix.lat,
        lon: lastFix.lon,
        client_ms: now,
      });
    },
    (err) => console.warn("geolocation error", err),
    { enableHighAccuracy: true, maximumAge: 5000 },
  );
}

function inBounds(bbox, lat, lon) {
  return lon >= bbox.min_lon && lon <= bbox.max_lon && lat >= bbox.min_lat && lat <= bbox.max_lat;
}

// Shown only while actively streaming (on a team, battle active). Computed
// entirely client-side from the last GPS fix and the battle's bbox (already
// in every scoreboard) — no extra server round-trip needed.
function renderMyStatus() {
  const el = document.getElementById("my-status");
  if (!latest || !myTeam || latest.battle.status !== "active") {
    el.textContent = "";
    el.className = "";
    return;
  }
  if (!lastFix) {
    el.textContent = "you: waiting for location...";
    el.className = "";
    return;
  }
  const inside = inBounds(latest.battle.park.bbox, lastFix.lat, lastFix.lon);
  el.textContent = inside ? "you: ✅ inside the battleground" : "you: ❌ outside the battleground";
  el.className = inside ? "in" : "out";
}

function fmtCountdown(ms) {
  if (ms <= 0) return "0:00:00";
  const totalSec = Math.floor(ms / 1000);
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function render() {
  if (!latest) return;
  const { battle, court_square, church_ave } = latest;
  const teams = { court_square, church_ave };

  document.getElementById("park").textContent = battle.park.name;

  for (const key of ["court_square", "church_ave"]) {
    const t = teams[key];
    const pct = t.members > 0 ? Math.round((100 * t.in_bounds) / t.members) : 0;
    document.getElementById(`members-${key}`).textContent = `${t.members} members`;
    document.getElementById(`bar-${key}`).style.width = `${pct}%`;
    document.getElementById(`pct-${key}`).textContent =
      battle.status === "pending" ? "" : `${t.in_bounds}/${t.members} in bounds (${pct}%)`;
    document.getElementById(`team-${key}`).classList.toggle("mine", myTeam === key);
    const btn = document.getElementById(`join-${key}`);
    btn.style.display = myTeam || battle.status !== "pending" ? "none" : "";
  }

  const startBtn = document.getElementById("start");
  startBtn.style.display = battle.status === "pending" && myTeam ? "" : "none";
  startBtn.disabled = court_square.members === 0 || church_ave.members === 0;

  const statusEl = document.getElementById("status");
  const banner = document.getElementById("banner");
  banner.classList.remove("show");

  if (battle.status === "pending") {
    statusEl.textContent = myTeam
      ? "waiting for both teams to have at least one member, then start the battle"
      : "pick a team to join";
  } else if (battle.status === "active") {
    const remaining = (battle.ends_at_ms ?? 0) - Date.now();
    statusEl.textContent = `battle active — time remaining ${fmtCountdown(remaining)}`;
    if (myTeam) startStreaming();
  } else if (battle.status === "ended") {
    statusEl.textContent = "battle ended";
    banner.classList.add("show");
    const o = battle.outcome;
    if (!o) {
      banner.textContent = "battle ended";
    } else if (o.kind === "tie") {
      banner.textContent = "battle ended in a tie";
    } else {
      const label = o.winner === "court_square" ? "Team Court Square" : "Team Church Ave";
      const via = o.kind === "elimination" ? "by eliminating the opposing team" : "on time, by percentage";
      banner.textContent = `${label} wins ${via}!`;
    }
  }

  renderMyStatus();
}

ws.onmessage = (event) => {
  latest = JSON.parse(event.data);
  render();
};

// keep the countdown ticking smoothly between server pushes
setInterval(() => { if (latest && latest.battle.status === "active") render(); }, 1000);
</script>
</body>
</html>"#,
    )
}
