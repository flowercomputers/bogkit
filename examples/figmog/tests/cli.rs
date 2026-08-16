#![recursion_limit = "256"]

mod common;

use assert_cmd::Command;

/// Materialize fixture_v1 into a DB via `pull --from-file` and return the
/// (tempdir, db-arg) pair every read command needs.
fn fixture_db() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let response = dir.path().join("resp.json");
    std::fs::write(&response, serde_json::to_string(&common::fixture_v1()).unwrap()).unwrap();
    let db = dir.path().join("db").display().to_string();
    Command::cargo_bin("figmog")
        .unwrap()
        .args(["pull", "--from-file", response.to_str().unwrap(), "--db", &db])
        .assert()
        .success();
    (dir, db)
}

#[test]
fn pull_from_file_reports_churn_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let response = dir.path().join("resp.json");
    std::fs::write(&response, serde_json::to_string(&common::fixture_v1()).unwrap()).unwrap();
    let db = dir.path().join("db").display().to_string();

    let out = Command::cargo_bin("figmog").unwrap()
        .args(["pull", "--from-file", response.to_str().unwrap(), "--db", &db, "--json"])
        .assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["added"], 18);

    let out = Command::cargo_bin("figmog").unwrap()
        .args(["pull", "--from-file", response.to_str().unwrap(), "--db", &db, "--json"])
        .assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["unchanged"], 18);
    assert_eq!(v["added"], 0);
}

#[test]
fn status_pages_tree_get_find() {
    let (_dir, db) = fixture_db();
    let run = |args: &[&str]| {
        let out = Command::cargo_bin("figmog").unwrap()
            .args(args).args(["--db", &db, "--json"])
            .assert().success();
        serde_json::from_slice::<serde_json::Value>(&out.get_output().stdout).unwrap()
    };

    let status = run(&["status"]);
    assert_eq!(status["name"], "Fixture");
    assert_eq!(status["version"], "100");
    assert_eq!(status["nodes"], 12);

    let pages = run(&["pages"]);
    assert_eq!(pages.as_array().unwrap().len(), 3);
    assert_eq!(pages[0]["id"], "0:1");
    assert_eq!(pages[0]["name"], "Page 1");

    let tree = run(&["tree", "1:1"]);
    let kids = tree["children"].as_array().unwrap();
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[0]["id"], "1:2"); // numeric child order

    // node-id normalization: URL form accepted
    let get = run(&["get", "1-2"]);
    assert_eq!(get["name"], "Title");
    assert_eq!(get["characters"], "Welcome to the garden");

    let texts = run(&["find", "--type", "TEXT"]);
    assert_eq!(texts.as_array().unwrap().len(), 1);
    assert_eq!(texts[0]["id"], "1:2");

    let on_page = run(&["find", "--type", "COMPONENT", "--page", "0:2"]);
    assert_eq!(on_page.as_array().unwrap().len(), 3); // 2:2, 2:3, 3:1
}

#[test]
fn get_unknown_node_fails_cleanly() {
    let (_dir, db) = fixture_db();
    Command::cargo_bin("figmog").unwrap()
        .args(["get", "99:99", "--db", &db])
        .assert()
        .failure()
        .code(1);
}
