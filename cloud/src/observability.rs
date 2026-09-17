//! Bounded, content-free diagnostics. Persistent operational events are separate
//! from the registry and in-memory request observations reset on manager restart.
use crate::{BogId, CloudError, Principal, PrincipalKind};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::Mutex,
    time::Instant,
};
const HOUR: i64 = 3600;
const MAX_REQUESTS: usize = 20000;
const RETENTION: i64 = 7 * 86400;
fn now() -> i64 {
    crate::registry::now()
}
fn unavailable() -> CloudError {
    CloudError::new("unavailable", "observability unavailable")
}
struct Request {
    at: i64,
    bog: BogId,
    credential: String,
    operation: &'static str,
    ms: f64,
    code: Option<String>,
    status: u16,
    request_id: String,
    outcome: Option<&'static str>,
    release_ms: Option<f64>,
}
#[derive(Default)]
struct Memory {
    requests: VecDeque<Request>,
    dropped_at: Option<i64>,
    active: HashMap<(BogId, String), usize>,
    writes: HashMap<BogId, (i64, u64, Instant)>,
    usage: HashMap<BogId, (i64, Value)>,
    activity: HashMap<String, i64>,
}
pub struct Observability {
    db: Mutex<Connection>,
    memory: Mutex<Memory>,
    since: i64,
}
pub struct WaitGuard<'a> {
    obs: &'a Observability,
    bog: BogId,
    credential: String,
}
impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut m) = self.obs.memory.lock() {
            let key = (self.bog, self.credential.clone());
            if let Some(n) = m.active.get_mut(&key) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    m.active.remove(&key);
                }
            }
        }
    }
}
pub fn credential(p: &Principal) -> String {
    p.token_id.clone().unwrap_or_else(|| {
        match p.kind() {
            PrincipalKind::Human => "human_session",
            _ => "operator",
        }
        .into()
    })
}
fn error_counts(rows: &[&Request]) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for row in rows {
        if let Some(code) = row.code.as_deref() {
            *counts.entry(code.to_owned()).or_default() += 1;
        }
    }
    counts
}
fn status_counts(rows: &[&Request]) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for r in rows {
        *counts.entry(r.status.to_string()).or_default() += 1;
    }
    counts
}
fn distribution(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    let p = |q: f64| {
        if values.is_empty() {
            Value::Null
        } else {
            json!(values[((values.len() as f64 * q).ceil() as usize).saturating_sub(1)])
        }
    };
    json!({"p50":p(0.5),"p95":p(0.95),"p99":p(0.99),"sample_count":values.len(),"method":"recent_samples_nearest_rank"})
}
impl Observability {
    pub fn open(root: &Path) -> Result<Self, CloudError> {
        let path = root.join("observability.sqlite");
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(unavailable());
        }
        let db = Connection::open(path).map_err(|_| unavailable())?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA max_page_count=8192; CREATE TABLE IF NOT EXISTS metadata(epoch TEXT NOT NULL); CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT,bog TEXT NOT NULL,at INTEGER NOT NULL,kind TEXT NOT NULL,credential TEXT,reason TEXT); CREATE INDEX IF NOT EXISTS events_bog ON events(bog,id); CREATE TABLE IF NOT EXISTS activity(credential TEXT PRIMARY KEY,last_used_at INTEGER NOT NULL); ").map_err(|_|unavailable())?;
        db.execute(
            "INSERT INTO metadata(epoch) SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM metadata)",
            [uuid::Uuid::new_v4().to_string()],
        )
        .map_err(|_| unavailable())?;
        db.busy_timeout(std::time::Duration::from_millis(50))
            .map_err(|_| unavailable())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join("observability.sqlite"),
            std::fs::Permissions::from_mode(0o600),
        )
        .map_err(|_| unavailable())?;
        Ok(Self {
            db: Mutex::new(db),
            memory: Mutex::new(Memory::default()),
            since: now(),
        })
    }
    pub fn event(&self, bog: BogId, kind: &str, credential: Option<&str>, reason: Option<&str>) {
        let Ok(db) = self.db.lock() else { return };
        let _ = db.execute(
            "INSERT OR IGNORE INTO events(bog,at,kind,credential,reason) VALUES(?1,?2,?3,?4,?5)",
            params![
                bog.to_string(),
                now(),
                kind.chars().take(64).collect::<String>(),
                credential.map(|s| s.chars().take(64).collect::<String>()),
                reason.map(|s| s.chars().take(64).collect::<String>())
            ],
        );
        let _=db.execute("DELETE FROM events WHERE at<?1 OR id IN (SELECT id FROM events WHERE bog=?2 ORDER BY id DESC LIMIT -1 OFFSET 1000) OR id IN (SELECT id FROM events ORDER BY id DESC LIMIT -1 OFFSET 20000)",params![now()-RETENTION,bog.to_string()]);
    }
    pub fn worker_metadata(&self, bog: BogId) -> Value {
        let Ok(db) = self.db.lock() else {
            return json!({});
        };
        let last:Option<(i64,String,Option<String>)>=db.query_row("SELECT at,kind,reason FROM events WHERE bog=?1 AND kind IN ('worker_starting','worker_running','worker_failed','worker_exited','worker_sleeping','worker_stopped') ORDER BY id DESC LIMIT 1",[bog.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().ok().flatten();
        last.map(|(at,kind,reason)|json!({"last_transition_at":at,"last_transition":kind,"last_transition_reason":reason})).unwrap_or(json!({}))
    }
    pub fn touch(&self, p: &Principal) {
        let Some(id) = p.token_id.as_deref() else {
            return;
        };
        let minute = now() / 60 * 60;
        if let Ok(mut memory) = self.memory.lock() {
            if memory.activity.get(id) == Some(&minute) {
                return;
            }
            if memory.activity.len() >= 20000 {
                memory.activity.retain(|_, at| *at >= minute);
                if memory.activity.len() >= 20000 {
                    return;
                }
            }
            memory.activity.insert(id.to_owned(), minute);
        } else {
            return;
        }
        if let Ok(db) = self.db.lock() {
            let _=db.execute("INSERT INTO activity(credential,last_used_at) VALUES(?1,?2) ON CONFLICT(credential) DO UPDATE SET last_used_at=excluded.last_used_at WHERE activity.last_used_at<excluded.last_used_at",params![id,minute]);
            let _=db.execute("DELETE FROM activity WHERE credential IN (SELECT credential FROM activity ORDER BY last_used_at DESC LIMIT -1 OFFSET 20000)",[]);
        }
    }
    pub fn merge_activity(&self, tokens: &mut Value) {
        let db = self.db.lock().ok();
        if let Some(tokens) = tokens.as_array_mut() {
            for token in tokens {
                let at = db.as_ref().and_then(|db| {
                    db.query_row(
                        "SELECT last_used_at FROM activity WHERE credential=?1",
                        [token["id"].as_str().unwrap_or("")],
                        |r| r.get::<_, i64>(0),
                    )
                    .optional()
                    .ok()
                    .flatten()
                });
                token["last_used_at"] = json!(at);
            }
        }
    }
    pub fn active_wait(&self, bog: BogId, p: &Principal) -> WaitGuard<'_> {
        let credential = credential(p);
        if let Ok(mut m) = self.memory.lock() {
            *m.active.entry((bog, credential.clone())).or_default() += 1;
        }
        WaitGuard {
            obs: self,
            bog,
            credential,
        }
    }
    pub fn write_ack(&self, bog: BogId, generation: i64, seq: u64) {
        if let Ok(mut m) = self.memory.lock()
            && (m.writes.len() < 4096 || m.writes.contains_key(&bog))
        {
            m.writes.insert(bog, (generation, seq, Instant::now()));
        }
    }
    pub fn cache_usage(&self, bog: BogId, usage: Value) {
        if let Ok(mut m) = self.memory.lock()
            && (m.usage.len() < 4096 || m.usage.contains_key(&bog))
        {
            let usage = json!({"logical_bytes":usage["logical_bytes"].as_u64(),"limit_bytes":usage["limit_bytes"].as_u64(),"over_limit":usage["over_limit"].as_bool()});
            m.usage.insert(bog, (now(), usage));
        }
    }
    pub fn record(
        &self,
        bog: BogId,
        p: &Principal,
        operation: &'static str,
        start: Instant,
        request_id: &str,
        result: &Result<crate::OperationResult, CloudError>,
    ) {
        let Ok(mut m) = self.memory.lock() else {
            return;
        };
        let at = now();
        while m.requests.front().is_some_and(|r| r.at < at - HOUR) {
            m.requests.pop_front();
        }
        let key_credential = credential(p);
        let present = m
            .requests
            .iter()
            .any(|r| r.bog == bog && r.credential == key_credential && r.operation == operation);
        if !present {
            let series: std::collections::HashSet<_> = m
                .requests
                .iter()
                .map(|r| (r.bog, &r.credential, r.operation))
                .collect();
            if series.len() >= 512 {
                m.dropped_at = Some(at);
                return;
            }
        }
        if m.requests.len() >= MAX_REQUESTS {
            m.requests.pop_front();
            m.dropped_at = Some(at);
        }
        let outcome = if operation == "wait_for_change" {
            result.as_ref().ok().map(|r| {
                if r.body["reset"] == true {
                    "reset"
                } else if r.body["changed"] == true {
                    "changed"
                } else {
                    "timeout"
                }
            })
        } else {
            None
        };
        let release_ms = if outcome == Some("changed") {
            result
                .as_ref()
                .ok()
                .and_then(|r| r.body["seq"].as_u64())
                .and_then(|seq| {
                    m.writes
                        .get(&bog)
                        .filter(|(g, s, t)| {
                            *s == seq
                                && *t >= start
                                && result
                                    .as_ref()
                                    .ok()
                                    .and_then(|r| r.body["cursor"].as_str())
                                    .and_then(|c| URL_SAFE_NO_PAD.decode(c).ok())
                                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                                    .and_then(|v| v["generation"].as_i64())
                                    == Some(*g)
                        })
                        .map(|(_, _, t)| t.elapsed().as_secs_f64() * 1000.)
                })
        } else {
            None
        };
        let request_id = uuid::Uuid::parse_str(request_id)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "unavailable".into());
        m.requests.push_back(Request {
            at,
            bog,
            credential: credential(p),
            operation,
            ms: start.elapsed().as_secs_f64() * 1000.,
            code: result.as_ref().err().map(|e| e.code.clone()),
            status: result
                .as_ref()
                .map(|r| r.status)
                .unwrap_or_else(|e| match e.code.as_str() {
                    "invalid_request" => 400,
                    "unauthorized" => 401,
                    "forbidden" => 403,
                    "not_found" => 404,
                    "conflict" => 409,
                    "payload_too_large" => 413,
                    "capacity" => 429,
                    _ => 503,
                }),
            request_id,
            outcome,
            release_ms,
        });
    }
    pub fn snapshot(&self, bog: BogId, p: &Principal, seconds: i64) -> Result<Value, CloudError> {
        let m = self.memory.lock().map_err(|_| unavailable())?;
        let at = now();
        let own = (p.kind() == PrincipalKind::App).then(|| credential(p));
        let rows: Vec<_> = m
            .requests
            .iter()
            .filter(|r| {
                r.bog == bog
                    && r.at >= at - seconds
                    && own.as_ref().is_none_or(|c| *c == r.credential)
            })
            .collect();
        let mut groups: std::collections::BTreeMap<(&str, &str), Vec<&Request>> =
            std::collections::BTreeMap::new();
        for r in &rows {
            groups
                .entry((r.operation, &r.credential))
                .or_default()
                .push(r);
        }
        let requests:Vec<_>=groups.into_iter().map(|((op,c),r)|json!({"operation":op,"credential_id":c,"count":r.len(),"errors":error_counts(&r),"statuses":status_counts(&r),"latency_ms":distribution(r.iter().rev().take(128).map(|r|r.ms).collect())})).collect();
        let errors:Vec<_>=rows.iter().rev().filter(|r|r.code.is_some()).take(32).map(|r|json!({"request_id":r.request_id,"operation":r.operation,"credential_id":r.credential,"code":r.code,"status":r.status,"at":r.at})).collect();
        let active: usize = m
            .active
            .iter()
            .filter(|((b, c), _)| *b == bog && own.as_ref().is_none_or(|o| o == c))
            .map(|(_, n)| *n)
            .sum();
        Ok(
            json!({"bog_id":bog,"window_seconds":seconds,"observed_since":self.since,"reset_at":self.since,"window_complete":self.since<=at-seconds&&m.dropped_at.is_none_or(|t|t<at-seconds),"truncated":m.dropped_at.is_some_and(|t|t>=at-seconds),"scope":if own.is_some(){"credential"}else{"bog"},"requests":requests,"error_samples":errors,"waits":{"active":active,"outcomes":{"changed":rows.iter().filter(|r|r.outcome==Some("changed")).count(),"timeout":rows.iter().filter(|r|r.outcome==Some("timeout")).count(),"reset":rows.iter().filter(|r|r.outcome==Some("reset")).count()},"duration_ms":distribution(rows.iter().rev().filter(|r|r.outcome.is_some()).take(128).map(|r|r.ms).collect()),"write_ack_to_release_ms":distribution(rows.iter().rev().filter_map(|r|r.release_ms).take(128).collect())},"storage":m.usage.get(&bog).map(|(at,usage)|json!({"available":true,"cached_at":at,"usage":usage})).unwrap_or(json!({"available":false})),"retention":{"max_series_global":512,"max_request_observations_global":MAX_REQUESTS,"max_latency_samples_per_series":128,"max_error_samples":32}}),
        )
    }
    pub fn events(
        &self,
        bog: BogId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Value, CloudError> {
        if limit == 0 || limit > 100 {
            return Err(CloudError::new(
                "invalid_request",
                "event limit must be 1 to 100",
            ));
        }
        let db = self.db.lock().map_err(|_| unavailable())?;
        db.execute("DELETE FROM events WHERE at<?1", [now() - RETENTION])
            .map_err(|_| unavailable())?;
        let epoch: String = db
            .query_row("SELECT epoch FROM metadata LIMIT 1", [], |r| r.get(0))
            .map_err(|_| unavailable())?;
        let mut epoch_reset = false;
        let mut after = 0i64;
        if let Some(c) = cursor {
            let invalid = || CloudError::new("invalid_request", "invalid event cursor");
            if c.len() > 256 {
                return Err(invalid());
            }
            let bytes = URL_SAFE_NO_PAD.decode(c).map_err(|_| invalid())?;
            let v: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if v["v"] != 1 || v["bog"] != bog.to_string() {
                return Err(invalid());
            }
            epoch_reset = v["epoch"] != epoch;
            after = v["after"]
                .as_i64()
                .filter(|v| *v >= 0)
                .ok_or_else(invalid)?;
        }
        let earliest: Option<i64> = db
            .query_row(
                "SELECT MIN(id) FROM events WHERE bog=?1",
                [bog.to_string()],
                |r| r.get(0),
            )
            .map_err(|_| unavailable())?;
        let exists = after == 0
            || db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE bog=?1 AND id=?2)",
                    params![bog.to_string(), after],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(|_| unavailable())?;
        let reset = cursor.is_some() && (!exists || epoch_reset);
        if reset {
            after = 0;
        }
        let mut stmt=db.prepare("SELECT id,at,kind,credential,reason FROM events WHERE bog=?1 AND id>?2 ORDER BY id LIMIT ?3").map_err(|_|unavailable())?;
        let events:Vec<Value>=stmt.query_map(params![bog.to_string(),after,(limit+1) as i64],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"at":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"credential_id":r.get::<_,Option<String>>(3)?,"reason":r.get::<_,Option<String>>(4)?}))).map_err(|_|unavailable())?.collect::<Result<Vec<_>,_>>().map_err(|_|unavailable())?;
        let has_more = events.len() > limit;
        let events: Vec<_> = events.into_iter().take(limit).collect();
        let last = events
            .last()
            .and_then(|v| v["id"].as_i64())
            .unwrap_or(after);
        let next = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"v":1,"bog":bog,"after":last,"epoch":epoch})).unwrap(),
        );
        Ok(
            json!({"bog_id":bog,"events":events,"next_cursor":next,"has_more":has_more,"reset":reset,"earliest_retained_id":earliest,"retention":{"max_age_seconds":RETENTION,"max_per_bog":1000,"max_global":20000,"survives_restart":true}}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CloudService, Operation, Scope, config::Config};
    const OWNER: &str = "observability-owner-secret-at-least-32-bytes";
    fn service(root: &Path) -> std::sync::Arc<CloudService> {
        CloudService::open(
            Config::new(root.join("root"), root.join("missing-worker")),
            OWNER,
        )
        .unwrap()
    }
    fn person(s: &CloudService, name: &str) -> Principal {
        let identity = crate::oauth::VerifiedIdentity::native(name, (now() + 3600) as u64);
        let (_, w) = s.auth.provision_identity(&identity).unwrap();
        s.auth.principal_from_verified(&identity, w.id).unwrap()
    }
    fn ok() -> Result<crate::OperationResult, CloudError> {
        Ok(crate::OperationResult {
            status: 200,
            body: json!({}),
        })
    }
    #[tokio::test]
    async fn metrics_isolate_credentials_recheck_membership_and_never_start_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let s = service(tmp.path());
        let human = person(&s, "owner");
        let bog = s
            .registry
            .create_for_principal(&human, "a", "records-v1", "a", 32)
            .unwrap()
            .id;
        let a = s.auth.issue(&human, bog, Scope::Read).unwrap();
        let b = s.auth.issue(&human, bog, Scope::Read).unwrap();
        let app = s.auth.authenticate(&a.secret).unwrap();
        let other = s.auth.authenticate(&b.secret).unwrap();
        s.execute(&app, Operation::DescribeBog { bog_id: bog })
            .await
            .unwrap();
        s.execute(&other, Operation::DescribeBog { bog_id: bog })
            .await
            .unwrap();
        let generation = s.registry.get(bog).unwrap().generation;
        let metrics = s
            .execute(
                &app,
                Operation::BogMetrics {
                    bog_id: bog,
                    window: "1h".into(),
                },
            )
            .await
            .unwrap()
            .body;
        assert_eq!(metrics["scope"], "credential");
        assert_eq!(metrics["requests"].as_array().unwrap().len(), 1);
        assert_eq!(metrics["requests"][0]["credential_id"], a.id);
        assert!(!metrics.to_string().contains(&b.id));
        assert_eq!(s.supervisor.resident_count().await, 0);
        assert_eq!(s.registry.get(bog).unwrap().generation, generation);
        assert_eq!(
            s.execute(
                &app,
                Operation::BogEvents {
                    bog_id: bog,
                    cursor: None,
                    limit: 50
                }
            )
            .await
            .err()
            .unwrap()
            .code,
            "forbidden"
        );
        let tokens = s
            .execute(&human, Operation::ListTokens { bog_id: bog })
            .await
            .unwrap()
            .body;
        assert!(
            tokens["tokens"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["last_used_at"].as_i64().is_some_and(|v| v % 60 == 0))
        );
        let foreign = person(&s, "foreign");
        let before = s.observability.snapshot(bog, &human, 3600).unwrap()["requests"].clone();
        assert!(
            s.execute(
                &foreign,
                Operation::BogMetrics {
                    bog_id: bog,
                    window: "1h".into()
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            before,
            s.observability.snapshot(bog, &human, 3600).unwrap()["requests"]
        );
        s.registry
            .connection()
            .unwrap()
            .execute(
                "DELETE FROM memberships WHERE account_id=?1",
                [human.account_id().unwrap()],
            )
            .unwrap();
        assert!(
            s.execute(
                &human,
                Operation::BogMetrics {
                    bog_id: bog,
                    window: "1h".into()
                }
            )
            .await
            .is_err()
        );
        assert!(
            s.execute(
                &app,
                Operation::BogMetrics {
                    bog_id: bog,
                    window: "1h".into()
                }
            )
            .await
            .is_err()
        );
    }
    #[test]
    fn retained_events_page_restart_expiry_and_activity() {
        let tmp = tempfile::tempdir().unwrap();
        let s = service(tmp.path());
        let p = person(&s, "events");
        let bog = s
            .registry
            .create_for_principal(&p, "a", "records-v1", "a", 32)
            .unwrap()
            .id;
        let token = s.auth.issue(&p, bog, Scope::Read).unwrap();
        let app = s.auth.authenticate(&token.secret).unwrap();
        s.observability.touch(&app);
        let changes = s.observability.db.lock().unwrap().total_changes();
        for _ in 0..10 {
            s.observability.touch(&app);
        }
        assert_eq!(changes, s.observability.db.lock().unwrap().total_changes());
        for _ in 0..3 {
            s.observability
                .event(bog, "worker_running", None, Some("readiness_passed"));
        }
        let first = s.observability.events(bog, None, 2).unwrap();
        assert_eq!(first["events"].as_array().unwrap().len(), 2);
        assert_eq!(first["has_more"], true);
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        drop(s);
        let s = service(tmp.path());
        let next = s.observability.events(bog, Some(&cursor), 2).unwrap();
        assert_eq!(next["events"].as_array().unwrap().len(), 2);
        assert_eq!(next["reset"], false);
        let mut tokens = json!([{"id":token.id}]);
        s.observability.merge_activity(&mut tokens);
        assert!(tokens[0]["last_used_at"].is_number());
        s.observability
            .db
            .lock()
            .unwrap()
            .execute("UPDATE events SET at=?1", [now() - RETENTION - 1])
            .unwrap();
        assert_eq!(
            s.observability.events(bog, Some(&cursor), 2).unwrap()["reset"],
            true
        );
        for _ in 0..1002 {
            s.observability.event(bog, "worker_running", None, None);
        }
        let count: i64 = s
            .observability
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1000);
        assert!(
            s.observability
                .events(BogId(uuid::Uuid::new_v4()), Some(&cursor), 2)
                .is_err()
        );
    }
    #[test]
    fn rolling_windows_samples_redaction_and_wait_raii() {
        let tmp = tempfile::tempdir().unwrap();
        let s = service(tmp.path());
        let p = s.auth.authenticate(OWNER).unwrap();
        let bog = BogId(uuid::Uuid::new_v4());
        for _ in 0..160 {
            s.observability.record(
                bog,
                &p,
                "schema",
                Instant::now(),
                "not-a-safe-request-id",
                &ok(),
            );
        }
        {
            let mut m = s.observability.memory.lock().unwrap();
            m.requests[0].at = now() - 3601;
            m.requests[1].at = now() - 301;
        }
        let v = s.observability.snapshot(bog, &p, 300).unwrap();
        assert_eq!(v["requests"][0]["count"], 158);
        assert_eq!(v["requests"][0]["latency_ms"]["sample_count"], 128);
        let guard = s.observability.active_wait(bog, &p);
        assert_eq!(
            s.observability.snapshot(bog, &p, 3600).unwrap()["waits"]["active"],
            1
        );
        drop(guard);
        assert_eq!(
            s.observability.snapshot(bog, &p, 3600).unwrap()["waits"]["active"],
            0
        );
        let error = Err(CloudError::new(
            "invalid_request",
            "private-key-and-payload",
        ));
        let id = uuid::Uuid::new_v4().to_string();
        s.observability
            .record(bog, &p, "schema", Instant::now(), &id, &error);
        let v = s.observability.snapshot(bog, &p, 3600).unwrap();
        assert_eq!(v["error_samples"][0]["request_id"], id);
        assert!(!v.to_string().contains("private-key-and-payload"));
        assert_eq!(v["requests"][0]["errors"]["invalid_request"], 1);
        for (reset, changed) in [(false, true), (false, false), (true, true)] {
            let start = Instant::now();
            s.observability.write_ack(bog, 1, 4);
            let result = Ok(crate::OperationResult {
                status: 200,
                body: json!({"reset":reset,"changed":changed,"seq":4,"cursor":crate::changes::cursor_for_response(bog,1,4)}),
            });
            s.observability
                .record(bog, &p, "wait_for_change", start, &id, &result);
        }
        let v = s.observability.snapshot(bog, &p, 3600).unwrap();
        assert_eq!(
            v["waits"]["outcomes"],
            json!({"changed":1,"timeout":1,"reset":1})
        );
        assert_eq!(v["waits"]["write_ack_to_release_ms"]["sample_count"], 1);
        assert!(
            s.observability
                .memory
                .lock()
                .unwrap()
                .requests
                .iter()
                .all(|r| r.at >= now() - 3600)
        );
    }
    #[test]
    fn request_memory_and_series_are_bounded_and_disclose_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let s = service(tmp.path());
        let p = s.auth.authenticate(OWNER).unwrap();
        let bog = BogId(uuid::Uuid::new_v4());
        for _ in 0..MAX_REQUESTS + 1 {
            s.observability
                .record(bog, &p, "schema", Instant::now(), "", &ok());
        }
        assert_eq!(
            s.observability.memory.lock().unwrap().requests.len(),
            MAX_REQUESTS
        );
        assert_eq!(
            s.observability.snapshot(bog, &p, 3600).unwrap()["truncated"],
            true
        );
        s.observability.memory.lock().unwrap().requests.clear();
        for _ in 0..513 {
            s.observability.record(
                BogId(uuid::Uuid::new_v4()),
                &p,
                "schema",
                Instant::now(),
                "",
                &ok(),
            );
        }
        assert_eq!(s.observability.memory.lock().unwrap().requests.len(), 512);
    }
    #[test]
    fn membership_and_credential_mutations_have_content_free_events() {
        let tmp = tempfile::tempdir().unwrap();
        let s = service(tmp.path());
        let owner = person(&s, "membership-owner");
        let guest = person(&s, "membership-guest");
        let bog = s
            .registry
            .create_for_principal(&owner, "a", "records-v1", "a", 32)
            .unwrap()
            .id;
        let invite = s.auth.invite(&owner, "member").unwrap();
        s.auth
            .accept_invitation_for_principal(&guest, &invite.secret)
            .unwrap();
        s.auth
            .remove_member(&owner, guest.account_id().unwrap())
            .unwrap();
        let token = s.auth.issue(&owner, bog, Scope::Read).unwrap();
        s.auth.revoke(&owner, bog, &token.id).unwrap();
        let events = s.observability.events(bog, None, 100).unwrap();
        let kinds: Vec<_> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "member_added",
                "member_removed",
                "credential_issued",
                "credential_revoked"
            ]
        );
        assert!(!events.to_string().contains(guest.account_id().unwrap()));
        assert!(!events.to_string().contains(&token.secret));
    }
}
