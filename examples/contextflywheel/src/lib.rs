use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anny::metric::Cosine;
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use fold::pipeline::{Keyed, Map, Scored, terminal};
use fold::stream::KeyedStream;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod ranker;
pub mod server;

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_TOKEN_BUDGET: usize = 8_000;
const RECENT_LIMIT: usize = 10;
const SEARCH_LIMIT: usize = 8;
const DIM: usize = ese::DIMENSIONS;
const RRF_K: f64 = 60.0;

fn embed_record(record: &Keyed<u64, String>) -> Keyed<u64, [f32; DIM]> {
    Keyed::new(record.key, ese::encode_single(&record.val))
}

type SemanticIndex = terminal::search::Hnsw<u64, f32, Cosine, DIM>;
type SemanticMap = Map<
    fn(&Keyed<u64, String>) -> Keyed<u64, [f32; DIM]>,
    SemanticIndex,
    Keyed<u64, String>,
    Keyed<u64, [f32; DIM]>,
>;

type SearchPipeline = (
    terminal::search::Bm25<u64, String>,
    SemanticMap,
    terminal::Table<u64, String>,
);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub record: MissionRecord,
    pub fused_score: f64,
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Objective,
    Constraint,
    Permission,
    Observation,
    ToolCall,
    ToolResult,
    ToolFailure,
    BeliefRevision,
    FailedApproach,
    Blocker,
    Checkpoint,
    UserPrompt,
    ContextSnapshot,
    DecisionRedirect,
    Compaction,
    AgentStop,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MissionRecord {
    pub schema_version: u32,
    pub id: String,
    pub step: u64,
    pub timestamp: DateTime<Utc>,
    pub kind: EventKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub belief_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub belief_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub schema_version: u32,
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub request_step: u64,
    pub event_step: u64,
    pub mode: String,
    pub ranking_enabled: bool,
    pub model: String,
    pub context_hash: String,
    pub estimated_tokens: usize,
    pub token_count_method: String,
    pub context: String,
}

impl MissionRecord {
    pub fn plain(kind: EventKind, text: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            step: 0,
            timestamp: Utc::now(),
            kind,
            text: text.into(),
            belief_key: None,
            belief_version: None,
            confidence: None,
            evidence_ids: vec![],
            supersedes: None,
        }
    }

    fn searchable_text(&self) -> String {
        format!(
            "{:?} {} {}",
            self.kind,
            self.belief_key.as_deref().unwrap_or(""),
            self.text
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Belief {
    pub record_id: String,
    pub key: String,
    pub version: u64,
    pub statement: String,
    pub confidence: u16,
    pub evidence_ids: Vec<String>,
    pub supersedes: Option<String>,
    pub step: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MissionState {
    pub objective: Option<MissionRecord>,
    pub constraints: Vec<MissionRecord>,
    pub permission: Option<MissionRecord>,
    pub beliefs: BTreeMap<String, Belief>,
    pub historical_beliefs: Vec<Belief>,
    pub blockers: Vec<MissionRecord>,
    pub failures: Vec<MissionRecord>,
    pub recent: Vec<MissionRecord>,
}

pub struct Ledger {
    root: PathBuf,
    _lock: File,
    records: Vec<MissionRecord>,
    search: KeyedStream<u64, String, SearchPipeline>,
}

impl Ledger {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("writer.lock"))?;
        lock.lock_exclusive()
            .context("acquire the single-writer mission lock")?;
        let ledger_path = root.join("ledger.jsonl");
        let records = read_records(&ledger_path)?;
        let mut search = KeyedStream::new(
            root.join("fold-search.db"),
            (
                terminal::search::Bm25::new("mission_bm25"),
                Map::new(
                    embed_record as fn(&Keyed<u64, String>) -> Keyed<u64, [f32; DIM]>,
                    SemanticIndex::new("mission_hnsw", Cosine, 42),
                ),
                terminal::Table::new("mission_docs"),
            ),
        );
        search.wtx(|tx| {
            for record in &records {
                tx.upsert(&record.step, &record.searchable_text());
            }
        });
        Ok(Self {
            root,
            _lock: lock,
            records,
            search,
        })
    }

    pub fn records(&self) -> &[MissionRecord] {
        &self.records
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn append(&mut self, mut record: MissionRecord) -> Result<MissionRecord> {
        if record.schema_version != SCHEMA_VERSION {
            bail!("unsupported schema version")
        }
        if record.text.trim().is_empty() {
            bail!("record text cannot be empty")
        }
        record.step = self.records.last().map_or(1, |r| r.step + 1);
        record.timestamp = Utc::now();

        record.text = redact_and_limit(&record.text);
        let line = serde_json::to_string(&record)?;
        let path = self.root.join("ledger.jsonl");
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{line}")?;
        file.sync_data()?;

        self.search
            .wtx(|tx| tx.upsert(&record.step, &record.searchable_text()));
        self.search.checkpoint();
        self.records.push(record.clone());
        Ok(record)
    }

    pub fn revise_belief(
        &mut self,
        key: &str,
        statement: &str,
        confidence: f32,
        evidence_ids: &[String],
        expected_current_version: Option<u64>,
    ) -> Result<MissionRecord> {
        if key.trim().is_empty() || statement.trim().is_empty() {
            bail!("belief key and statement are required")
        }
        if evidence_ids.is_empty() {
            bail!("belief revisions require at least one evidence ID")
        }
        if !(0.0..=1.0).contains(&confidence) {
            bail!("confidence must be between 0 and 1")
        }
        let known: BTreeSet<&str> = self.records.iter().map(|r| r.id.as_str()).collect();
        for id in evidence_ids {
            if !known.contains(id.as_str()) {
                bail!("unknown evidence ID: {id}")
            }
        }

        let state = self.state_at(None);
        let old = state.beliefs.get(key);
        let actual = old.map(|b| b.version);
        if actual != expected_current_version {
            bail!(
                "stale belief revision: expected {expected_current_version:?}, current is {actual:?}"
            )
        }
        let mut record = MissionRecord::plain(EventKind::BeliefRevision, statement);
        record.belief_key = Some(key.to_owned());
        record.belief_version = Some(actual.unwrap_or(0) + 1);
        record.confidence = Some((confidence * 1000.0).round() as u16);
        record.evidence_ids = evidence_ids.to_vec();
        record.supersedes = old.map(|b| b.record_id.clone());
        self.append(record)
    }

    pub fn record_context_snapshot(
        &mut self,
        request_step: u64,
        context: &str,
        mode: &str,
        ranking_enabled: bool,
        model: &str,
    ) -> Result<ContextSnapshot> {
        let id = Uuid::new_v4().to_string();
        let mut event = MissionRecord::plain(
            EventKind::ContextSnapshot,
            serde_json::json!({
                "snapshot_id": id,
                "request_step": request_step,
                "mode": mode,
                "ranking_enabled": ranking_enabled,
                "model": model,
                "context_hash": stable_hash(context),
                "estimated_tokens": estimate_tokens(context),
                "token_count_method": "Estimated at four characters per token",
            })
            .to_string(),
        );
        event = self.append(event)?;
        let snapshot = ContextSnapshot {
            schema_version: SCHEMA_VERSION,
            id,
            created_at: event.timestamp,
            request_step,
            event_step: event.step,
            mode: mode.to_owned(),
            ranking_enabled,
            model: model.to_owned(),
            context_hash: stable_hash(context),
            estimated_tokens: estimate_tokens(context),
            token_count_method: "Estimated at four characters per token".into(),
            context: context.to_owned(),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("context-snapshots.jsonl"))?;
        writeln!(file, "{}", serde_json::to_string(&snapshot)?)?;
        file.sync_data()?;
        Ok(snapshot)
    }

    pub fn maybe_record_redirect(&mut self, action: &str) -> Result<Option<MissionRecord>> {
        if !(action.starts_with("Edit ")
            || action.contains("unittest")
            || action.contains("profile.py"))
        {
            return Ok(None);
        }
        let Some(belief) = self
            .records
            .iter()
            .rev()
            .find(|record| record.kind == EventKind::BeliefRevision)
            .cloned()
        else {
            return Ok(None);
        };
        let already_recorded = self.records.iter().any(|record| {
            record.kind == EventKind::DecisionRedirect && record.text.contains(&belief.id)
        });
        if already_recorded {
            return Ok(None);
        }
        let from = belief
            .supersedes
            .as_deref()
            .and_then(|id| self.records.iter().find(|record| record.id == id))
            .map_or("Previous investigation", |record| record.text.as_str());
        let action_record = MissionRecord::plain(EventKind::ToolCall, action);
        let redirect = MissionRecord::plain(
            EventKind::DecisionRedirect,
            serde_json::json!({
                "from": from,
                "to": first_line_action(&action_record),
                "caused_by_belief_id": belief.id,
                "belief_version": belief.belief_version,
            })
            .to_string(),
        );
        self.append(redirect).map(Some)
    }

    pub fn state_at(&self, step: Option<u64>) -> MissionState {
        compile_state(
            self.records
                .iter()
                .filter(|r| step.is_none_or(|s| r.step <= s)),
        )
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<MissionRecord> {
        if query.trim().is_empty() {
            return vec![];
        }
        let by_step: BTreeMap<u64, &MissionRecord> =
            self.records.iter().map(|r| (r.step, r)).collect();
        self.search.rtx(|(bm25, _, _)| {
            bm25.search(query, limit)
                .into_iter()
                .filter_map(|hit| by_step.get(&hit.val).map(|r| (*r).clone()))
                .collect()
        })
    }

    pub fn hybrid_candidates(&self, query: &str, limit: usize) -> Vec<Candidate> {
        if query.trim().is_empty() {
            return vec![];
        }
        let by_step: BTreeMap<u64, &MissionRecord> =
            self.records.iter().map(|r| (r.step, r)).collect();
        self.search.rtx(|(bm25, semantic, _)| {
            let keyword = bm25.search(query, 30);
            let vectors = semantic.search(&ese::encode_single(query));
            let mut scores: BTreeMap<u64, (f64, BTreeSet<String>)> = BTreeMap::new();
            add_rrf(&mut scores, &keyword, "bm25");
            add_rrf(&mut scores, &vectors, "semantic");
            let mut output: Vec<_> = scores
                .into_iter()
                .filter_map(|(step, (score, sources))| {
                    by_step.get(&step).map(|record| Candidate {
                        record: (*record).clone(),
                        fused_score: score,
                        sources: sources.into_iter().collect(),
                    })
                })
                .collect();
            output.sort_by(|a, b| b.fused_score.total_cmp(&a.fused_score));
            output.truncate(limit);
            output
        })
    }

    pub fn render_context(&self, query: &str, token_budget: usize) -> String {
        let state = self.state_at(None);
        let mut out = String::from("CONTEXTFLYWHEEL MISSION STATE\n\n");
        section_record(&mut out, "MISSION", state.objective.as_ref());
        section_records(
            &mut out,
            "CONSTRAINTS AND PERMISSIONS",
            state.constraints.iter().chain(state.permission.iter()),
        );

        out.push_str("CURRENT BELIEFS\n");
        if state.beliefs.is_empty() {
            out.push_str("- None recorded.\n")
        }
        for b in state.beliefs.values() {
            out.push_str(&format!(
                "- [v{}, current, confidence {:.2}] {}\n  Evidence: {}\n",
                b.version,
                b.confidence as f32 / 1000.0,
                b.statement,
                b.evidence_ids.join(", ")
            ));
        }
        out.push('\n');

        section_records(&mut out, "ACTIVE BLOCKERS", state.blockers.iter());
        let evidence_by_id: BTreeMap<&str, &MissionRecord> =
            self.records.iter().map(|r| (r.id.as_str(), r)).collect();
        let mut evidence = vec![];
        for belief in state.beliefs.values() {
            for id in &belief.evidence_ids {
                if let Some(record) = evidence_by_id.get(id.as_str()) {
                    evidence.push((*record).clone())
                }
            }
        }
        section_records(&mut out, "SUPPORTING EVIDENCE", evidence.iter());

        out.push_str("HISTORICAL — NOT CURRENT\n");
        if state.historical_beliefs.is_empty() {
            out.push_str("- None recorded.\n")
        }
        for b in &state.historical_beliefs {
            out.push_str(&format!("- [v{}, superseded] {}\n", b.version, b.statement));
        }
        out.push('\n');
        section_records(&mut out, "FAILED APPROACHES", state.failures.iter());

        let protected_ids: BTreeSet<&str> = evidence.iter().map(|r| r.id.as_str()).collect();
        let mut optional = Vec::new();
        for r in self.search(query, SEARCH_LIMIT) {
            if !protected_ids.contains(r.id.as_str()) {
                optional.push(r)
            }
        }
        for r in state.recent.iter().rev().take(RECENT_LIMIT) {
            if !protected_ids.contains(r.id.as_str()) && !optional.iter().any(|x| x.id == r.id) {
                optional.push(r.clone())
            }
        }
        append_optional_with_budget(
            &mut out,
            "RELEVANT AND RECENT EVENTS",
            &optional,
            token_budget,
        );
        out.push_str("\nPROVENANCE\n- Compiled from the append-only Mission Ledger. Historical beliefs are not current instructions.\n");
        out
    }

    pub fn render_hybrid_context(
        &self,
        query: &str,
        token_budget: usize,
        rank: bool,
        base_url: Option<&str>,
        api_key: Option<&str>,
        model: &str,
    ) -> String {
        let mut output = self.render_context(query, token_budget);
        let state = self.state_at(None);
        let protected: BTreeSet<_> = state
            .beliefs
            .values()
            .map(|belief| belief.record_id.as_str())
            .collect();
        let candidates: Vec<_> = self
            .hybrid_candidates(query, 30)
            .into_iter()
            .filter(|candidate| !protected.contains(candidate.record.id.as_str()))
            .collect();
        let gate = if rank {
            ranker::select(self, query, &candidates, base_url, api_key, model)
        } else {
            ranker::select(self, query, &candidates, None, None, model)
        };
        let by_id: BTreeMap<_, _> = candidates
            .iter()
            .map(|c| (c.record.id.as_str(), c))
            .collect();
        output.push_str(&format!("\nHYBRID CONTEXT GATE ({})\n", gate.method));
        for item in gate.selected {
            if let Some(candidate) = by_id.get(item.record_id.as_str()) {
                output.push_str(&format!(
                    "- [{}] {} — {} [{}]\n",
                    item.role, candidate.record.text, item.reason, item.record_id
                ));
            }
        }
        output
    }

    pub fn render_history(&self, step: u64) -> Result<String> {
        if step == 0 || self.records.iter().all(|r| r.step != step) {
            bail!("step {step} does not exist")
        }
        let state = self.state_at(Some(step));
        let event = self.records.iter().find(|r| r.step == step).unwrap();
        let later: Vec<_> = self
            .records
            .iter()
            .filter(|r| r.step > step && r.kind == EventKind::BeliefRevision)
            .collect();
        let mut out = format!(
            "STEP {step}\nEvent: {:?} — {}\nTime: {}\n\n",
            event.kind, event.text, event.timestamp
        );
        out.push_str("BELIEFS ACTIVE AT THIS STEP\n");
        if state.beliefs.is_empty() {
            out.push_str("- None recorded.\n")
        }
        for b in state.beliefs.values() {
            out.push_str(&format!("- {} v{}: {}\n", b.key, b.version, b.statement));
        }
        out.push_str("\nPERMISSIONS IN FORCE\n");
        out.push_str(&format!(
            "- {}\n",
            state
                .permission
                .as_ref()
                .map_or("Not recorded", |r| r.text.as_str())
        ));
        out.push_str("\nLATER BELIEF CHANGES\n");
        if later.is_empty() {
            out.push_str("- None.\n")
        }
        for r in later {
            out.push_str(&format!(
                "- Step {}: {} v{} — {}\n",
                r.step,
                r.belief_key.as_deref().unwrap_or("belief"),
                r.belief_version.unwrap_or(0),
                r.text
            ));
        }
        Ok(out)
    }
}

fn add_rrf<T>(
    scores: &mut BTreeMap<u64, (f64, BTreeSet<String>)>,
    hits: &[Scored<T, u64>],
    source: &str,
) {
    for (rank, hit) in hits.iter().enumerate() {
        let entry = scores.entry(hit.val).or_default();
        entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
        entry.1.insert(source.to_owned());
    }
}

pub fn compile_state<'a>(records: impl Iterator<Item = &'a MissionRecord>) -> MissionState {
    let mut state = MissionState::default();
    for record in records {
        match record.kind {
            EventKind::Objective => state.objective = Some(record.clone()),
            EventKind::Constraint => state.constraints.push(record.clone()),
            EventKind::Permission => state.permission = Some(record.clone()),
            EventKind::BeliefRevision => {
                let belief = Belief {
                    record_id: record.id.clone(),
                    key: record
                        .belief_key
                        .clone()
                        .unwrap_or_else(|| "unknown".into()),
                    version: record.belief_version.unwrap_or(1),
                    statement: record.text.clone(),
                    confidence: record.confidence.unwrap_or(0),
                    evidence_ids: record.evidence_ids.clone(),
                    supersedes: record.supersedes.clone(),
                    step: record.step,
                };
                if let Some(old) = state.beliefs.insert(belief.key.clone(), belief) {
                    state.historical_beliefs.push(old)
                }
            }
            EventKind::FailedApproach => state.failures.push(record.clone()),
            EventKind::Blocker => state.blockers.push(record.clone()),
            _ => {}
        }
        state.recent.push(record.clone());
        if state.recent.len() > RECENT_LIMIT {
            state.recent.remove(0);
        }
    }
    state
}

fn read_records(path: &Path) -> Result<Vec<MissionRecord>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let mut records = vec![];
    for (line_no, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let record: MissionRecord = serde_json::from_str(&line?)
            .with_context(|| format!("invalid ledger line {}", line_no + 1))?;
        if record.schema_version != SCHEMA_VERSION {
            bail!("ledger schema {} is unsupported", record.schema_version)
        }
        let expected = records.last().map_or(1, |r: &MissionRecord| r.step + 1);
        if record.step != expected {
            bail!("ledger step sequence is invalid at {}", record.step)
        }
        records.push(record);
    }
    Ok(records)
}

pub fn load_records(root: &Path) -> Result<Vec<MissionRecord>> {
    read_records(&root.join("ledger.jsonl"))
}

pub fn load_context_snapshots(root: &Path) -> Result<Vec<ContextSnapshot>> {
    let path = root.join("context-snapshots.jsonl");
    if !path.exists() {
        return Ok(vec![]);
    }
    BufReader::new(File::open(path)?)
        .lines()
        .enumerate()
        .map(|(line_no, line)| {
            let snapshot: ContextSnapshot = serde_json::from_str(&line?)
                .with_context(|| format!("invalid context snapshot line {}", line_no + 1))?;
            if snapshot.schema_version != SCHEMA_VERSION {
                bail!("context snapshot schema is unsupported")
            }
            Ok(snapshot)
        })
        .collect()
}

fn section_record(out: &mut String, title: &str, record: Option<&MissionRecord>) {
    out.push_str(title);
    out.push('\n');
    out.push_str(&format!(
        "- {}\n\n",
        record.map_or("Not recorded", |r| r.text.as_str())
    ));
}

fn section_records<'a>(
    out: &mut String,
    title: &str,
    records: impl Iterator<Item = &'a MissionRecord>,
) {
    out.push_str(title);
    out.push('\n');
    let mut any = false;
    for r in records {
        any = true;
        out.push_str(&format!("- {} [{}]\n", r.text, r.id));
    }
    if !any {
        out.push_str("- None recorded.\n")
    }
    out.push('\n');
}

fn append_optional_with_budget(
    out: &mut String,
    title: &str,
    records: &[MissionRecord],
    token_budget: usize,
) {
    out.push_str(title);
    out.push('\n');
    let max_chars = token_budget.saturating_mul(4);
    let mut any = false;
    for r in records {
        let line = format!("- Step {} {:?}: {} [{}]\n", r.step, r.kind, r.text, r.id);
        if out.len() + line.len() > max_chars {
            break;
        }
        out.push_str(&line);
        any = true;
    }
    if !any {
        out.push_str("- None selected.\n")
    }
}

pub fn parse_kind(value: &str) -> Result<EventKind> {
    serde_json::from_str(&format!("\"{}\"", value.replace('-', "_")))
        .map_err(|_| anyhow!("unknown event kind: {value}"))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnittestOutcome {
    pub ran: u32,
    pub passed: u32,
    pub failures: u32,
    pub errors: u32,
    pub ok: bool,
    pub source_step: u64,
}

pub fn parse_unittest_outcome(text: &str, source_step: u64) -> Option<UnittestOutcome> {
    let ran = number_after(text, "Ran ")?;
    if !text.contains(" test") {
        return None;
    }
    let failures = number_after(text, "failures=").unwrap_or(0);
    let errors = number_after(text, "errors=").unwrap_or(0);
    let ok = text.contains("\nOK") || text.contains(" OK") || text.ends_with("OK");
    let passed = if ok {
        ran
    } else {
        ran.saturating_sub(failures).saturating_sub(errors)
    };
    Some(UnittestOutcome {
        ran,
        passed,
        failures: if ok { 0 } else { failures },
        errors: if ok { 0 } else { errors },
        ok,
        source_step,
    })
}

fn number_after(text: &str, prefix: &str) -> Option<u32> {
    let start = text.find(prefix)? + prefix.len();
    let digits: String = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn first_line_action(record: &MissionRecord) -> String {
    let text = record.text.replace('\n', " ");
    match record.kind {
        EventKind::ToolCall => {
            let tool = text.split_whitespace().next().unwrap_or("Tool");
            if tool == "Edit" {
                let path = text
                    .split("file_path\":\"")
                    .nth(1)
                    .or_else(|| text.split("file_path\": \"").nth(1))
                    .and_then(|rest| rest.split('"').next())
                    .and_then(|path| Path::new(path).file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("source file");
                format!("Edit {path}")
            } else if text.contains("unittest") {
                "Run the complete test suite".into()
            } else if text.contains("profile.py") {
                "Run the profiler for timing evidence".into()
            } else {
                format!("{tool} started")
            }
        }
        EventKind::ToolResult if text.contains("Ran ") && text.contains(" test") => {
            parse_unittest_outcome(&record.text, record.step)
                .map(|outcome| {
                    if outcome.ok {
                        format!("{}/{} tests passed", outcome.passed, outcome.ran)
                    } else {
                        format!(
                            "{}/{} tests passed ({} failed, {} errors)",
                            outcome.passed, outcome.ran, outcome.failures, outcome.errors
                        )
                    }
                })
                .unwrap_or_else(|| "Tests recorded".into())
        }
        EventKind::FailedApproach => format!("Failed approach: {}", truncate_words(&text, 18)),
        EventKind::BeliefRevision => "Revise the current belief".into(),
        _ => truncate_words(&text, 18),
    }
}

fn truncate_words(text: &str, max_words: usize) -> String {
    let words: Vec<_> = text.split_whitespace().collect();
    if words.len() <= max_words {
        words.join(" ")
    } else {
        format!("{}…", words[..max_words].join(" "))
    }
}

pub fn latest_unittest_outcomes(
    records: &[MissionRecord],
) -> (Option<UnittestOutcome>, Option<UnittestOutcome>) {
    let mut baseline = None;
    let mut verified = None;
    for record in records {
        if let Some(outcome) = parse_unittest_outcome(&record.text, record.step) {
            if baseline.is_none() {
                baseline = Some(outcome.clone());
            }
            if outcome.ok {
                verified = Some(outcome);
            } else {
                baseline = Some(outcome);
            }
        }
    }
    (baseline, verified)
}

fn next_action_after(records: &[MissionRecord], after_step: u64) -> String {
    for record in records.iter().filter(|record| record.step > after_step) {
        match record.kind {
            EventKind::ToolCall
                if record.text.starts_with("Edit ")
                    || record.text.contains("unittest")
                    || record.text.contains("profile.py") =>
            {
                return first_line_action(record);
            }
            EventKind::FailedApproach | EventKind::BeliefRevision => {
                return first_line_action(record);
            }
            EventKind::ToolResult
                if record.text.contains("Ran ") && record.text.contains(" test") =>
            {
                return first_line_action(record);
            }
            _ => {}
        }
    }
    "Waiting for the agent’s next action.".into()
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn stable_hash(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

pub fn mission_brief(records: &[MissionRecord]) -> serde_json::Value {
    let state = compile_state(records.iter());
    let beliefs: Vec<_> = records
        .iter()
        .filter(|record| record.kind == EventKind::BeliefRevision)
        .collect();
    let current = beliefs.last().copied();
    let previous = if beliefs.len() >= 2 {
        beliefs.get(beliefs.len() - 2).copied()
    } else {
        None
    };
    let evidence: Vec<_> = current
        .map(|belief| {
            records
                .iter()
                .filter(|record| belief.evidence_ids.iter().any(|id| id == &record.id))
                .map(|record| {
                    serde_json::json!({
                        "id": record.id,
                        "step": record.step,
                        "kind": record.kind,
                        "text": record.text,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let (baseline, verified) = latest_unittest_outcomes(records);
    let last_step = records.last().map(|record| record.step).unwrap_or(0);
    let evidence_recorded = records.iter().any(|record| {
        matches!(
            record.kind,
            EventKind::ToolResult | EventKind::Observation | EventKind::ToolFailure
        )
    });
    let belief_evaluated = current.is_some();
    let captured_context = records
        .iter()
        .rev()
        .find(|record| record.kind == EventKind::ContextSnapshot);
    let explicit_redirect = records
        .iter()
        .rev()
        .find(|record| record.kind == EventKind::DecisionRedirect);
    let context_compiled = captured_context.is_some()
        || belief_evaluated
        || records
            .iter()
            .any(|record| record.kind == EventKind::UserPrompt);
    let redirected = explicit_redirect.is_some()
        || current.is_some_and(|belief| {
            records.iter().any(|record| {
                record.step > belief.step
                    && matches!(
                        record.kind,
                        EventKind::ToolCall | EventKind::FailedApproach | EventKind::AgentStop
                    )
            })
        });
    let current_stage = if records
        .last()
        .is_some_and(|record| record.kind == EventKind::AgentStop)
        && redirected
    {
        "agent redirected"
    } else if redirected {
        "agent redirected"
    } else if context_compiled && belief_evaluated {
        "context compiled"
    } else if belief_evaluated {
        "belief evaluated"
    } else if evidence_recorded {
        "evidence recorded"
    } else {
        "waiting"
    };
    let raw_character_count: usize = records
        .iter()
        .map(|record| record.text.chars().count())
        .sum();
    let behavioral_effect = explicit_redirect
        .and_then(|record| serde_json::from_str::<serde_json::Value>(&record.text).ok())
        .and_then(|value| value["to"].as_str().map(str::to_owned))
        .or_else(|| current.map(|record| next_action_after(records, record.step)));
    serde_json::json!({
        "objective": state.objective.as_ref().map(|record| record.text.clone()),
        "constraints": state.constraints.iter().map(|record| record.text.clone()).collect::<Vec<_>>(),
        "permission": state.permission.as_ref().map(|record| record.text.clone()),
        "current_belief": current.map(|record| serde_json::json!({
            "id": record.id,
            "step": record.step,
            "key": record.belief_key,
            "version": record.belief_version,
            "text": record.text,
            "confidence": record.confidence,
            "evidence_ids": record.evidence_ids,
        })),
        "previous_belief": previous.map(|record| serde_json::json!({
            "id": record.id,
            "step": record.step,
            "version": record.belief_version,
            "text": record.text,
        })),
        "evidence": evidence,
        "next_action": current.map(|record| next_action_after(records, record.step)),
        "behavioral_effect": behavioral_effect,
        "captured_context_event": captured_context,
        "decision_redirect_event": explicit_redirect,
        "baseline_outcome": baseline,
        "verified_outcome": verified,
        "pipeline": {
            "evidence_recorded": evidence_recorded,
            "belief_evaluated": belief_evaluated,
            "context_compiled": context_compiled,
            "agent_redirected": redirected,
            "current": current_stage,
        },
        "comparison": {
            "source": "Live mission ledger",
            "token_count_method": "Estimated at four characters per token",
            "raw_event_count": records.len(),
            "raw_character_count": raw_character_count,
            "raw_estimated_tokens": raw_character_count.div_ceil(4),
            "tool_calls": records.iter().filter(|record| record.kind == EventKind::ToolCall).count(),
            "belief_revisions": beliefs.len(),
            "superseded_beliefs": state.historical_beliefs.len(),
            "last_step": last_step,
        },
        "last_step": last_step,
        "completed": records.last().is_some_and(|record| record.kind == EventKind::AgentStop),
    })
}

pub fn context_report(records: &[MissionRecord], step: u64) -> Result<serde_json::Value> {
    let records: Vec<_> = records
        .iter()
        .filter(|record| record.step <= step)
        .cloned()
        .collect();
    if records.iter().all(|record| record.step != step) {
        bail!("step not found")
    }
    let state = compile_state(records.iter());
    let evidence_ids: BTreeSet<_> = state
        .beliefs
        .values()
        .flat_map(|belief| belief.evidence_ids.iter().cloned())
        .collect();
    let protected: Vec<_> = state
        .objective
        .iter()
        .chain(state.constraints.iter())
        .chain(state.permission.iter())
        .map(|record| {
            serde_json::json!({"id":record.id,"text":record.text,"reason":"Protected mission state","source":"protected"})
        })
        .collect();
    let beliefs: Vec<_> = state
        .beliefs
        .values()
        .map(|belief| {
            serde_json::json!({"id":belief.record_id,"text":belief.statement,"reason":"Current versioned belief","source":"protected"})
        })
        .collect();
    let evidence: Vec<_> = records
        .iter()
        .filter(|record| evidence_ids.contains(&record.id))
        .map(|record| {
            serde_json::json!({"id":record.id,"text":record.text,"reason":"Directly linked to the current belief","source":"evidence_link"})
        })
        .collect();
    let selected_ids: BTreeSet<_> = protected
        .iter()
        .chain(beliefs.iter())
        .chain(evidence.iter())
        .filter_map(|item| item["id"].as_str().map(str::to_owned))
        .collect();
    let history: Vec<_> = records
        .iter()
        .rev()
        .filter(|record| !selected_ids.contains(record.id.as_str()))
        .take(RECENT_LIMIT)
        .map(|record| {
            serde_json::json!({"id":record.id,"text":record.text,"reason":"Recent mission activity","source":"recency"})
        })
        .collect();
    let excluded: Vec<_> = state
        .historical_beliefs
        .iter()
        .map(|belief| {
            serde_json::json!({
                "id": belief.record_id,
                "text": belief.statement,
                "reason": "Superseded belief excluded from current instructions",
                "source": "historical_belief",
            })
        })
        .collect();
    let estimate = |items: &[serde_json::Value]| -> usize {
        items
            .iter()
            .filter_map(|item| item["text"].as_str())
            .map(estimate_tokens)
            .sum()
    };
    let sections = serde_json::json!({
        "protected_state": estimate(&protected),
        "current_beliefs": estimate(&beliefs),
        "supporting_evidence": estimate(&evidence),
        "relevant_history": estimate(&history),
    });
    let total = sections
        .as_object()
        .unwrap()
        .values()
        .filter_map(|value| value.as_u64())
        .sum::<u64>();
    Ok(serde_json::json!({
        "step": step,
        "budget": DEFAULT_TOKEN_BUDGET,
        "total_estimated_tokens": total,
        "token_count_method": "Estimated at four characters per token",
        "selection_method": "Protected state + evidence links + deterministic recency",
        "semantic_retrieval_used": false,
        "model_ranking_used": false,
        "retrieval_source": "append-only Mission Ledger",
        "sections": sections,
        "selected": protected.into_iter().chain(beliefs).chain(evidence).chain(history).collect::<Vec<_>>(),
        "excluded": excluded,
        "superseded_beliefs_excluded": true,
    }))
}

fn redact_and_limit(text: &str) -> String {
    let mut redacted = Vec::new();
    for token in text.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("authorization:")
            || lower.starts_with("sk-")
            || lower.starts_with("bearer")
        {
            redacted.push("[REDACTED]");
        } else {
            redacted.push(token);
        }
    }
    let mut value = redacted.join(" ");
    if value.len() > 8_000 {
        value.truncate(value.floor_char_boundary(8_000));
        value.push_str(" [TRUNCATED]");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(dir.path()).unwrap();
        (dir, ledger)
    }

    #[test]
    fn belief_revisions_are_versioned_and_historical() {
        let (_dir, mut db) = ledger();
        let e1 = db
            .append(MissionRecord::plain(
                EventKind::Observation,
                "query seems slow",
            ))
            .unwrap();
        db.revise_belief("timeout", "database is slow", 0.6, &[e1.id], None)
            .unwrap();
        let profiler = db
            .append(MissionRecord::plain(
                EventKind::ToolResult,
                "profiler: database 20ms, retry waits 5s",
            ))
            .unwrap();
        db.revise_belief(
            "timeout",
            "retry backoff is slow",
            0.9,
            &[profiler.id],
            Some(1),
        )
        .unwrap();
        let state = db.state_at(None);
        assert_eq!(state.beliefs["timeout"].version, 2);
        assert_eq!(state.historical_beliefs[0].statement, "database is slow");
        assert!(
            db.render_context("timeout", 8_000)
                .contains("[v1, superseded] database is slow")
        );
    }

    #[test]
    fn stale_and_evidence_free_revisions_are_rejected() {
        let (_dir, mut db) = ledger();
        assert!(db.revise_belief("x", "y", 0.5, &[], None).is_err());
        let evidence = db
            .append(MissionRecord::plain(EventKind::Observation, "evidence"))
            .unwrap();
        db.revise_belief("x", "first", 0.5, std::slice::from_ref(&evidence.id), None)
            .unwrap();
        assert!(
            db.revise_belief("x", "stale", 0.6, &[evidence.id], None)
                .is_err()
        );
    }

    #[test]
    fn history_reconstructs_prior_state_and_restart_recovers() {
        let (dir, mut db) = ledger();
        let evidence = db
            .append(MissionRecord::plain(
                EventKind::Observation,
                "first evidence",
            ))
            .unwrap();
        let v1 = db
            .revise_belief("cause", "database", 0.5, &[evidence.id], None)
            .unwrap();
        let profiler = db
            .append(MissionRecord::plain(
                EventKind::ToolResult,
                "database only 20ms",
            ))
            .unwrap();
        db.revise_belief("cause", "retry", 0.9, &[profiler.id], Some(1))
            .unwrap();
        assert_eq!(
            db.state_at(Some(v1.step)).beliefs["cause"].statement,
            "database"
        );
        drop(db);
        let reopened = Ledger::open(dir.path()).unwrap();
        assert_eq!(reopened.state_at(None).beliefs["cause"].statement, "retry");
    }

    #[test]
    fn bm25_finds_exact_terms() {
        let (_dir, mut db) = ledger();
        db.append(MissionRecord::plain(
            EventKind::ToolFailure,
            "TimeoutError in src/client.rs retry_loop",
        ))
        .unwrap();
        assert_eq!(db.search("src/client.rs retry_loop", 8).len(), 1);
    }

    #[test]
    fn hybrid_search_fuses_keyword_and_semantic_sources() {
        let (_dir, mut db) = ledger();
        db.append(MissionRecord::plain(
            EventKind::Observation,
            "requests pause between repeated attempts",
        ))
        .unwrap();
        let candidates = db.hybrid_candidates("retry backoff delay", 10);
        assert!(!candidates.is_empty());
        assert!(
            candidates
                .iter()
                .any(|c| c.sources.iter().any(|s| s == "semantic"))
        );
    }

    #[test]
    fn gate_falls_back_without_remote_credentials() {
        let (_dir, mut db) = ledger();
        db.append(MissionRecord::plain(
            EventKind::Observation,
            "retry waits five seconds",
        ))
        .unwrap();
        let candidates = db.hybrid_candidates("retry", 30);
        let result = ranker::select(&db, "retry", &candidates, None, None, "sonnet");
        assert_eq!(result.method, "deterministic_rrf_fallback");
        assert!(result.selected.len() <= 12);
    }

    #[test]
    fn unittest_outcomes_and_mission_brief_come_from_ledger_text() {
        let (_dir, mut db) = ledger();
        db.append(MissionRecord::plain(
            EventKind::Objective,
            "Fix the timeout",
        ))
        .unwrap();
        let baseline = db
            .append(MissionRecord::plain(
                EventKind::ToolResult,
                "Ran 7 tests in 0.001s\n\nFAILED (failures=1, errors=2)",
            ))
            .unwrap();
        let evidence = db
            .append(MissionRecord::plain(
                EventKind::ToolResult,
                "profiler: database 20ms, backoff slept 120s",
            ))
            .unwrap();
        db.revise_belief(
            "cause",
            "retry backoff is the timeout",
            0.8,
            &[evidence.id.clone()],
            None,
        )
        .unwrap();
        db.append(MissionRecord::plain(
            EventKind::ToolCall,
            "Edit {\"file_path\":\"/tmp/checkout.py\"}",
        ))
        .unwrap();
        db.append(MissionRecord::plain(
            EventKind::ToolResult,
            "test_uses_all_configured_attempts ... ok\n\nRan 7 tests in 0.000s\n\nOK",
        ))
        .unwrap();
        let outcome = parse_unittest_outcome(&baseline.text, baseline.step).unwrap();
        assert_eq!(outcome.ran, 7);
        assert_eq!(outcome.passed, 4);
        let brief = mission_brief(db.records());
        assert_eq!(brief["verified_outcome"]["passed"], 7);
        assert_eq!(brief["verified_outcome"]["ok"], true);
        assert_eq!(brief["comparison"]["raw_event_count"], db.records().len());
        assert!(
            brief["behavioral_effect"]
                .as_str()
                .unwrap()
                .contains("checkout.py")
        );
        let report = context_report(db.records(), db.records().last().unwrap().step).unwrap();
        assert_eq!(report["superseded_beliefs_excluded"], true);
        assert!(report["total_estimated_tokens"].as_u64().unwrap() > 0);
    }

    #[test]
    fn exact_injected_context_and_redirect_are_persisted() {
        let (dir, mut db) = ledger();
        let evidence = db
            .append(MissionRecord::plain(
                EventKind::Observation,
                "profiler disproved the database hypothesis",
            ))
            .unwrap();
        db.revise_belief(
            "cause",
            "retry timing is the cause",
            0.9,
            &[evidence.id],
            None,
        )
        .unwrap();
        let exact = "MISSION\nFix timeout\n\nCURRENT BELIEFS\n- retry timing";
        let snapshot = db
            .record_context_snapshot(2, exact, "deterministic", false, "sonnet")
            .unwrap();
        assert_eq!(snapshot.context, exact);
        assert!(snapshot.context_hash.starts_with("fnv1a64:"));
        let loaded = load_context_snapshots(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].context, exact);
        let redirect = db
            .maybe_record_redirect("Edit {\"file_path\":\"/tmp/retry.py\"}")
            .unwrap()
            .unwrap();
        assert_eq!(redirect.kind, EventKind::DecisionRedirect);
        assert!(redirect.text.contains("retry.py"));
        assert!(
            db.maybe_record_redirect("Edit {\"file_path\":\"/tmp/again.py\"}")
                .unwrap()
                .is_none()
        );
    }
}
