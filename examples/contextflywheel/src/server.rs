use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

use crate::{
    EventKind, compile_state, context_report, load_context_snapshots, load_records, mission_brief,
};

pub fn serve(root: PathBuf, bind: &str) -> Result<()> {
    let server = Server::http(bind).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    eprintln!("ContextFlywheel Time Machine: http://{bind}");
    for request in server.incoming_requests() {
        let root = root.clone();
        thread::spawn(move || {
            if let Err(error) = respond(request, &root) {
                eprintln!("web request failed: {error:#}");
            }
        });
    }
    Ok(())
}

fn respond(request: tiny_http::Request, root: &Path) -> Result<()> {
    let url = request.url().to_owned();
    let route = url.split('?').next().unwrap_or(&url);
    let selected_root = resolve_session(root, query_param(&url, "session"))?;
    if route == "/api/live" {
        let after = query_param(&url, "after")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let deadline = Instant::now() + Duration::from_secs(20);
        let record = loop {
            if let Some(record) = load_records(&selected_root)?
                .into_iter()
                .find(|record| record.step > after)
            {
                break Some(record);
            }
            if Instant::now() >= deadline {
                break None;
            }
            thread::sleep(Duration::from_millis(350));
        };
        let response = match record {
            Some(record) => Response::from_string(serde_json::to_string(&record)?)
                .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
            None => Response::from_string("").with_status_code(204),
        };
        request.respond(response)?;
        return Ok(());
    }
    let (body, content_type, status) = if route == "/" {
        (HTML.to_owned(), "text/html; charset=utf-8", 200)
    } else if route == "/api/timeline" {
        (
            serde_json::to_string(&load_records(&selected_root)?)?,
            "application/json",
            200,
        )
    } else if route == "/api/mission" {
        (
            serde_json::to_string(&render_mission(&selected_root)?)?,
            "application/json",
            200,
        )
    } else if route == "/api/sessions" {
        (
            serde_json::to_string(&render_sessions(root)?)?,
            "application/json",
            200,
        )
    } else if route == "/api/context-snapshots" {
        (
            serde_json::to_string(&load_context_snapshots(&selected_root)?)?,
            "application/json",
            200,
        )
    } else if let Some(value) = route.strip_prefix("/api/step/") {
        match value
            .parse::<u64>()
            .ok()
            .and_then(|step| render_history(&selected_root, step).ok())
        {
            Some(history) => (serde_json::to_string(&history)?, "application/json", 200),
            None => (
                "{\"error\":\"step not found\"}".into(),
                "application/json",
                404,
            ),
        }
    } else if let Some(value) = route.strip_prefix("/api/context/") {
        match value
            .parse::<u64>()
            .ok()
            .and_then(|step| render_context_report(&selected_root, step).ok())
        {
            Some(report) => (serde_json::to_string(&report)?, "application/json", 200),
            None => (
                "{\"error\":\"context unavailable\"}".into(),
                "application/json",
                404,
            ),
        }
    } else {
        ("not found".into(), "text/plain", 404)
    };
    let header = Header::from_bytes("Content-Type", content_type)
        .map_err(|_| anyhow::anyhow!("invalid content-type header"))?;
    request.respond(
        Response::from_string(body)
            .with_status_code(status)
            .with_header(header),
    )?;
    Ok(())
}

fn resolve_session(root: &Path, requested: Option<&str>) -> Result<PathBuf> {
    let Some(requested) = requested else {
        return Ok(root.to_path_buf());
    };
    let Some(parent) = root.parent() else {
        return Ok(root.to_path_buf());
    };
    for entry in std::fs::read_dir(parent)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some(requested)
            && path.join("ledger.jsonl").is_file()
        {
            return Ok(path);
        }
    }
    anyhow::bail!("unknown session")
}

fn render_sessions(root: &Path) -> Result<Vec<serde_json::Value>> {
    let Some(parent) = root.parent() else {
        return Ok(vec![]);
    };
    let mut sessions = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let path = entry?.path();
        if !path.join("ledger.jsonl").is_file() {
            continue;
        }
        let records = load_records(&path)?;
        let state = compile_state(records.iter());
        sessions.push(json!({
            "id": path.file_name().and_then(|name| name.to_str()).unwrap_or("mission"),
            "active": path == root,
            "objective": state.objective.as_ref().map(|record| record.text.as_str()),
            "current_belief": state.beliefs.values().next().map(|belief| belief.statement.as_str()),
            "event_count": records.len(),
            "completed": records.last().is_some_and(|record| record.kind == EventKind::AgentStop),
        }));
    }
    sessions.sort_by_key(|session| !session["active"].as_bool().unwrap_or(false));
    Ok(sessions)
}

fn render_mission(root: &Path) -> Result<serde_json::Value> {
    let records = load_records(root)?;
    let mut brief = mission_brief(&records);
    if let Some(step) = brief["last_step"].as_u64() {
        if let Ok(report) = context_report(&records, step) {
            if let Some(comparison) = brief.get_mut("comparison") {
                comparison["compiled_estimated_tokens"] = report["total_estimated_tokens"].clone();
                comparison["selected_count"] =
                    json!(report["selected"].as_array().map_or(0, Vec::len));
                comparison["excluded_stale_beliefs"] =
                    json!(report["excluded"].as_array().map_or(0, Vec::len));
                comparison["budget"] = report["budget"].clone();
            }
            brief["context"] = report;
        }
    }
    let experiment_path = root.join("evaluation-results.json");
    if experiment_path.exists() {
        if let Ok(value) =
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(experiment_path)?)
        {
            brief["experiment"] = value;
        }
    }
    let snapshots = load_context_snapshots(root)?;
    brief["captured_context"] = snapshots
        .last()
        .map(|snapshot| serde_json::to_value(snapshot).unwrap_or_default())
        .unwrap_or_default();
    brief["context_snapshot_count"] = json!(snapshots.len());
    Ok(brief)
}

fn render_context_report(root: &Path, step: u64) -> Result<serde_json::Value> {
    context_report(&load_records(root)?, step)
}

fn query_param<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    url.split_once('?')?.1.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key == name).then_some(value)
    })
}

fn render_history(root: &Path, step: u64) -> Result<String> {
    let records = load_records(root)?;
    let event = records
        .iter()
        .find(|record| record.step == step)
        .ok_or_else(|| anyhow::anyhow!("step not found"))?;
    let state = compile_state(records.iter().filter(|record| record.step <= step));
    let mut out = format!(
        "STEP {step}\nEvent: {:?} — {}\nTime: {}\n\nBELIEFS ACTIVE AT THIS STEP\n",
        event.kind, event.text, event.timestamp
    );
    if state.beliefs.is_empty() {
        out.push_str("- None recorded.\n");
    }
    for belief in state.beliefs.values() {
        out.push_str(&format!(
            "- {} v{}: {}\n",
            belief.key, belief.version, belief.statement
        ));
    }
    out.push_str("\nPERMISSIONS IN FORCE\n");
    out.push_str(&format!(
        "- {}\n\nLATER BELIEF CHANGES\n",
        state
            .permission
            .as_ref()
            .map_or("Not recorded", |record| record.text.as_str())
    ));
    for record in records
        .iter()
        .filter(|record| record.step > step && record.kind == EventKind::BeliefRevision)
    {
        out.push_str(&format!(
            "- Step {}: {} v{} — {}\n",
            record.step,
            record.belief_key.as_deref().unwrap_or("belief"),
            record.belief_version.unwrap_or(0),
            record.text
        ));
    }
    Ok(out)
}

const HTML: &str = include_str!("ui.html");
