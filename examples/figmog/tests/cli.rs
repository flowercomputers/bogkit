#![recursion_limit = "256"]

mod common;

use assert_cmd::Command;

/// Materialize fixture_v1 into a DB via `pull --from-file` and return the
/// (tempdir, db-arg) pair every read command needs.
fn fixture_db() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let response = dir.path().join("resp.json");
    std::fs::write(
        &response,
        serde_json::to_string(&common::fixture_v1()).unwrap(),
    )
    .unwrap();
    let db = dir.path().join("db").display().to_string();
    Command::cargo_bin("figmog")
        .unwrap()
        .args([
            "pull",
            "--from-file",
            response.to_str().unwrap(),
            "--db",
            &db,
        ])
        .assert()
        .success();
    (dir, db)
}

#[test]
fn pull_from_file_reports_churn_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let response = dir.path().join("resp.json");
    std::fs::write(
        &response,
        serde_json::to_string(&common::fixture_v1()).unwrap(),
    )
    .unwrap();
    let db = dir.path().join("db").display().to_string();

    let out = Command::cargo_bin("figmog")
        .unwrap()
        .args([
            "pull",
            "--from-file",
            response.to_str().unwrap(),
            "--db",
            &db,
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["added"], 18);

    let out = Command::cargo_bin("figmog")
        .unwrap()
        .args([
            "pull",
            "--from-file",
            response.to_str().unwrap(),
            "--db",
            &db,
            "--json",
        ])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["unchanged"], 18);
    assert_eq!(v["added"], 0);
}

#[test]
fn status_pages_tree_get_find() {
    let (_dir, db) = fixture_db();
    let run = |args: &[&str]| {
        let out = Command::cargo_bin("figmog")
            .unwrap()
            .args(args)
            .args(["--db", &db, "--json"])
            .assert()
            .success();
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
    Command::cargo_bin("figmog")
        .unwrap()
        .args(["get", "99:99", "--db", &db])
        .assert()
        .failure()
        .code(1);
}

#[test]
fn search_instances_components_styles_uses_vars() {
    let (_dir, db) = fixture_db();
    let run = |args: &[&str]| {
        let out = Command::cargo_bin("figmog")
            .unwrap()
            .args(args)
            .args(["--db", &db, "--json"])
            .assert()
            .success();
        serde_json::from_slice::<serde_json::Value>(&out.get_output().stdout).unwrap()
    };

    let hits = run(&["search", "garden"]);
    assert_eq!(hits[0]["id"], "1:2");
    assert!(hits[0]["score"].as_f64().unwrap() > 0.0);

    // by node id, by key, by set name (=> all variants' instances)
    for target in ["2:2", "key22", "Button"] {
        let inst = run(&["instances", target]);
        assert_eq!(inst[0]["id"], "1:3", "target={target}");
    }

    let comps = run(&["components"]);
    let sets = comps["sets"].as_array().unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0]["name"], "Button");
    assert_eq!(sets[0]["variants"].as_array().unwrap().len(), 2);
    let axes = &sets[0]["property_definitions"];
    assert_eq!(
        axes["Size"]["variantOptions"],
        serde_json::json!(["Large", "Small"])
    );
    assert_eq!(comps["components"].as_array().unwrap().len(), 1); // standalone only
    assert_eq!(comps["components"][0]["name"], "IconStar");

    let styles = run(&["styles"]);
    assert_eq!(styles.as_array().unwrap().len(), 2);
    assert_eq!(styles[0]["style_id"], "S:1");
    assert_eq!(styles[0]["uses"], 1);

    let styles = run(&["styles", "--values"]);
    assert_eq!(styles[1]["value"]["fontSize"], 32.0); // S:2 from consumer 1:2

    let uses = run(&["uses", "S:1"]);
    assert_eq!(uses[0]["id"], "1:1");
    let uses = run(&["uses", "VariableID:100"]);
    assert_eq!(uses[0]["id"], "1:1");

    let vars = run(&["vars"]);
    let arr = vars.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["variable_id"], "VariableID:100");
    assert_eq!(arr[0]["source"], "inferred");
}

#[test]
fn import_variables_upgrades_vars_to_authoritative() {
    let (dir, db) = fixture_db();
    let export = dir.path().join("vars.json");
    std::fs::write(&export, include_str!("fixtures/variables-export.json")).unwrap();

    Command::cargo_bin("figmog")
        .unwrap()
        .args(["import-variables", export.to_str().unwrap(), "--db", &db])
        .assert()
        .success();

    let out = Command::cargo_bin("figmog")
        .unwrap()
        .args(["vars", "--db", &db, "--json"])
        .assert()
        .success();
    let vars: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let v100 = vars
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["variable_id"] == "VariableID:100")
        .unwrap();
    assert_eq!(v100["source"], "imported");
    assert_eq!(v100["name"], "color/surface/primary");
    assert_eq!(v100["collection"], "colors");
    assert_eq!(v100["values_by_mode"]["light"]["r"], 0.06);
    // inference detail still present alongside
    assert_eq!(v100["sites"][0][0], "1:1");
}
