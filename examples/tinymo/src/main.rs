//! tinymo: a toio cube whose every move is stored in bog (fold), so it can
//! replay a recorded session or reverse its last N moves back to the start.
//!
//! Shape:
//!
//!   toio/tinymo_driver.py  <-- JSON lines over TCP -->  this binary (owns the fold Stream)
//!
//! The Python driver talks BLE + keyboard and drives the cube directly on
//! each keypress; it *tells* us about each move after the fact. We persist
//! every move, and when asked (playback / boomerang) we send drive commands
//! back for the driver to execute.
//!
//!   cargo run -p tinymo -- record              # R = start/stop recording, P = play last session
//!   cargo run -p tinymo -- boomerang --limit 5 # after N moves, auto-reverse them
//!
//! then, in another terminal:  python3 examples/tinymo/toio/tinymo_driver.py

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fold::pipeline::{Aggregate, KeyBy, terminal};
use fold::stream::Stream;
use serde::{Deserialize, Serialize};

// ------------------------------------------------------------- data model --

/// One drive command, as the driver reported it: logical wheel speeds
/// (before any MOUNTED_BACKWARDS flip) and how long they ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Move {
    session: u64,
    seq: u64,
    at_ms: u64,
    left: i16,
    right: i16,
    duration_ms: u16,
    label: String,
}

impl Move {
    /// The exact inverse command: same wheels, same time, opposite direction.
    fn inverse(&self) -> Move {
        Move {
            left: -self.left,
            right: -self.right,
            label: format!("undo {}", self.label),
            ..self.clone()
        }
    }
}

/// Reverse a session's moves so that executing them returns to the start:
/// last move first, each one negated.
fn reversed(mut moves: Vec<Move>) -> Vec<Move> {
    moves.sort_by_key(|m| m.seq);
    moves.iter().rev().map(Move::inverse).collect()
}

// -------------------------------------------------------- wire protocol --

/// driver -> brain
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Inbound {
    Move {
        left: i16,
        right: i16,
        duration_ms: u16,
        #[serde(default)]
        label: String,
    },
    Key {
        key: String,
    },
    Hello {
        #[serde(default)]
        name: String,
    },
}

/// brain -> driver
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Outbound<'a> {
    Drive {
        left: i16,
        right: i16,
        duration_ms: u16,
        label: &'a str,
    },
    Led {
        r: u8,
        g: u8,
        b: u8,
    },
    Beep {
        effect: u8,
    },
    Status {
        text: String,
    },
    /// Tell the driver whether to accept drive keys right now.
    Input {
        enabled: bool,
    },
}

// ------------------------------------------------------------- the brain --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Record,
    Boomerang { limit: u64 },
}

/// One thread owns the fold Stream. Because the pipeline type contains
/// closures it can't be named, so `main` builds these closures where the
/// types are inferred and the rest of the program talks to bog through them.
struct Db<'a> {
    insert: Box<dyn FnMut(&Move) + 'a>,
    session_moves: Box<dyn Fn(u64) -> Vec<Move> + 'a>,
    session_count: Box<dyn Fn(u64) -> i64 + 'a>,
    max_session: Box<dyn Fn() -> Option<u64> + 'a>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match args.first().map(String::as_str) {
        Some("boomerang") => {
            let limit = args
                .iter()
                .position(|a| a == "--limit")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.parse().ok())
                .unwrap_or(5);
            Mode::Boomerang { limit }
        }
        Some("record") | None => Mode::Record,
        Some(other) => {
            eprintln!("unknown mode {other:?}; use `record` or `boomerang --limit N`");
            std::process::exit(2);
        }
    };

    // persistent across runs on purpose: recordings live in bog
    let db_path = std::env::temp_dir().join("tinymo.db");
    if args.iter().any(|a| a == "--fresh") {
        let _ = std::fs::remove_dir_all(&db_path);
    }
    let st = Stream::new(
        &db_path,
        (
            terminal::Count::new("moves_total"),
            terminal::Bag::<Move>::new("moves"),
            // moves per session, maintained incrementally; the boomerang
            // limit is read straight from this table
            KeyBy::new(
                |m: &Move| m.session,
                Aggregate::new(
                    "by_session",
                    |acc: &mut i64, _m: &Move, delta| *acc += delta as i64,
                    terminal::Table::<u64, i64>::new("moves_per_session"),
                ),
            ),
        ),
    );

    let total: i64 = st.rtx(|(count, _, _)| count.get());
    let st = std::cell::RefCell::new(st);
    let mut db = Db {
        insert: Box::new(|m: &Move| st.borrow_mut().wtx(|tx| tx.insert(m))),
        session_moves: Box::new(|session: u64| {
            let mut v: Vec<Move> = st.borrow().rtx(|(_, log, _)| {
                log.iter()
                    .filter(|(m, _): &(Move, i64)| m.session == session)
                    .map(|(m, _)| m)
                    .collect()
            });
            v.sort_by_key(|m| m.seq);
            v
        }),
        session_count: Box::new(|session: u64| {
            st.borrow()
                .rtx(|(_, _, per_session)| per_session.get(&session).unwrap_or(0))
        }),
        max_session: Box::new(|| {
            st.borrow()
                .rtx(|(_, _, per_session)| per_session.iter().map(|(s, _): (u64, i64)| s).max())
        }),
    };
    let last_session = (db.max_session)();
    println!("tinymo brain [{mode:?}] db={}", db_path.display());
    println!(
        "  bog has {total} moves across {} sessions",
        last_session.map(|s| s + 1).unwrap_or(0)
    );

    let port: u16 = std::env::var("TINYMO_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7777);
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    println!("  listening on 127.0.0.1:{port} — start toio/tinymo_driver.py");

    // one driver at a time; when it disconnects, wait for the next
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        println!("driver connected");
        if let Err(e) = serve_driver(conn, mode, &mut db) {
            println!("driver connection ended: {e}");
        }
        println!("waiting for driver ...");
    }
}

struct Driver {
    out: TcpStream,
}

impl Driver {
    fn send(&mut self, msg: &Outbound) -> std::io::Result<()> {
        let mut line = serde_json::to_string(msg).unwrap();
        line.push('\n');
        self.out.write_all(line.as_bytes())
    }
    fn status(&mut self, text: impl Into<String>) -> std::io::Result<()> {
        let text = text.into();
        println!("  {text}");
        self.send(&Outbound::Status { text })
    }
    fn led(&mut self, r: u8, g: u8, b: u8) -> std::io::Result<()> {
        self.send(&Outbound::Led { r, g, b })
    }

    /// Execute a list of moves on the cube with their original timing.
    /// Blocks this thread — the driver ignores keys while `Input{false}`.
    fn execute(&mut self, moves: &[Move]) -> std::io::Result<()> {
        self.send(&Outbound::Input { enabled: false })?;
        for (i, m) in moves.iter().enumerate() {
            self.status(format!(
                "  [{}/{}] {} L={:+} R={:+} {}ms",
                i + 1,
                moves.len(),
                m.label,
                m.left,
                m.right,
                m.duration_ms
            ))?;
            self.send(&Outbound::Drive {
                left: m.left,
                right: m.right,
                duration_ms: m.duration_ms,
                label: &m.label,
            })?;
            // a little slack so consecutive timed commands don't overlap
            std::thread::sleep(Duration::from_millis(m.duration_ms as u64 + 60));
        }
        self.send(&Outbound::Input { enabled: true })
    }
}

fn serve_driver(conn: TcpStream, mode: Mode, db: &mut Db<'_>) -> std::io::Result<()> {
    let reader = BufReader::new(conn.try_clone()?);
    let mut drv = Driver { out: conn };

    // session bookkeeping (in memory; the moves themselves are in bog)
    let mut next_session: u64 = (db.max_session)().map(|s| s + 1).unwrap_or(0);
    let mut recording: Option<u64> = None; // record mode: active session
    let mut seq: u64 = 0;

    match mode {
        Mode::Record => {
            drv.led(0, 255, 0)?;
            drv.status("record mode: R = start/stop recording, P = play last recording")?;
        }
        Mode::Boomerang { limit } => {
            // boomerang always records; a session is one "trip out"
            recording = Some(next_session);
            next_session += 1;
            drv.led(255, 120, 0)?;
            drv.status(format!(
                "boomerang mode: make {limit} moves and I'll drive them back in reverse"
            ))?;
        }
    }

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Inbound = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                println!("bad line from driver ({e}): {line}");
                continue;
            }
        };
        match msg {
            Inbound::Hello { name } => {
                println!("driver hello: {name}");
            }
            Inbound::Move { left, right, duration_ms, label } => {
                let Some(session) = recording else {
                    println!("  move {label} (not recording)");
                    continue;
                };
                let m = Move {
                    session,
                    seq,
                    at_ms: now_ms(),
                    left,
                    right,
                    duration_ms,
                    label,
                };
                seq += 1;
                (db.insert)(&m);
                let n = (db.session_count)(session);
                drv.status(format!("session {session}: {n} moves  (+{} L={left:+} R={right:+})", m.label))?;

                if let Mode::Boomerang { limit } = mode
                    && n >= limit as i64
                {
                    let moves = (db.session_moves)(session);
                    drv.send(&Outbound::Beep { effect: 4 })?;
                    drv.led(0, 80, 255)?;
                    drv.status(format!("limit reached — reversing {} moves", moves.len()))?;
                    std::thread::sleep(Duration::from_millis(500));
                    drv.execute(&reversed(moves))?;
                    drv.led(255, 120, 0)?;
                    // next trip
                    recording = Some(next_session);
                    next_session += 1;
                    seq = 0;
                    drv.status(format!("back home. new session {}", next_session - 1))?;
                }
            }
            Inbound::Key { key } => match (mode, key.as_str()) {
                (Mode::Record, "r") => {
                    if let Some(session) = recording.take() {
                        let n = (db.session_count)(session);
                        drv.led(0, 255, 0)?;
                        drv.status(format!("stopped recording session {session} ({n} moves)"))?;
                    } else {
                        recording = Some(next_session);
                        next_session += 1;
                        seq = 0;
                        drv.led(255, 0, 0)?;
                        drv.status(format!("recording session {} ...", next_session - 1))?;
                    }
                }
                (Mode::Record, "p") => {
                    if recording.is_some() {
                        drv.status("stop recording first (R)")?;
                        continue;
                    }
                    // most recent non-empty session
                    let Some(session) = (db.max_session)() else {
                        drv.status("nothing recorded yet")?;
                        continue;
                    };
                    let moves = (db.session_moves)(session);
                    drv.led(0, 80, 255)?;
                    drv.status(format!("playing session {session}: {} moves", moves.len()))?;
                    drv.execute(&moves)?;
                    drv.led(0, 255, 0)?;
                    drv.status("playback done")?;
                }
                (_, k) => println!("  key {k:?} ignored in {mode:?}"),
            },
        }
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(seq: u64, left: i16, right: i16, label: &str) -> Move {
        Move {
            session: 0,
            seq,
            at_ms: 0,
            left,
            right,
            duration_ms: 400,
            label: label.into(),
        }
    }

    #[test]
    fn reversed_negates_and_reverses_order() {
        let out = reversed(vec![
            mv(0, 90, 90, "forward"),
            mv(1, -90, 90, "spin left"),
            mv(2, 30, 90, "curve left"),
        ]);
        assert_eq!(out.len(), 3);
        assert_eq!((out[0].left, out[0].right), (-30, -90));
        assert_eq!((out[1].left, out[1].right), (90, -90));
        assert_eq!((out[2].left, out[2].right), (-90, -90));
        assert_eq!(out[2].label, "undo forward");
    }

    #[test]
    fn reversed_sorts_by_seq_first() {
        let out = reversed(vec![mv(2, 1, 1, "c"), mv(0, 3, 3, "a"), mv(1, 2, 2, "b")]);
        let seqs: Vec<u64> = out.iter().map(|m| m.seq).collect();
        assert_eq!(seqs, vec![2, 1, 0]);
    }

    #[test]
    fn inbound_json_shapes() {
        let m: Inbound =
            serde_json::from_str(r#"{"type":"move","left":90,"right":-90,"duration_ms":400,"label":"spin"}"#)
                .unwrap();
        assert!(matches!(m, Inbound::Move { left: 90, right: -90, duration_ms: 400, .. }));
        let k: Inbound = serde_json::from_str(r#"{"type":"key","key":"r"}"#).unwrap();
        assert!(matches!(k, Inbound::Key { .. }));
    }

    #[test]
    fn outbound_json_shape() {
        let s = serde_json::to_string(&Outbound::Drive { left: 1, right: -1, duration_ms: 400, label: "x" }).unwrap();
        assert_eq!(s, r#"{"type":"drive","left":1,"right":-1,"duration_ms":400,"label":"x"}"#);
    }
}
