use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start(data: &Path, socket: &Path) -> Worker {
    Worker(
        Command::new(env!("CARGO_BIN_EXE_bog-records-worker"))
            .args([
                "--data-dir",
                data.to_str().unwrap(),
                "--socket",
                socket.to_str().unwrap(),
                "--template-version",
                "records-v1",
            ])
            .env("BOG_INSTANCE_ID", "test-instance")
            .env("BOG_STARTUP_NONCE", "private-test-nonce")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn request(socket: &Path, method: &str, path: &str, body: Value) -> (u16, Value) {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let body = if method == "GET" {
        String::new()
    } else {
        body.to_string()
    };
    let wire = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(wire.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap_or(Value::String(body.into())),
    )
}
fn ready(worker: &mut Worker, socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            worker.0.try_wait().unwrap().is_none(),
            "worker exited before ready"
        );
        if UnixStream::connect(socket).is_ok() {
            assert_eq!(
                request(socket, "GET", "/_cloud/identity", Value::Null).1["template_version"],
                "records-v1"
            );
            return;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn wait(worker: &mut Worker) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(status) = worker.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn paths() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let socket = dir.path().join("worker.sock");
    (dir, data, socket)
}
#[test]
fn acknowledged_nested_records_survive_kill_and_restart() {
    let (_dir, data, socket) = paths();
    let mut worker = start(&data, &socket);
    ready(&mut worker, &socket);
    let record = json!({"nested":{"array":[null,1,true,{"s":"hello"}]}});
    assert_eq!(request(&socket, "PUT", "/docs/123", record.clone()).0, 200);
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    let mut worker = start(&data, &socket);
    ready(&mut worker, &socket);
    assert_eq!(
        request(&socket, "GET", "/docs/123", Value::Null).1["data"],
        record
    );
    assert_eq!(
        request(
            &socket,
            "POST",
            "/_cloud/shutdown",
            json!({"nonce":"wrong"})
        )
        .0,
        403
    );
    assert_eq!(
        request(
            &socket,
            "POST",
            "/_cloud/shutdown",
            json!({"nonce":"private-test-nonce"})
        )
        .0,
        200
    );
    wait(&mut worker);
    assert!(!socket.exists());
    let reopened = bog_cloud_records::records_service(&data).unwrap();
    reopened.shutdown().unwrap();
}
#[test]
fn competing_worker_cannot_unlink_live_socket_and_sigterm_releases_store() {
    let (_dir, data, socket) = paths();
    let mut first = start(&data, &socket);
    ready(&mut first, &socket);
    let mut second = start(&data, &socket);
    assert!(!second.0.wait().unwrap().success());
    assert_eq!(
        request(&socket, "PUT", "/docs/first", json!({"value":1})).0,
        200
    );
    assert!(
        Command::new("kill")
            .args(["-TERM", &first.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    wait(&mut first);
    let mut third = start(&data, &socket);
    ready(&mut third, &socket);
    assert_eq!(request(&socket, "GET", "/docs/first", Value::Null).0, 200);
    request(
        &socket,
        "POST",
        "/_cloud/shutdown",
        json!({"nonce":"private-test-nonce"}),
    );
    wait(&mut third);
}
#[test]
fn unrelated_files_and_sockets_are_not_removed() {
    let (_dir, data, socket) = paths();
    std::fs::write(&socket, "unrelated").unwrap();
    let mut worker = start(&data, &socket);
    assert!(!worker.0.wait().unwrap().success());
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "unrelated");
    std::fs::remove_file(&socket).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut worker = start(&data, &socket);
    assert!(!worker.0.wait().unwrap().success());
    assert!(socket.exists());
}
#[test]
fn schema_guard_io_failures_refuse_startup() {
    let (_dir, data, _socket) = paths();
    let marker = PathBuf::from(format!("{}.schema", data.display()));
    std::fs::create_dir(&marker).unwrap();
    assert!(bog_cloud_records::records_service(&data).is_err());
    std::fs::remove_dir(&marker).unwrap();
    // A dangling symlink reads as NotFound, then writing through it must fail.
    std::os::unix::fs::symlink(data.join("missing-parent/marker"), &marker).unwrap();
    assert!(bog_cloud_records::records_service(&data).is_err());
}
#[test]
fn dot_segments_are_rejected_in_document_and_batch_paths() {
    let (_dir, data, socket) = paths();
    let mut worker = start(&data, &socket);
    ready(&mut worker, &socket);
    for key in [".", ".."] {
        assert_eq!(
            request(&socket, "PUT", &format!("/docs/{key}"), json!({"x":1})).0,
            400
        );
        assert_eq!(
            request(
                &socket,
                "POST",
                "/batch",
                json!([{"op":"upsert","key":key,"data":{"x":1}}])
            )
            .0,
            400
        );
    }
    request(
        &socket,
        "POST",
        "/_cloud/shutdown",
        json!({"nonce":"private-test-nonce"}),
    );
    wait(&mut worker);
}
#[test]
fn shutdown_does_not_remove_a_replacement_socket_path() {
    let (dir, data, socket) = paths();
    let mut worker = start(&data, &socket);
    ready(&mut worker, &socket);
    let moved = dir.path().join("moved.sock");
    std::fs::rename(&socket, &moved).unwrap();
    std::fs::write(&socket, "replacement").unwrap();
    request(
        &moved,
        "POST",
        "/_cloud/shutdown",
        json!({"nonce":"private-test-nonce"}),
    );
    wait(&mut worker);
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "replacement");
    assert!(moved.exists());
}

#[test]
fn sigterm_during_requests_preserves_every_acknowledged_write() {
    let (_dir, data, socket) = paths();
    let mut worker = start(&data, &socket);
    ready(&mut worker, &socket);
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    let writer_socket = socket.clone();
    let writer = std::thread::spawn(move || {
        for n in 0..100 {
            let Ok(mut stream) = UnixStream::connect(&writer_socket) else {
                break;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let body = json!({"n":n}).to_string();
            if write!(stream, "PUT /docs/{n} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).is_err() {break}
            let mut response = String::new();
            if stream.read_to_string(&mut response).is_err() {
                break;
            }
            if response.starts_with("HTTP/1.1 200 ") {
                ack_tx.send(n).unwrap()
            } else {
                break;
            }
        }
    });
    let first = ack_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        Command::new("kill")
            .args(["-TERM", &worker.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    writer.join().unwrap();
    wait(&mut worker);
    let acknowledged = std::iter::once(first)
        .chain(ack_rx.try_iter())
        .collect::<Vec<_>>();
    let mut reopened = start(&data, &socket);
    ready(&mut reopened, &socket);
    for n in acknowledged {
        assert_eq!(
            request(&socket, "GET", &format!("/docs/{n}"), Value::Null).1["data"]["n"],
            n
        );
    }
    request(
        &socket,
        "POST",
        "/_cloud/shutdown",
        json!({"nonce":"private-test-nonce"}),
    );
    wait(&mut reopened);
}

#[test]
fn unfinished_request_cannot_hold_worker_beyond_shutdown_deadline() {
    let (_dir, data, socket) = paths();
    let mut worker = start(&data, &socket);
    ready(&mut worker, &socket);
    let mut unfinished = UnixStream::connect(&socket).unwrap();
    unfinished.write_all(b"PUT /docs/slow HTTP/1.1\r\nHost: worker\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        request(
            &socket,
            "POST",
            "/_cloud/shutdown",
            json!({"nonce":"private-test-nonce"})
        )
        .0,
        200
    );
    let deadline = Instant::now() + Duration::from_secs(13);
    loop {
        if let Some(status) = worker.0.try_wait().unwrap() {
            assert!(
                !status.success(),
                "unfinished request must not claim clean shutdown"
            );
            assert!(
                socket.exists(),
                "failed drain must leave its owned socket marker"
            );
            break;
        }
        assert!(Instant::now() < deadline, "shutdown exceeded hard deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
}
