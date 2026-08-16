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
            Ok(()) => break,
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
