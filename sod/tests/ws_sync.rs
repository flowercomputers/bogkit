//! End-to-end websocket sync: two replicas on real sockets converge.
#![cfg(feature = "ws")]

use std::time::Duration;

use sod::engine::MemEngine;
use sod::store::MemLog;
use sod::transport::ws::{serve, sync_with};
use sod::{Replica, ReplicaId};

const ADDR: &str = "127.0.0.1:47163";
const SCHEMA: u32 = 1;

fn replica(b: u8) -> Replica<MemEngine, MemLog> {
    Replica::open(ReplicaId([b; 16]), MemLog::new(), MemEngine::new()).unwrap()
}

#[test]
fn two_processes_converge() {
    let mut server = replica(1);
    server.commit(vec![(b"served".to_vec(), 2)], 10).unwrap();
    server.commit(vec![(b"also".to_vec(), 1)], 30).unwrap();

    let handle = std::thread::spawn(move || {
        serve(ADDR, &mut server, SCHEMA, Some(1)).unwrap();
        server
    });

    let mut client = replica(2);
    client.commit(vec![(b"dialed".to_vec(), -1)], 20).unwrap();

    // the server thread may not be listening yet: retry briefly
    let mut attempts = 0;
    loop {
        match sync_with(&format!("ws://{ADDR}"), &mut client, SCHEMA) {
            Ok(skipped) => {
                assert!(skipped.is_empty(), "clean sync must skip nothing");
                break;
            }
            Err(e) => {
                attempts += 1;
                assert!(attempts < 50, "could not sync: {e}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let server = handle.join().unwrap();

    assert_eq!(client.vector(), server.vector());
    assert_eq!(client.engine().view_bytes(), server.engine().view_bytes());
    assert_eq!(client.watermark(), 30);
    assert_eq!(client.engine().count(b"served"), 2);
    assert_eq!(server.engine().count(b"dialed"), -1);
}

const ADDR2: &str = "127.0.0.1:47164";

#[test]
fn stray_connection_does_not_consume_one_shot_serve() {
    // A TCP probe that never completes the websocket handshake must not
    // count toward max_sessions — only completed sessions do.
    let mut server = replica(3);
    server.commit(vec![(b"payload".to_vec(), 1)], 5).unwrap();
    let handle = std::thread::spawn(move || {
        serve(ADDR2, &mut server, SCHEMA, Some(1)).unwrap();
        server
    });

    // stray probe: raw TCP, garbage bytes, hang up
    let mut attempts = 0;
    loop {
        match std::net::TcpStream::connect(ADDR2) {
            Ok(mut junk) => {
                use std::io::Write;
                let _ = junk.write_all(b"not a websocket handshake\r\n\r\n");
                drop(junk);
                break;
            }
            Err(_) => {
                attempts += 1;
                assert!(attempts < 50, "server never came up");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    // the real session must still be served
    let mut client = replica(4);
    let mut attempts = 0;
    loop {
        match sync_with(&format!("ws://{ADDR2}"), &mut client, SCHEMA) {
            Ok(_) => break,
            Err(e) => {
                attempts += 1;
                assert!(attempts < 50, "could not sync after stray probe: {e}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let server = handle.join().unwrap();
    assert_eq!(client.vector(), server.vector());
    assert_eq!(client.engine().count(b"payload"), 1);
}
