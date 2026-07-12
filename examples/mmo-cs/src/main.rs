//! Area denial: a location-based team game where fold maintains two live
//! per-team aggregates — roster size and in-bounds presence — over a
//! stream of joins and GPS pings, broadcast to every browser over a
//! websocket. See `battle.rs` for the fold pipelines and win-condition
//! logic, `web.rs` for the HTTP/websocket surface, `parks.rs` for the
//! battleground catalog.
//!
//! Run with `cargo run -p mmo-cs`, then open http://localhost:3000 — join
//! a team in two browser tabs (or two devices), start the battle once both
//! teams have a member, and use devtools' geolocation override (or a real
//! phone) to move each "player" in or out of the chosen park's bounding box.

mod battle;
mod domain;
mod parks;
mod protocol;
mod web;

use std::sync::mpsc;

use domain::Scoreboard;
use protocol::ClientMsg;
use tokio::sync::watch;

fn main() {
    // MMO_DATA_DIR should point at a stable path for a real deployment (a
    // tmp-cleaner or reboot could otherwise wipe an in-progress battle);
    // defaults to a fresh temp dir for local dev, matching the other
    // example crates' convention.
    let data_dir = match std::env::var_os("MMO_DATA_DIR") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => {
            let dir = std::env::temp_dir().join("bog-kit-mmo-cs");
            let _ = std::fs::remove_dir_all(&dir);
            dir
        }
    };
    std::fs::create_dir_all(&data_dir).expect("create MMO_DATA_DIR");

    let (msg_tx, msg_rx) = mpsc::channel::<ClientMsg>();
    let (state_tx, state_rx) = watch::channel(placeholder_scoreboard());
    std::thread::spawn(move || battle::run(&data_dir, msg_rx, state_tx));

    web::serve(msg_tx, state_rx);
}

/// Sent to no one — overwritten by `battle::run`'s first snapshot before
/// any websocket client can possibly connect. Exists only because
/// `watch::channel` needs an initial value.
fn placeholder_scoreboard() -> Scoreboard {
    Scoreboard {
        battle: domain::Battle {
            id: 0,
            park: domain::Park {
                id: String::new(),
                name: String::new(),
                bbox: domain::Bbox { min_lon: 0.0, min_lat: 0.0, max_lon: 0.0, max_lat: 0.0 },
            },
            status: domain::BattleStatus::Pending,
            started_at_ms: None,
            ends_at_ms: None,
            outcome: None,
        },
        court_square: domain::TeamStats::default(),
        church_ave: domain::TeamStats::default(),
    }
}
