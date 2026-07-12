//! What's over your head right now: a fold-backed live sky map.
//!
//! The inversion that makes this a good fit for incremental dataflow:
//! the earth spinning does NOT change the data. A star's celestial
//! coordinates (right ascension / declination) are fixed; rotation only
//! changes *which keys are over your head*. So:
//!
//! - The catalog (~2000 stars + deep-sky objects) is materialized ONCE
//!   into a fold `Multimap` keyed by 10°x10° celestial buckets. Answering
//!   "what's overhead at (lat, lon, t)" is a handful of point reads on
//!   the bucket index — a sliding window over right ascension that
//!   advances at 15°/hour. Zero writes per tick, per client, forever.
//!
//! - The only genuine deltas are bodies whose celestial coordinates
//!   actually move: the sun, the moon, the planets. An ephemeris thread
//!   recomputes them every few seconds; when a position really changed,
//!   the old record is retracted and the new one inserted, and fold
//!   cascades that through every view (bucket index, per-kind counts,
//!   magnitude histogram) by retraction — nothing is rebuilt.
//!
//! Serving shape is the same as the `chat` example:
//!
//!   ephemeris thread -> mpsc -> ingest thread (owns the fold Stream)
//!     -> watch -> websocket tasks -> browsers

mod astro;

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Html,
    routing::get,
};
use fold::pipeline::{Aggregate, KeyBy, ScoreBy, terminal};
use fold::stream::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// Radius of the "overhead" cone around the zenith, degrees.
const CONE_DEG: f64 = 30.0;

/// Celestial bucket size, degrees, in both RA and dec.
const BUCKET_DEG: f64 = 10.0;
const DEC_BANDS: u8 = 18;
const RA_BUCKETS: u8 = 36;

/// A position change smaller than this is not worth a retract/insert.
const MOVE_EPS_DEG: f64 = 1e-4;

/// One record in the sky database. `name` is the primary key; retraction
/// requires pushing back the exact record that was inserted, so the
/// ingest thread always reads the current record from the `by_name`
/// table before retracting it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Body {
    name: String,
    kind: String, // "star" | "planet" | "moon" | "sun" | "galaxy" | ...
    ra_deg: f64,
    dec_deg: f64,
    mag: f64,
    con: String,
}

/// The bucket a body lives in: (declination band, right-ascension bucket).
fn bucket_of(ra_deg: f64, dec_deg: f64) -> (u8, u8) {
    let band = (((dec_deg + 90.0) / BUCKET_DEG) as u8).min(DEC_BANDS - 1);
    let ra = ((ra_deg.rem_euclid(360.0)) / BUCKET_DEG) as u8 % RA_BUCKETS;
    (band, ra)
}

#[derive(Debug, Clone, Serialize)]
struct DeltaEvent {
    at_ms: u64,
    action: String, // "loaded" | "appeared" | "moved"
    name: String,
    kind: String,
    detail: String,
}

/// What the ingest thread publishes after every committed transaction:
/// one consistent snapshot of every materialized view.
#[derive(Debug, Clone, Default)]
struct SkyState {
    version: u64,
    /// The whole bucket index, mirrored out of fold's `Multimap` so
    /// websocket tasks can do their per-client point reads without
    /// touching the store.
    buckets: HashMap<(u8, u8), Vec<Body>>,
    total: i64,
    kind_counts: Vec<(String, i64)>,
    mag_hist: Vec<(i64, i64)>,
    events: Vec<DeltaEvent>,
}

enum Cmd {
    /// Upsert bodies by name: for each body, retract the current record
    /// under that name (if any) and insert the new one — but only when
    /// the record actually changed. Positions within `MOVE_EPS_DEG` are
    /// considered unchanged, which makes replaying the catalog on every
    /// startup a no-op.
    Upsert { bodies: Vec<Body>, label: String },
}

fn main() {
    // persistent across restarts: the catalog loads once, ephemeris
    // records pick up where they left off. delete the dir to reset.
    let db_path = std::env::temp_dir().join("bog-kit-overhead.db");

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (state_tx, state_rx) = watch::channel(SkyState::default());

    std::thread::spawn(move || ingest(&db_path, cmd_rx, state_tx));
    std::thread::spawn(move || ephemeris_loop(cmd_tx));

    serve(state_rx);
}

// ---------------------------------------------------------------- catalog

fn load_catalog() -> Vec<Body> {
    let mut bodies = Vec::new();

    for line in include_str!("../data/stars.csv").lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 5 {
            continue;
        }
        bodies.push(Body {
            name: f[0].to_string(),
            kind: "star".to_string(),
            ra_deg: f[1].parse::<f64>().unwrap() * 15.0, // hours -> degrees
            dec_deg: f[2].parse().unwrap(),
            mag: f[3].parse().unwrap(),
            con: f[4].to_string(),
        });
    }

    for line in include_str!("../data/dsos.csv").lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 6 {
            continue;
        }
        bodies.push(Body {
            name: f[0].to_string(),
            kind: f[5].to_string(),
            ra_deg: f[1].parse::<f64>().unwrap() * 15.0,
            dec_deg: f[2].parse().unwrap(),
            mag: f[3].parse().unwrap(),
            con: f[4].to_string(),
        });
    }

    bodies
}

// ------------------------------------------------------------- ephemeris

/// The delta source: recompute geocentric positions of the bodies that
/// genuinely move against the celestial sphere and hand them to the
/// ingest thread. Everything else in the database never gets written
/// again after the initial load.
fn ephemeris_loop(cmd_tx: mpsc::Sender<Cmd>) {
    loop {
        let d = astro::day_number(now_ms());
        let mut bodies = Vec::with_capacity(9);

        let (ra, dec) = astro::sun_radec(d);
        bodies.push(Body {
            name: "Sun".into(),
            kind: "sun".into(),
            ra_deg: ra,
            dec_deg: dec,
            mag: -26.7,
            con: String::new(),
        });

        let (ra, dec) = astro::moon_radec(d);
        bodies.push(Body {
            name: "Moon".into(),
            kind: "moon".into(),
            ra_deg: ra,
            dec_deg: dec,
            mag: -12.7,
            con: String::new(),
        });

        for name in astro::PLANETS {
            let (ra, dec) = astro::planet_radec(name, d);
            bodies.push(Body {
                name: name.into(),
                kind: "planet".into(),
                ra_deg: ra,
                dec_deg: dec,
                mag: astro::planet_mag(name),
                con: String::new(),
            });
        }

        if cmd_tx
            .send(Cmd::Upsert {
                bodies,
                label: "ephemeris".into(),
            })
            .is_err()
        {
            return;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

// ---------------------------------------------------------------- ingest

/// Upsert a batch of bodies by name: retract each current record and
/// insert the new one, but only when something actually changed. Yields
/// whether anything committed. A macro because the pipeline type (and
/// thus the reader tuple type) contains closures and can't be written out.
macro_rules! apply {
    ($st:expr, $events:expr, $bodies:expr, $label:expr) => {{
        let bodies: Vec<Body> = $bodies;
        // pair each incoming body with the record currently under its name
        let olds: Vec<Option<Body>> =
            $st.rtx(|(_, _, _, _, by_name)| bodies.iter().map(|b| by_name.get(&b.name)).collect());

        let changed: Vec<(Option<Body>, Body)> = bodies
            .into_iter()
            .zip(olds)
            .filter(|(new, old)| match old {
                None => true,
                Some(old) => {
                    astro::angular_sep_deg(old.ra_deg, old.dec_deg, new.ra_deg, new.dec_deg)
                        > MOVE_EPS_DEG
                        || old.mag != new.mag
                        || old.kind != new.kind
                        || old.con != new.con
                }
            })
            .map(|(new, old)| (old, new))
            .collect();

        if changed.is_empty() {
            false
        } else {
            // one atomic transaction: every view sees all retractions and
            // insertions together or not at all
            $st.wtx(|tx| {
                for (old, new) in &changed {
                    if let Some(old) = old {
                        tx.remove(old);
                    }
                    tx.insert(new);
                }
            });
            log_events($events, &changed, $label);
            true
        }
    }};
}

/// Read one consistent snapshot of every sink into a `SkyState`.
macro_rules! publish {
    ($st:expr, $version:expr, $events:expr) => {
        $st.rtx(|(total, sky, kinds, hist, _)| {
            let mut buckets = HashMap::new();
            for band in 0..DEC_BANDS {
                for ra in 0..RA_BUCKETS {
                    let v: Vec<Body> = sky.get(&(band, ra));
                    if !v.is_empty() {
                        buckets.insert((band, ra), v);
                    }
                }
            }
            SkyState {
                version: $version,
                buckets,
                total: total.get(),
                kind_counts: kinds.iter().collect(),
                mag_hist: hist.iter().collect(),
                events: $events.iter().cloned().collect(),
            }
        })
    };
}

fn log_events(events: &mut VecDeque<DeltaEvent>, changed: &[(Option<Body>, Body)], label: &str) {
    let at_ms = now_ms();
    if label == "catalog" {
        events.push_front(DeltaEvent {
            at_ms,
            action: "loaded".into(),
            name: format!("{} bodies", changed.len()),
            kind: "catalog".into(),
            detail: "initial ingest".into(),
        });
    } else {
        for (old, new) in changed {
            let (action, detail) = match old {
                Some(old) => (
                    "moved",
                    format!(
                        "Δ {:.4}° {} bucket {:?}",
                        astro::angular_sep_deg(old.ra_deg, old.dec_deg, new.ra_deg, new.dec_deg),
                        if bucket_of(old.ra_deg, old.dec_deg)
                            == bucket_of(new.ra_deg, new.dec_deg)
                        {
                            "within"
                        } else {
                            "CROSSED into"
                        },
                        bucket_of(new.ra_deg, new.dec_deg),
                    ),
                ),
                None => (
                    "appeared",
                    format!("bucket {:?}", bucket_of(new.ra_deg, new.dec_deg)),
                ),
            };
            events.push_front(DeltaEvent {
                at_ms,
                action: action.into(),
                name: new.name.clone(),
                kind: new.kind.clone(),
                detail,
            });
        }
    }
    events.truncate(40);
}

/// Owns the fold stream: applies upserts, republishes snapshots.
fn ingest(db_path: &std::path::Path, rx: mpsc::Receiver<Cmd>, state_tx: watch::Sender<SkyState>) {
    let mut st = Stream::new(
        db_path,
        (
            terminal::Count::new("bodies_total"),
            // the star of the show: bodies indexed by celestial bucket.
            // "what's overhead" = point reads of the buckets around the
            // zenith, whose keys slide with sidereal time.
            KeyBy::new(
                |b: &Body| bucket_of(b.ra_deg, b.dec_deg),
                terminal::Multimap::new("sky_index"),
            ),
            // per-kind counts, maintained by retraction: when the moon
            // moves, its -old/+new cancel and this view doesn't churn
            KeyBy::new(
                |b: &Body| b.kind.clone(),
                Aggregate::new(
                    "count_by_kind",
                    |acc: &mut i64, _b: &Body, d| *acc += d as i64,
                    terminal::Table::new("kind_counts"),
                ),
            ),
            // brightness distribution in whole-magnitude buckets
            ScoreBy::new(
                |b: &Body| b.mag,
                terminal::Histogram::new("mag_hist", |mag: &f64| mag.floor() as i64),
            ),
            // primary-key view: the current record under each name, read
            // before every upsert so retractions push back exact bytes
            KeyBy::new(
                |b: &Body| b.name.clone(),
                terminal::Table::new("by_name"),
            ),
        ),
    );

    let mut events: VecDeque<DeltaEvent> = VecDeque::new();
    let mut version = 0u64;

    // replaying the catalog is a no-op on every startup after the first:
    // nothing moved, so nothing commits
    apply!(st, &mut events, load_catalog(), "catalog");
    let _ = state_tx.send(publish!(st, version, events));

    for Cmd::Upsert { bodies, label } in rx {
        if apply!(st, &mut events, bodies, &label) {
            version += 1;
            let _ = state_tx.send(publish!(st, version, events));
        }
    }
}

// ----------------------------------------------------------------- serve

#[derive(Debug, Clone, Copy, Deserialize)]
struct Params {
    lat: f64,
    lon: f64,
    #[serde(default = "default_warp")]
    warp: f64,
}
fn default_warp() -> f64 {
    1.0
}

#[tokio::main]
async fn serve(state_rx: watch::Receiver<SkyState>) {
    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state(state_rx);

    let port: u16 = std::env::var("OVERHEAD_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr = format!("0.0.0.0:{port}");
    println!("overhead running on http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn ws_upgrade(
    State(state_rx): State<watch::Receiver<SkyState>>,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state_rx))
}

/// Per-client task. Holds the client's (lat, lon, time-warp) and, on every
/// tick / data change, slides the read window: pick the buckets around the
/// zenith, point-read them from the latest snapshot, exact-filter to the
/// cone. Re-sends only when the overhead set (or the data) changed.
async fn handle_socket(mut socket: WebSocket, mut state_rx: watch::Receiver<SkyState>) {
    let mut params: Option<Params> = None;
    // virtual clock: vt advances at `warp` x real time from vt_base
    let mut vt_base_ms = now_ms() as f64;
    let mut real_base = Instant::now();
    let mut last_sig = 0u64;

    let mut tick = tokio::time::interval(Duration::from_millis(1000));

    loop {
        tokio::select! {
            _ = tick.tick() => {}
            changed = state_rx.changed() => {
                if changed.is_err() {
                    return; // ingest thread gone
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(msg)) = incoming else { return };
                if let Message::Text(text) = msg
                    && let Ok(p) = serde_json::from_str::<Params>(&text)
                {
                    // keep the virtual clock continuous across warp changes
                    let vt_now =
                        vt_base_ms + real_base.elapsed().as_millis() as f64
                            * params.map_or(1.0, |p| p.warp);
                    vt_base_ms = vt_now;
                    real_base = Instant::now();
                    params = Some(p);
                    last_sig = 0; // force a send
                }
            }
        }

        let Some(p) = params else { continue };
        let vt_ms = (vt_base_ms + real_base.elapsed().as_millis() as f64 * p.warp) as u64;
        let lst = astro::lst_deg(vt_ms, p.lon);

        let payload = {
            let state = state_rx.borrow_and_update();

            // the sliding window: buckets whose cells can intersect the
            // zenith cone. bucket half-diagonal ≈ 7.1°, padded to 8.
            let mut bucket_keys = Vec::new();
            for band in 0..DEC_BANDS {
                for ra in 0..RA_BUCKETS {
                    let c_dec = band as f64 * BUCKET_DEG - 90.0 + BUCKET_DEG / 2.0;
                    let c_ra = ra as f64 * BUCKET_DEG + BUCKET_DEG / 2.0;
                    if astro::angular_sep_deg(lst, p.lat, c_ra, c_dec) <= CONE_DEG + 8.0 {
                        bucket_keys.push((band, ra));
                    }
                }
            }

            let mut candidates = 0usize;
            let mut overhead: Vec<&Body> = Vec::new();
            for key in &bucket_keys {
                if let Some(bodies) = state.buckets.get(key) {
                    candidates += bodies.len();
                    for b in bodies {
                        if astro::angular_sep_deg(lst, p.lat, b.ra_deg, b.dec_deg) <= CONE_DEG {
                            overhead.push(b);
                        }
                    }
                }
            }
            overhead.sort_by(|a, b| a.mag.total_cmp(&b.mag));

            // signature of what the client can see: data version + the
            // set of overhead names. skip the send if neither changed.
            let sig = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                state.version.hash(&mut h);
                for b in &overhead {
                    b.name.hash(&mut h);
                    b.ra_deg.to_bits().hash(&mut h);
                    b.dec_deg.to_bits().hash(&mut h);
                }
                h.finish()
            };
            if sig == last_sig {
                None
            } else {
                last_sig = sig;
                Some(serde_json::json!({
                    "type": "sky",
                    "vt_ms": vt_ms,
                    "lst_deg": lst,
                    "lat": p.lat,
                    "lon": p.lon,
                    "warp": p.warp,
                    "cone_deg": CONE_DEG,
                    "read_path": {
                        "buckets_total": (DEC_BANDS as usize) * (RA_BUCKETS as usize),
                        "buckets_read": bucket_keys.len(),
                        "candidates": candidates,
                        "overhead": overhead.len(),
                    },
                    "bodies": overhead,
                    "stats": {
                        "total": state.total,
                        "kind_counts": state.kind_counts,
                        "mag_hist": state.mag_hist,
                        "version": state.version,
                    },
                    "events": state.events,
                })
                .to_string())
            }
        };

        if let Some(json) = payload
            && socket.send(Message::text(json)).await.is_err()
        {
            return;
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}
