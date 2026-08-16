#![recursion_limit = "256"]

//! End-to-end test of `figmog serve`: spawns the real compiled binary as a
//! child process, drives it over stdin/stdout exactly as an MCP client
//! would, and asserts on the JSON-RPC frames it writes back. Everything
//! else in this crate tests the pieces (`mcp::handle_message` unit tests,
//! CLI smoke tests over `query::*`); this is the one test proving the
//! pieces are wired together correctly in the real process, including the
//! stdin-EOF exit contract `--no-watch` mode relies on.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Generous but bounded: every wait in this test — for a response line or
/// for the child to exit — is capped at this, so a regression that makes
/// the server hang fails the test instead of the test run.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Kills the child on drop so a failed assertion (which unwinds past the
/// rest of the test body, skipping the normal stdin-close/wait sequence)
/// never leaves an orphaned `figmog serve` process behind.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `figmog serve --no-watch --db <db>` with piped stdio. Returns the
/// kill-on-drop guard, a writer for stdin, and a channel of stdout lines
/// fed by a reader thread — driving the child through a channel (rather
/// than reading its stdout inline) means a hung child blocks only the
/// bounded `recv_timeout` in [`recv`], never the test thread itself.
fn spawn_serve(db: &std::path::Path) -> (ChildGuard, ChildStdin, Receiver<String>) {
    let bin = assert_cmd::cargo::cargo_bin("figmog");
    let mut child = Command::new(bin)
        .args(["serve", "--no-watch", "--db"])
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn figmog serve");

    let stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let stderr = child.stderr.take().expect("child stderr");

    // Drain stderr on its own thread purely for debugging visibility
    // (`serve` logs there, e.g. "figmog serving ..."); never asserted on.
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("[figmog serve stderr] {line}");
        }
    });

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    (ChildGuard(child), stdin, rx)
}

/// Write one JSON-RPC frame, newline-delimited (the protocol this crate's
/// `mcp`/`serve` modules speak).
fn send(stdin: &mut ChildStdin, msg: &Value) {
    writeln!(stdin, "{msg}").expect("write to child stdin");
    stdin.flush().expect("flush child stdin");
}

/// Read and parse the next response line, bounded by [`TIMEOUT`] so a
/// stuck server fails this assertion instead of hanging the test binary.
fn recv(rx: &Receiver<String>) -> Value {
    let line = rx
        .recv_timeout(TIMEOUT)
        .expect("figmog serve did not respond within the timeout");
    serde_json::from_str(&line)
        .unwrap_or_else(|e| panic!("response line was not valid JSON: {e}\nline: {line}"))
}

/// Poll `try_wait` instead of a single blocking `wait()`, so a child that
/// never exits fails with a clear panic at `timeout` rather than hanging
/// the test run forever.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            panic!("figmog serve did not exit within {timeout:?} of stdin EOF");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn call(stdin: &mut ChildStdin, rx: &Receiver<String>, id: i64, name: &str, args: Value) -> Value {
    send(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        }),
    );
    recv(rx)
}

/// The tool result's text content, parsed as JSON (every `figmog_*` tool
/// returns `query::*` JSON serialized as the single text content block).
fn result_json(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content in: {resp}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("content text not JSON: {e}\ntext: {text}"))
}

#[test]
fn serve_e2e_initialize_tools_list_and_tool_calls() {
    let (_dir, db) = common::fixture_db();
    let (mut guard, mut stdin, rx) = spawn_serve(&db);

    // -- initialize --
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {}},
        }),
    );
    let resp = recv(&rx);
    assert_eq!(resp["id"], json!(1));
    assert_eq!(resp["result"]["serverInfo"]["name"], json!("figmog"));
    let instructions = resp["result"]["instructions"]
        .as_str()
        .expect("instructions is a string");
    assert!(!instructions.is_empty());
    assert!(
        instructions.contains("official Figma MCP"),
        "instructions should mention the official Figma MCP: {instructions}"
    );

    // notifications/initialized: no `id`, so no response frame is expected
    // (mirrors a real MCP client's handshake; the server ignores it).
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );

    // -- tools/list: exactly 17 figmog_* tools --
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let resp = recv(&rx);
    let tools = resp["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 17, "tools: {tools:#?}");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for name in &names {
        assert!(
            name.starts_with("figmog_"),
            "tool outside the figmog_ namespace: {name}"
        );
    }
    for expected in ["figmog_search", "figmog_where", "figmog_sync"] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }

    // -- figmog_search: first hit is 1:2 ("Title", text "...garden") --
    let resp = call(
        &mut stdin,
        &rx,
        3,
        "figmog_search",
        json!({"query": "garden"}),
    );
    assert_eq!(resp["result"]["isError"], json!(false));
    let hits = result_json(&resp);
    assert_eq!(hits[0]["id"], json!("1:2"));

    // -- figmog_node: id normalization (12-34 form) + raw JSON name --
    let resp = call(&mut stdin, &rx, 4, "figmog_node", json!({"id": "1-2"}));
    assert_eq!(resp["result"]["isError"], json!(false));
    let node = result_json(&resp);
    assert_eq!(node["name"], json!("Title"));

    // -- figmog_where: exactly one row, id 1:1 --
    let resp = call(
        &mut stdin,
        &rx,
        5,
        "figmog_where",
        json!({"pointer": "/layoutMode", "equals": "VERTICAL"}),
    );
    assert_eq!(resp["result"]["isError"], json!(false));
    let rows = result_json(&resp);
    let rows = rows.as_array().expect("rows array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], json!("1:1"));

    // -- figmog_node on an unknown id: isError --
    let resp = call(&mut stdin, &rx, 6, "figmog_node", json!({"id": "99:99"}));
    assert_eq!(resp["result"]["isError"], json!(true));

    // -- unknown JSON-RPC method: -32601 --
    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 7, "method": "totally/bogus"}),
    );
    let resp = recv(&rx);
    assert_eq!(resp["error"]["code"], json!(-32601));

    // -- unknown tool name: isError, not a protocol-level error --
    let resp = call(&mut stdin, &rx, 8, "figmog_nonexistent", json!({}));
    assert_eq!(resp["result"]["isError"], json!(true));

    // Closing stdin is what makes the (`--no-watch`) serve loop exit: its
    // reader thread sees EOF and drops the sender, so the main loop's
    // blocking `rx.recv()` returns `Disconnected` and the process exits 0.
    drop(stdin);
    let status = wait_with_timeout(&mut guard.0, TIMEOUT);
    assert!(status.success(), "figmog serve exited with {status:?}");
}
