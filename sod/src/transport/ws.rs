//! Blocking websocket transport (feature `ws`).
//!
//! Wire: each protocol [`Msg`] is one binary websocket message, postcard-
//! encoded. The exchange is strictly half-duplex to be deadlock-free over
//! blocking sockets:
//!
//! - initiator: send Hello → recv Hello → send our Frames…Done → recv theirs
//! - responder: recv Hello → send Hello → recv their Frames…Done → send ours
//!
//! Both sides end up with the union either way — symmetry is a property of
//! the protocol, not of who dialed. There is nothing to resume: an
//! interrupted session leaves both replicas correct (SOD-6) and the next
//! session picks up from the version vectors.

use std::net::{TcpListener, TcpStream};

use tungstenite::{Message, WebSocket};

use crate::engine::Engine;
use crate::replica::Replica;
use crate::store::LogStore;
use crate::sync::{Msg, Session};
use crate::SodError;

fn io_err<E: std::fmt::Display>(e: E) -> SodError {
    SodError::Io(e.to_string())
}

fn send<S: std::io::Read + std::io::Write>(
    sock: &mut WebSocket<S>,
    msg: &Msg,
) -> Result<(), SodError> {
    let bytes = postcard::to_stdvec(msg).map_err(io_err)?;
    sock.send(Message::Binary(bytes.into())).map_err(io_err)
}

fn recv<S: std::io::Read + std::io::Write>(sock: &mut WebSocket<S>) -> Result<Msg, SodError> {
    loop {
        match sock.read().map_err(io_err)? {
            Message::Binary(b) => {
                return postcard::from_bytes(&b)
                    .map_err(|_| SodError::Corrupt("undecodable sync message"));
            }
            // tungstenite answers pings itself on the next read/write;
            // ignore everything that isn't a protocol message
            Message::Close(_) => return Err(SodError::Io("peer closed mid-session".into())),
            _ => {}
        }
    }
}

/// Run one session over an established socket. `initiator` fixes the
/// half-duplex order; see the module docs.
fn run_session<E: Engine, L: LogStore, S: std::io::Read + std::io::Write>(
    sock: &mut WebSocket<S>,
    r: &mut Replica<E, L>,
    schema: u32,
    initiator: bool,
) -> Result<(), SodError> {
    let mut session = Session::new(schema);

    if initiator {
        send(sock, &session.hello(r))?;
    }
    let peer_hello = recv(sock)?;
    // Our Frames…Done for the peer, computed from their Hello.
    let ours = session.on_msg(r, peer_hello)?;
    if !initiator {
        send(sock, &session.hello(r))?;
    }

    if initiator {
        for m in &ours {
            send(sock, m)?;
        }
        loop {
            let msg = recv(sock)?;
            session.on_msg(r, msg)?;
            if session.finished() {
                break;
            }
        }
    } else {
        loop {
            let msg = recv(sock)?;
            session.on_msg(r, msg)?;
            if session.finished() {
                break;
            }
        }
        for m in &ours {
            send(sock, m)?;
        }
    }
    Ok(())
}

/// Dial `url` (e.g. `ws://127.0.0.1:7171`) and run one full sync session.
pub fn sync_with<E: Engine, L: LogStore>(
    url: &str,
    r: &mut Replica<E, L>,
    schema: u32,
) -> Result<(), SodError> {
    let (mut sock, _resp) = tungstenite::connect(url).map_err(io_err)?;
    let result = run_session(&mut sock, r, schema, true);
    let _ = sock.close(None);
    result
}

/// Accept sync sessions on `addr` (e.g. `127.0.0.1:7171`), one at a time.
///
/// With `max_sessions: Some(n)` returns after `n` sessions (tests, one-shot
/// serving); with `None` loops forever. A failed session is logged to
/// stderr and does not stop the loop — the peer simply retries.
pub fn serve<E: Engine, L: LogStore>(
    addr: &str,
    r: &mut Replica<E, L>,
    schema: u32,
    max_sessions: Option<usize>,
) -> Result<(), SodError> {
    let listener = TcpListener::bind(addr).map_err(io_err)?;
    let mut done = 0usize;
    for stream in listener.incoming() {
        let stream: TcpStream = stream.map_err(io_err)?;
        match tungstenite::accept(stream) {
            Ok(mut sock) => {
                if let Err(e) = run_session(&mut sock, r, schema, false) {
                    eprintln!("sod: sync session failed: {e}");
                }
                let _ = sock.close(None);
            }
            Err(e) => eprintln!("sod: websocket handshake failed: {e}"),
        }
        done += 1;
        if Some(done) == max_sessions {
            break;
        }
    }
    Ok(())
}
