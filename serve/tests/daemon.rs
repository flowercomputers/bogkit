//! Daemon-mode behavior: schema-drift handling, unix-socket serving with
//! the discovery sidecar, and idle shutdown (exercised in a child process,
//! since a graceful idle exit terminates the process).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bog_serve::{App, Bind, SchemaDrift, sidecar_path};
use fold::pipeline::terminal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod common;
use common::send;

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct Entry {
    id: u64,
    text: String,
}

fn tmp() -> PathBuf {
    tempfile::tempdir().unwrap().keep()
}

fn entry(id: u64, text: &str) -> Value {
    json!({ "id": id, "text": text })
}

#[tokio::test]
async fn schema_drift_panics_by_default() {
    let db = tmp().join("db");
    drop(
        App::stream(
            &db,
            (
                terminal::Count::new("total"),
                terminal::Bag::<Entry>::new("entries"),
            ),
        )
        .into_router(),
    );

    // same data dir, different sink structure => fingerprint mismatch
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        App::<Entry, _>::stream(&db, terminal::Bag::<Entry>::new("entries")).into_router()
    }));
    assert!(panicked.is_err());
}

#[tokio::test]
async fn schema_drift_wipe_and_rebuild() {
    let db = tmp().join("db");
    {
        let router = App::stream(
            &db,
            (
                terminal::Count::new("total"),
                terminal::Bag::<Entry>::new("entries"),
            ),
        )
        .into_router();
        send(&router, "POST", "/insert", Some(entry(1, "peat"))).await;
        send(&router, "POST", "/insert", Some(entry(2, "moss"))).await;
    }

    // drop the bag sink but keep "total": without the wipe, the old count
    // of 2 would still be visible through the same-named sink
    let router = App::<Entry, _>::stream(&db, terminal::Count::new("total"))
        .on_schema_drift(SchemaDrift::WipeAndRebuild)
        .into_router();
    let (status, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["data"]["value"], 0);

    let (_, body) = send(&router, "POST", "/insert", Some(entry(3, "fen"))).await;
    assert_eq!(body["seq"], 1);
    drop(router);

    // the marker was rewritten: the new pipeline reopens fine in Panic mode
    let router = App::<Entry, _>::stream(&db, terminal::Count::new("total")).into_router();
    let (_, body) = send(&router, "GET", "/views/total", None).await;
    assert_eq!(body["data"]["value"], 1);
}

/// One blocking HTTP/1.1 request over a unix socket, returning the raw
/// response text.
fn uds_get(sock: &Path, path: &str) -> String {
    let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    buf
}

fn wait_for(what: &str, timeout: Duration, mut ready: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ready() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn uds_serving_and_sidecar() {
    let dir = tmp();
    let db = dir.join("db");
    let sock = dir.join("serve.sock");
    let (db2, sock2) = (db.clone(), sock.clone());
    std::thread::spawn(move || {
        App::stream(&db2, terminal::Bag::<Entry>::new("entries"))
            .bind(Bind::Unix(sock2))
            .run()
    });
    wait_for("socket", Duration::from_secs(10), || sock.exists());

    let resp = uds_get(&sock, "/healthz");
    assert!(resp.starts_with("HTTP/1.1 200"), "response: {resp}");

    let sidecar: Value =
        serde_json::from_str(&std::fs::read_to_string(sidecar_path(&db)).unwrap()).unwrap();
    assert_eq!(sidecar["pid"], std::process::id());
    assert_eq!(sidecar["bind"]["unix"], sock.to_str().unwrap());
    assert!(sidecar["fingerprint"].is_string());
    assert!(sidecar["version"].is_string());
}

/// Not a test of its own: the daemon half of `idle_exit_...`, run in a
/// child process because a clean idle shutdown exits the process. Without
/// the env var (the normal test run) it's a no-op.
#[test]
fn idle_exit_child() {
    let Ok(db) = std::env::var("BOG_DAEMON_TEST_DB") else {
        return;
    };
    App::stream(&db, terminal::Bag::<Entry>::new("entries"))
        .bind(Bind::Unix(PathBuf::from(format!("{db}.sock"))))
        .idle_timeout(Duration::from_secs(3))
        .run();
    unreachable!("the idle timeout should have exited the process");
}

#[test]
fn idle_exit_checkpoints_and_cleans_up() {
    let dir = tmp();
    let db = dir.join("db");
    let sock = PathBuf::from(format!("{}.sock", db.display()));

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["idle_exit_child", "--exact", "--nocapture"])
        .env("BOG_DAEMON_TEST_DB", &db)
        .spawn()
        .unwrap();

    wait_for("child socket", Duration::from_secs(15), || sock.exists());
    let resp = uds_get(&sock, "/healthz");
    assert!(resp.starts_with("HTTP/1.1 200"), "response: {resp}");

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "child did not exit; killing"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "idle exit status: {status}");
    assert!(!sock.exists(), "socket file not cleaned up");
    assert!(
        !sidecar_path(&db).exists(),
        "discovery sidecar not cleaned up"
    );
}
