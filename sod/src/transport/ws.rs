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
use crate::sync::{Msg, Session, SyncReport};
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
) -> Result<SyncReport, SodError> {
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
    Ok(session.report())
}

/// Dial `url` (e.g. `ws://127.0.0.1:7171`) and run one full sync session.
///
/// The report identifies the peer and carries any per-origin refusals the
/// session recorded while continuing (SOD-2) — surface a non-empty
/// `skipped` to the user: swallowing it hides that some feed silently
/// stopped replicating.
pub fn sync_with<E: Engine, L: LogStore>(
    url: &str,
    r: &mut Replica<E, L>,
    schema: u32,
) -> Result<SyncReport, SodError> {
    let (mut sock, _resp) = tungstenite::connect(url).map_err(io_err)?;
    let result = run_session(&mut sock, r, schema, true);
    let _ = sock.close(None);
    result
}

/// A bound sync listener. Owns **no replica** — hosts embedding sod in a
/// live server accept on a dedicated thread and borrow the replica only
/// per session (sessions are milliseconds), so writes and syncs interleave
/// on one lock without ever holding it while idle.
pub struct SyncListener {
    listener: TcpListener,
}

impl SyncListener {
    /// Bind `addr` (e.g. `127.0.0.1:7300`, or port `0` for ephemeral).
    pub fn bind(addr: &str) -> Result<Self, SodError> {
        Ok(SyncListener {
            listener: TcpListener::bind(addr).map_err(io_err)?,
        })
    }

    /// The actually-bound address (resolves port `0`).
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, SodError> {
        self.listener.local_addr().map_err(io_err)
    }

    /// Block until a peer connects **and** completes the websocket
    /// handshake. Still owns no replica.
    pub fn accept(&self) -> Result<IncomingSession, SodError> {
        let (stream, _addr) = self.listener.accept().map_err(io_err)?;
        let sock = tungstenite::accept(stream)
            .map_err(|e| SodError::Io(format!("websocket handshake failed: {e}")))?;
        Ok(IncomingSession { sock })
    }
}

/// A handshaken inbound connection, waiting for its session to run.
pub struct IncomingSession {
    sock: WebSocket<TcpStream>,
}

impl IncomingSession {
    /// Run the whole session as responder; the replica is borrowed only
    /// for this call. Closes the socket on exit either way.
    pub fn run<E: Engine, L: LogStore>(
        mut self,
        r: &mut Replica<E, L>,
        schema: u32,
    ) -> Result<SyncReport, SodError> {
        let result = run_session(&mut self.sock, r, schema, false);
        let _ = self.sock.close(None);
        result
    }
}

/// Accept sync sessions on `addr` (e.g. `127.0.0.1:7171`), one at a time.
///
/// With `max_sessions: Some(n)` returns after `n` **completed** sessions
/// (tests, one-shot serving); with `None` loops forever. Failed handshakes
/// and failed sessions are logged to stderr and do not count — a stray TCP
/// probe must not use up a one-shot serve. Per-origin refusals recorded by
/// completed sessions are logged to stderr.
///
/// This holds `r` exclusively for the whole loop; hosts that also take
/// writes should use [`SyncListener`] directly instead.
pub fn serve<E: Engine, L: LogStore>(
    addr: &str,
    r: &mut Replica<E, L>,
    schema: u32,
    max_sessions: Option<usize>,
) -> Result<(), SodError> {
    let listener = SyncListener::bind(addr)?;
    let mut done = 0usize;
    let mut consecutive_accept_errors = 0usize;
    loop {
        match listener.accept() {
            Ok(incoming) => {
                consecutive_accept_errors = 0;
                match incoming.run(r, schema) {
                    Ok(report) => {
                        for s in &report.skipped {
                            eprintln!("sod: refused during sync: {s}");
                        }
                        done += 1;
                    }
                    Err(e) => eprintln!("sod: sync session failed: {e}"),
                }
            }
            Err(e) => {
                // failed handshakes (stray probes) must not stop serving,
                // but a persistently broken listener must not spin forever
                eprintln!("sod: {e}");
                consecutive_accept_errors += 1;
                if consecutive_accept_errors >= 32 {
                    return Err(e);
                }
            }
        }
        if Some(done) == max_sessions {
            return Ok(());
        }
    }
}
