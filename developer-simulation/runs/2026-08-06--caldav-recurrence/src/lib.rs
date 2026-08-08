use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{
    DateTime, Datelike, Days, Duration, FixedOffset, NaiveDate, NaiveDateTime, SecondsFormat, Utc,
    Weekday,
};
use fold::pipeline::terminal;
use fold::stream::KeyedStream;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub type EventStore = KeyedStream<String, Event, terminal::Table<String, Event>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub uid: String,
    pub kind: String,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub tzid: Option<String>,
    #[serde(default)]
    pub rrule: Option<String>,
    #[serde(default)]
    pub exdate: Vec<String>,
    #[serde(default)]
    pub overrides: Vec<Override>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Override {
    pub recurrence_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub tzid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Edit {
    pub uid: String,
    #[serde(default)]
    pub delete: bool,
    #[serde(default)]
    pub event: Option<Event>,
}

#[derive(Debug, Clone, Deserialize)]
struct TransitionFile {
    zones: BTreeMap<String, ZoneInput>,
}

#[derive(Debug, Clone, Deserialize)]
struct ZoneInput {
    initial_offset_seconds: i32,
    #[serde(default)]
    transitions: Vec<TransitionInput>,
}

#[derive(Debug, Clone, Deserialize)]
struct TransitionInput {
    at_utc: String,
    offset_after_seconds: i32,
}

#[derive(Debug, Clone)]
struct Transition {
    at_utc: i64,
    offset_before: i32,
    offset_after: i32,
}

#[derive(Debug, Clone)]
struct Zone {
    initial_offset: i32,
    transitions: Vec<Transition>,
}

#[derive(Debug, Clone)]
struct ZoneBook {
    zones: BTreeMap<String, Zone>,
}

impl ZoneBook {
    fn parse(raw: &[u8]) -> Result<Self, String> {
        let input: TransitionFile = serde_json::from_slice(raw)
            .map_err(|_| "transition file is not valid JSON".to_string())?;
        if input.zones.is_empty() {
            return Err("transition file has no zones".to_string());
        }

        let mut zones = BTreeMap::new();
        for (name, zone_input) in input.zones {
            if name.is_empty() || name.len() > 128 {
                return Err("transition file has an invalid zone name".to_string());
            }
            validate_offset(zone_input.initial_offset_seconds)?;
            let mut transitions = Vec::with_capacity(zone_input.transitions.len());
            let mut previous_at = None;
            let mut previous_offset = zone_input.initial_offset_seconds;
            for transition in zone_input.transitions {
                let at_utc = parse_utc_seconds(&transition.at_utc)
                    .map_err(|_| "transition file has an invalid transition instant".to_string())?;
                validate_offset(transition.offset_after_seconds)?;
                if previous_at.is_some_and(|value| at_utc <= value) {
                    return Err("transition table is not strictly ordered".to_string());
                }
                transitions.push(Transition {
                    at_utc,
                    offset_before: previous_offset,
                    offset_after: transition.offset_after_seconds,
                });
                previous_at = Some(at_utc);
                previous_offset = transition.offset_after_seconds;
            }
            zones.insert(
                name,
                Zone {
                    initial_offset: zone_input.initial_offset_seconds,
                    transitions,
                },
            );
        }
        Ok(ZoneBook { zones })
    }

    fn require(&self, name: &str) -> Result<&Zone, String> {
        if name == "UTC" {
            return Err(
                "UTC is implicit and must not be supplied as a transition zone".to_string(),
            );
        }
        self.zones
            .get(name)
            .ok_or_else(|| "event references a zone absent from the transition file".to_string())
    }
}

impl Zone {
    fn offset_at(&self, utc_seconds: i64) -> i32 {
        let mut low = 0usize;
        let mut high = self.transitions.len();
        while low < high {
            let middle = (low + high) / 2;
            if self.transitions[middle].at_utc <= utc_seconds {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low == 0 {
            self.initial_offset
        } else {
            self.transitions[low - 1].offset_after
        }
    }

    fn local_to_utc(&self, local: NaiveDateTime) -> Result<i64, String> {
        let local_seconds = local.and_utc().timestamp();
        let mut offsets = Vec::with_capacity(6);
        push_unique(&mut offsets, self.initial_offset);
        push_unique(&mut offsets, self.offset_at(local_seconds));

        // Only transitions near the local wall time can create a gap or fold.
        // This keeps expansion independent of the number of historical rows in
        // a supplied table while still considering both sides of a transition.
        let lower = local_seconds.saturating_sub(172_800);
        let upper = local_seconds.saturating_add(172_800);
        let mut index = self.first_transition_at_or_after(lower);
        if index > 0 {
            let transition = &self.transitions[index - 1];
            push_unique(&mut offsets, transition.offset_before);
            push_unique(&mut offsets, transition.offset_after);
        }
        while index < self.transitions.len() && self.transitions[index].at_utc <= upper {
            let transition = &self.transitions[index];
            push_unique(&mut offsets, transition.offset_before);
            push_unique(&mut offsets, transition.offset_after);
            index += 1;
        }

        let mut candidates = Vec::new();
        for offset in offsets {
            let candidate = local_seconds - i64::from(offset);
            if self.offset_at(candidate) == offset {
                candidates.push(candidate);
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        if let Some(earliest) = candidates.first() {
            // A fold has two valid instants. Choosing the earlier one is
            // deterministic and corresponds to the pre-transition side.
            return Ok(*earliest);
        }

        for transition in &self.transitions {
            if transition.offset_after > transition.offset_before {
                let gap_start = transition.at_utc + i64::from(transition.offset_before);
                let gap_end = transition.at_utc + i64::from(transition.offset_after);
                if (gap_start..gap_end).contains(&local_seconds) {
                    // Shift a nonexistent wall time forward by the gap.
                    return Ok(local_seconds - i64::from(transition.offset_before));
                }
            }
        }
        Err("local time is not representable by the supplied transition table".to_string())
    }

    fn first_transition_at_or_after(&self, value: i64) -> usize {
        let mut low = 0usize;
        let mut high = self.transitions.len();
        while low < high {
            let middle = (low + high) / 2;
            if self.transitions[middle].at_utc < value {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }
}

fn push_unique(values: &mut Vec<i32>, value: i32) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn validate_offset(value: i32) -> Result<(), String> {
    if !(-86_400..=86_400).contains(&value) {
        Err("transition table contains an invalid UTC offset".to_string())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TimeMode {
    Utc,
    Zone(String),
}

#[derive(Debug, Clone)]
enum EventPlan {
    Timed {
        uid: String,
        start: NaiveDateTime,
        end: NaiveDateTime,
        mode: TimeMode,
        rule: Option<Rule>,
        exdates: BTreeSet<String>,
        overrides: BTreeMap<String, OverridePlan>,
    },
    AllDay {
        uid: String,
        start: NaiveDate,
        end: NaiveDate,
        rule: Option<Rule>,
        exdates: BTreeSet<String>,
        overrides: BTreeMap<String, OverridePlan>,
    },
}

#[derive(Debug, Clone)]
struct OverridePlan {
    cancelled: bool,
    timed_start: Option<NaiveDateTime>,
    timed_end: Option<NaiveDateTime>,
    all_day_start: Option<NaiveDate>,
    all_day_end: Option<NaiveDate>,
}

#[derive(Debug, Clone)]
struct Rule {
    frequency: Frequency,
    interval: i64,
    count: Option<u64>,
    until: Option<Until>,
    byday: Vec<Weekday>,
    bymonthday: Vec<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frequency {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone)]
enum Until {
    Date(NaiveDate),
    DateTime(NaiveDateTime),
}

#[derive(Debug, Clone)]
struct Window {
    from_utc: i64,
    to_utc: i64,
    from_date: NaiveDate,
    to_date_exclusive: NaiveDate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Occurrence {
    pub uid: String,
    pub recurrence_id: String,
    pub kind: String,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    context_hash: u64,
    shards: BTreeMap<String, ShardMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ShardMeta {
    event_hash: u64,
    shard: String,
    #[serde(default)]
    byte_length: u64,
    #[serde(default)]
    content_hash: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub events: PathBuf,
    pub transitions: PathBuf,
    pub from: String,
    pub to: String,
    pub output: PathBuf,
    pub state_dir: PathBuf,
    pub edits: Option<PathBuf>,
    pub crash_after_uid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub events: usize,
    pub occurrences: usize,
    pub rebuilt_uids: usize,
    pub reused_uids: usize,
    pub removed_uids: usize,
    pub resumed: bool,
    pub elapsed_ms: u128,
    pub peak_rss_bytes: Option<u64>,
    pub publication: &'static str,
}

pub fn run(config: &Config) -> Result<RunResult, String> {
    let started = Instant::now();
    let event_values = read_jsonl::<Event>(&config.events, "events")?;
    let transition_raw =
        fs::read(&config.transitions).map_err(|_| "could not read transition file".to_string())?;
    let zones = ZoneBook::parse(&transition_raw)?;
    let window = parse_window(&config.from, &config.to)?;

    let mut desired = BTreeMap::new();
    for event in event_values {
        if desired.insert(event.uid.clone(), event).is_some() {
            return Err("events contain a duplicate UID".to_string());
        }
    }
    if let Some(edits_path) = &config.edits {
        let edits = read_jsonl::<Edit>(edits_path, "edits")?;
        let mut edit_uids = BTreeSet::new();
        for edit in edits {
            if edit.uid.is_empty() || !edit_uids.insert(edit.uid.clone()) {
                return Err("edits contain an invalid or duplicate UID".to_string());
            }
            if edit.delete == edit.event.is_some() {
                return Err("each edit must be exactly one of delete or replacement".to_string());
            }
            if let Some(event) = edit.event {
                if event.uid != edit.uid {
                    return Err("edit UID does not match replacement event".to_string());
                }
                build_plan(&event, &zones)?;
                desired.insert(edit.uid, event);
            } else {
                desired.remove(&edit.uid);
            }
        }
    }

    // Validate expansion before touching the durable event store. If a
    // recurrence cannot be expanded, the previous authoritative snapshot and
    // published output remain unchanged.
    for event in desired.values() {
        let plan = build_plan(event, &zones)?;
        expand_plan(&plan, &window, &zones)?;
    }

    fs::create_dir_all(&config.state_dir)
        .map_err(|_| "could not create state directory".to_string())?;
    if let Some(parent) = config.output.parent() {
        fs::create_dir_all(parent).map_err(|_| "could not create output directory".to_string())?;
    }
    let shard_dir = config.state_dir.join("shards");
    fs::create_dir_all(&shard_dir).map_err(|_| "could not create shard directory".to_string())?;

    let store_path = config.state_dir.join("event-store");
    let mut store = EventStore::new(&store_path, terminal::Table::new("events"));
    let existing_uids = store.rtx(|table| table.iter().map(|(uid, _)| uid).collect::<Vec<_>>());
    let mut removed_uids = 0;
    store.wtx(|tx| {
        for uid in &existing_uids {
            if !desired.contains_key(uid) {
                tx.remove(uid);
                removed_uids += 1;
            }
        }
        for (uid, event) in &desired {
            tx.upsert(uid, event);
        }
    });
    store.checkpoint();

    let mut current_events = store.rtx(|table| table.iter().collect::<Vec<(String, Event)>>());
    current_events.sort_by(|a, b| a.0.cmp(&b.0));

    let manifest_path = config.state_dir.join("manifest.json");
    let previous_manifest = read_manifest(&manifest_path)?;
    let context_hash = hash_context(&transition_raw, &config.from, &config.to);
    let resumed = previous_manifest.is_some();
    let mut next_manifest = Manifest {
        version: 1,
        context_hash,
        shards: BTreeMap::new(),
    };
    let mut publication = PublicationWriter::new(&config.output)?;
    let mut rebuilt_uids = 0;
    let mut reused_uids = 0;
    let mut occurrence_count = 0;

    for (uid, event) in &current_events {
        let event_hash = hash_bytes(&serde_json::to_vec(event).unwrap_or_default());
        let shard_name = format!("{}.jsonl", hex(uid.as_bytes()));
        let shard_path = shard_dir.join(&shard_name);
        let reusable = previous_manifest.as_ref().is_some_and(|manifest| {
            manifest.version == 1
                && manifest.context_hash == context_hash
                && manifest.shards.get(uid).is_some_and(|meta| {
                    meta.event_hash == event_hash && verify_shard_metadata(&shard_path, meta)
                })
        });

        let occurrences = if reusable {
            reused_uids += 1;
            let occurrences = read_occurrences(&shard_path)?;
            validate_shard(&occurrences, uid)?;
            publication.append_shard(&shard_path)?;
            occurrences
        } else {
            rebuilt_uids += 1;
            let plan = build_plan(event, &zones)?;
            let occurrences = expand_plan(&plan, &window, &zones)?;
            validate_shard(&occurrences, uid)?;
            write_shard_and_append(&shard_path, &occurrences, &mut publication)?;
            if config.crash_after_uid.as_deref() == Some(uid.as_str()) {
                return Err("simulated interruption after one shard commit".to_string());
            }
            occurrences
        };

        let (byte_length, content_hash) = fingerprint_file(&shard_path)?;

        next_manifest.shards.insert(
            uid.clone(),
            ShardMeta {
                event_hash,
                shard: shard_name,
                byte_length,
                content_hash,
            },
        );
        occurrence_count += occurrences.len();
    }

    publication.finish()?;
    atomic_write_json(&manifest_path, &next_manifest)?;

    Ok(RunResult {
        events: current_events.len(),
        occurrences: occurrence_count,
        rebuilt_uids,
        reused_uids,
        removed_uids,
        resumed,
        elapsed_ms: started.elapsed().as_millis(),
        peak_rss_bytes: process_max_rss_bytes(),
        publication: "atomic-rename",
    })
}

#[cfg(unix)]
fn process_max_rss_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage initializes the rusage structure when it returns 0.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        return None;
    }
    // macOS reports bytes; Linux and the other Unix targets report KiB.
    let raw = u64::try_from(unsafe { usage.assume_init() }.ru_maxrss).ok()?;
    #[cfg(target_os = "linux")]
    {
        raw.checked_mul(1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Some(raw)
    }
}

#[cfg(not(unix))]
fn process_max_rss_bytes() -> Option<u64> {
    None
}

fn read_jsonl<T: DeserializeOwned>(path: &Path, label: &str) -> Result<Vec<T>, String> {
    let file = File::open(path).map_err(|_| format!("could not read {label} input"))?;
    let reader = BufReader::new(file);
    let mut values = Vec::new();
    for (line_number, line) in reader.lines().enumerate() {
        let line = line.map_err(|_| format!("could not read {label} input"))?;
        if line.trim().is_empty() {
            continue;
        }
        values.push(serde_json::from_str(&line).map_err(|_| {
            format!(
                "{label} input has malformed JSON at line {}",
                line_number + 1
            )
        })?);
    }
    Ok(values)
}

fn parse_window(from: &str, to: &str) -> Result<Window, String> {
    let from_utc =
        parse_utc_seconds(from).map_err(|_| "query start is not RFC3339 UTC".to_string())?;
    let to_utc = parse_utc_seconds(to).map_err(|_| "query end is not RFC3339 UTC".to_string())?;
    if from_utc >= to_utc {
        return Err("query window must have a positive duration".to_string());
    }
    let from_date = DateTime::<Utc>::from_timestamp(from_utc, 0)
        .ok_or_else(|| "query start is out of range".to_string())?
        .date_naive();
    let to_date = DateTime::<Utc>::from_timestamp(to_utc, 0)
        .ok_or_else(|| "query end is out of range".to_string())?
        .date_naive();
    let to_date_exclusive = if to_utc.rem_euclid(86_400) == 0 {
        to_date
    } else {
        to_date
            .checked_add_days(Days::new(1))
            .ok_or_else(|| "query end date is out of range".to_string())?
    };
    Ok(Window {
        from_utc,
        to_utc,
        from_date,
        to_date_exclusive,
    })
}

fn build_plan(event: &Event, zones: &ZoneBook) -> Result<EventPlan, String> {
    if event.uid.is_empty() || event.uid.len() > 512 || event.uid.chars().any(char::is_control) {
        return Err("event has an invalid UID".to_string());
    }
    let rule = event.rrule.as_deref().map(parse_rule).transpose()?;
    match event.kind.as_str() {
        "timed" => build_timed_plan(event, zones, rule),
        "all_day" => build_all_day_plan(event, rule),
        _ => Err("event kind is unsupported".to_string()),
    }
}

fn build_timed_plan(
    event: &Event,
    zones: &ZoneBook,
    rule: Option<Rule>,
) -> Result<EventPlan, String> {
    let mode = match event.tzid.as_deref() {
        Some("UTC") => TimeMode::Utc,
        Some(name) => {
            zones.require(name)?;
            TimeMode::Zone(name.to_string())
        }
        None => {
            zones.require("FLOATING")?;
            TimeMode::Zone("FLOATING".to_string())
        }
    };
    let start = parse_timed_for_mode(&event.start, &mode)?;
    let end = parse_timed_for_mode(&event.end, &mode)?;
    if start >= end {
        return Err("timed event end must be after start".to_string());
    }

    let mut exdates = BTreeSet::new();
    for value in &event.exdate {
        let parsed = parse_timed_for_mode(value, &mode)?;
        exdates.insert(format_recurrence_id(parsed, &mode));
    }
    let mut overrides = BTreeMap::new();
    for value in &event.overrides {
        let recurrence_id = canonical_timed_id(&value.recurrence_id, &mode)?;
        if overrides.contains_key(&recurrence_id) {
            return Err("event has duplicate occurrence overrides".to_string());
        }
        let status = value.status.as_deref().unwrap_or("confirmed");
        if status != "confirmed" && status != "cancelled" {
            return Err("occurrence override status is unsupported".to_string());
        }
        if value.tzid.as_deref().is_some_and(|tzid| {
            Some(tzid)
                != match &mode {
                    TimeMode::Utc => Some("UTC"),
                    TimeMode::Zone(name) => Some(name.as_str()),
                }
        }) {
            return Err("occurrence override changes time zone".to_string());
        }
        if status == "cancelled" {
            if value.start.is_some() || value.end.is_some() {
                return Err("cancelled override must not have start or end".to_string());
            }
            overrides.insert(
                recurrence_id,
                OverridePlan {
                    cancelled: true,
                    timed_start: None,
                    timed_end: None,
                    all_day_start: None,
                    all_day_end: None,
                },
            );
        } else {
            let start_value = value
                .start
                .as_deref()
                .ok_or_else(|| "replacement override is missing start".to_string())?;
            let end_value = value
                .end
                .as_deref()
                .ok_or_else(|| "replacement override is missing end".to_string())?;
            let start_value = parse_timed_for_mode(start_value, &mode)?;
            let end_value = parse_timed_for_mode(end_value, &mode)?;
            if start_value >= end_value {
                return Err("replacement override end must be after start".to_string());
            }
            overrides.insert(
                recurrence_id,
                OverridePlan {
                    cancelled: false,
                    timed_start: Some(start_value),
                    timed_end: Some(end_value),
                    all_day_start: None,
                    all_day_end: None,
                },
            );
        }
    }
    Ok(EventPlan::Timed {
        uid: event.uid.clone(),
        start,
        end,
        mode,
        rule,
        exdates,
        overrides,
    })
}

fn build_all_day_plan(event: &Event, rule: Option<Rule>) -> Result<EventPlan, String> {
    if event.tzid.is_some() {
        return Err("all-day event must not carry a TZID".to_string());
    }
    let start = parse_date(&event.start)?;
    let end = parse_date(&event.end)?;
    if start >= end {
        return Err("all-day event end must be after start".to_string());
    }
    let mut exdates = BTreeSet::new();
    for value in &event.exdate {
        exdates.insert(parse_date(value)?.format("%Y-%m-%d").to_string());
    }
    let mut overrides = BTreeMap::new();
    for value in &event.overrides {
        let recurrence_id = parse_date(&value.recurrence_id)?
            .format("%Y-%m-%d")
            .to_string();
        if overrides.contains_key(&recurrence_id) {
            return Err("event has duplicate occurrence overrides".to_string());
        }
        let status = value.status.as_deref().unwrap_or("confirmed");
        if status != "confirmed" && status != "cancelled" {
            return Err("occurrence override status is unsupported".to_string());
        }
        if value.tzid.is_some() {
            return Err("all-day occurrence override must not carry a TZID".to_string());
        }
        if status == "cancelled" {
            if value.start.is_some() || value.end.is_some() {
                return Err("cancelled override must not have start or end".to_string());
            }
            overrides.insert(
                recurrence_id,
                OverridePlan {
                    cancelled: true,
                    timed_start: None,
                    timed_end: None,
                    all_day_start: None,
                    all_day_end: None,
                },
            );
        } else {
            let replacement_start = parse_date(
                value
                    .start
                    .as_deref()
                    .ok_or_else(|| "replacement override is missing start".to_string())?,
            )?;
            let replacement_end = parse_date(
                value
                    .end
                    .as_deref()
                    .ok_or_else(|| "replacement override is missing end".to_string())?,
            )?;
            if replacement_start >= replacement_end {
                return Err("replacement override end must be after start".to_string());
            }
            overrides.insert(
                recurrence_id,
                OverridePlan {
                    cancelled: false,
                    timed_start: None,
                    timed_end: None,
                    all_day_start: Some(replacement_start),
                    all_day_end: Some(replacement_end),
                },
            );
        }
    }
    Ok(EventPlan::AllDay {
        uid: event.uid.clone(),
        start,
        end,
        rule,
        exdates,
        overrides,
    })
}

fn parse_rule(raw: &str) -> Result<Rule, String> {
    if raw.is_empty() {
        return Err("recurrence rule is empty".to_string());
    }
    let mut fields = BTreeMap::new();
    for part in raw.split(';') {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| "recurrence rule has a malformed field".to_string())?;
        let key = key.to_ascii_uppercase();
        if fields.insert(key, value.to_string()).is_some() {
            return Err("recurrence rule has a duplicate field".to_string());
        }
    }
    let frequency = match fields.remove("FREQ").as_deref() {
        Some("DAILY") => Frequency::Daily,
        Some("WEEKLY") => Frequency::Weekly,
        Some("MONTHLY") => Frequency::Monthly,
        _ => return Err("recurrence frequency is unsupported or missing".to_string()),
    };
    let interval = fields.remove("INTERVAL").map_or(Ok(1), |value| {
        value
            .parse::<i64>()
            .map_err(|_| "recurrence interval is invalid".to_string())
    })?;
    if interval <= 0 {
        return Err("recurrence interval must be positive".to_string());
    }
    let count = fields
        .remove("COUNT")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| "recurrence count is invalid".to_string())
        })
        .transpose()?;
    if count == Some(0) {
        return Err("recurrence count must be positive".to_string());
    }
    let until = fields
        .remove("UNTIL")
        .map(|value| parse_until(&value))
        .transpose()?;
    let byday = fields
        .remove("BYDAY")
        .map(|value| parse_byday(&value))
        .transpose()?
        .unwrap_or_default();
    let bymonthday = fields
        .remove("BYMONTHDAY")
        .map(|value| parse_bymonthday(&value))
        .transpose()?
        .unwrap_or_default();
    if !fields.is_empty() {
        return Err("recurrence rule contains an unsupported field".to_string());
    }
    if frequency != Frequency::Weekly && !byday.is_empty() {
        return Err("BYDAY is supported only for weekly rules".to_string());
    }
    if frequency != Frequency::Monthly && !bymonthday.is_empty() {
        return Err("BYMONTHDAY is supported only for monthly rules".to_string());
    }
    Ok(Rule {
        frequency,
        interval,
        count,
        until,
        byday,
        bymonthday,
    })
}

fn parse_until(value: &str) -> Result<Until, String> {
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y%m%d") {
        return Ok(Until::Date(date));
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok(Until::Date(date));
    }
    let value = value.trim_end_matches('Z');
    let datetime = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
        .map_err(|_| "recurrence UNTIL is invalid".to_string())?;
    Ok(Until::DateTime(datetime))
}

fn parse_byday(value: &str) -> Result<Vec<Weekday>, String> {
    let mut days = Vec::new();
    for token in value.split(',') {
        let day = match token {
            "MO" => Weekday::Mon,
            "TU" => Weekday::Tue,
            "WE" => Weekday::Wed,
            "TH" => Weekday::Thu,
            "FR" => Weekday::Fri,
            "SA" => Weekday::Sat,
            "SU" => Weekday::Sun,
            _ => return Err("recurrence BYDAY is invalid".to_string()),
        };
        if days.contains(&day) {
            return Err("recurrence BYDAY has a duplicate day".to_string());
        }
        days.push(day);
    }
    days.sort_by_key(|day| day.num_days_from_monday());
    Ok(days)
}

fn parse_bymonthday(value: &str) -> Result<Vec<i32>, String> {
    let mut days = Vec::new();
    for token in value.split(',') {
        let day = token
            .parse::<i32>()
            .map_err(|_| "recurrence BYMONTHDAY is invalid".to_string())?;
        if day == 0 || !(-31..=31).contains(&day) {
            return Err("recurrence BYMONTHDAY is out of range".to_string());
        }
        if days.contains(&day) {
            return Err("recurrence BYMONTHDAY has a duplicate day".to_string());
        }
        days.push(day);
    }
    days.sort_unstable();
    Ok(days)
}

fn parse_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(value, "%Y%m%d"))
        .map_err(|_| "date value is invalid".to_string())
}

fn parse_timed_for_mode(value: &str, mode: &TimeMode) -> Result<NaiveDateTime, String> {
    let absolute = DateTime::parse_from_rfc3339(value).ok();
    match (mode, absolute) {
        (TimeMode::Utc, Some(value)) => Ok(value.with_timezone(&Utc).naive_utc()),
        (TimeMode::Utc, None) => NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
            .map_err(|_| "UTC time value is invalid".to_string()),
        (TimeMode::Zone(_), Some(_)) => {
            Err("zoned time must not carry a numeric UTC offset".to_string())
        }
        (TimeMode::Zone(_), None) => NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
            .map_err(|_| "local time value is invalid".to_string()),
    }
}

fn canonical_timed_id(value: &str, mode: &TimeMode) -> Result<String, String> {
    let parsed = parse_timed_for_mode(value, mode)?;
    Ok(format_recurrence_id(parsed, mode))
}

fn format_recurrence_id(value: NaiveDateTime, mode: &TimeMode) -> String {
    let base = value.format("%Y-%m-%dT%H:%M:%S").to_string();
    if *mode == TimeMode::Utc {
        format!("{base}Z")
    } else {
        base
    }
}

fn parse_utc_seconds(value: &str) -> Result<i64, ()> {
    let parsed: DateTime<FixedOffset> = DateTime::parse_from_rfc3339(value).map_err(|_| ())?;
    Ok(parsed.with_timezone(&Utc).timestamp())
}

fn expand_plan(
    plan: &EventPlan,
    window: &Window,
    zones: &ZoneBook,
) -> Result<Vec<Occurrence>, String> {
    match plan {
        EventPlan::Timed {
            uid,
            start,
            end,
            mode,
            rule,
            exdates,
            overrides,
        } => {
            let candidates =
                timed_candidates(*start, *end, mode, rule.as_ref(), window, overrides)?;
            let mut output = Vec::new();
            let mut seen = BTreeSet::new();
            for (recurrence_id, candidate_start, candidate_end) in candidates {
                if !seen.insert(recurrence_id.clone()) {
                    return Err(
                        "recurrence rule produced a duplicate occurrence identity".to_string()
                    );
                }
                if exdates.contains(&recurrence_id) {
                    continue;
                }
                let (candidate_start, candidate_end) = match overrides.get(&recurrence_id) {
                    Some(override_plan) if override_plan.cancelled => continue,
                    Some(override_plan) => (
                        override_plan
                            .timed_start
                            .ok_or_else(|| "timed replacement is incomplete".to_string())?,
                        override_plan
                            .timed_end
                            .ok_or_else(|| "timed replacement is incomplete".to_string())?,
                    ),
                    None => (candidate_start, candidate_end),
                };
                let start_utc = to_utc(candidate_start, mode, zones)?;
                let end_utc = to_utc(candidate_end, mode, zones)?;
                if end_utc <= start_utc {
                    return Err(
                        "occurrence end is not after start after time-zone conversion".to_string(),
                    );
                }
                if start_utc < window.to_utc && end_utc > window.from_utc {
                    output.push(Occurrence {
                        uid: uid.clone(),
                        recurrence_id,
                        kind: "timed".to_string(),
                        start: format_utc(start_utc)?,
                        end: format_utc(end_utc)?,
                    });
                }
            }
            validate_override_identities(&seen, overrides)?;
            output.sort_by(|a, b| a.recurrence_id.cmp(&b.recurrence_id));
            Ok(output)
        }
        EventPlan::AllDay {
            uid,
            start,
            end,
            rule,
            exdates,
            overrides,
        } => {
            let candidates = all_day_candidates(*start, *end, rule.as_ref(), window, overrides)?;
            let mut output = Vec::new();
            let mut seen = BTreeSet::new();
            for (recurrence_id, candidate_start, candidate_end) in candidates {
                if !seen.insert(recurrence_id.clone()) {
                    return Err(
                        "recurrence rule produced a duplicate occurrence identity".to_string()
                    );
                }
                if exdates.contains(&recurrence_id) {
                    continue;
                }
                let (candidate_start, candidate_end) = match overrides.get(&recurrence_id) {
                    Some(override_plan) if override_plan.cancelled => continue,
                    Some(override_plan) => (
                        override_plan
                            .all_day_start
                            .ok_or_else(|| "all-day replacement is incomplete".to_string())?,
                        override_plan
                            .all_day_end
                            .ok_or_else(|| "all-day replacement is incomplete".to_string())?,
                    ),
                    None => (candidate_start, candidate_end),
                };
                if candidate_start < window.to_date_exclusive && candidate_end > window.from_date {
                    output.push(Occurrence {
                        uid: uid.clone(),
                        recurrence_id,
                        kind: "all_day".to_string(),
                        start: candidate_start.format("%Y-%m-%d").to_string(),
                        end: candidate_end.format("%Y-%m-%d").to_string(),
                    });
                }
            }
            validate_override_identities(&seen, overrides)?;
            output.sort_by(|a, b| a.recurrence_id.cmp(&b.recurrence_id));
            Ok(output)
        }
    }
}

fn timed_candidates(
    start: NaiveDateTime,
    end: NaiveDateTime,
    mode: &TimeMode,
    rule: Option<&Rule>,
    window: &Window,
    overrides: &BTreeMap<String, OverridePlan>,
) -> Result<Vec<(String, NaiveDateTime, NaiveDateTime)>, String> {
    let Some(rule) = rule else {
        return Ok(vec![(format_recurrence_id(start, mode), start, end)]);
    };
    let duration = end - start;
    let mut hard_stop = window.to_utc.saturating_add(172_800);
    for id in overrides.keys() {
        let value = parse_timed_for_mode(id, mode)?;
        hard_stop = hard_stop.max(value.and_utc().timestamp().saturating_add(172_800));
    }
    match rule.frequency {
        Frequency::Daily => {
            let mut output = Vec::new();
            let mut date = start.date();
            let mut accepted = 0u64;
            loop {
                let candidate = date.and_time(start.time());
                if candidate.and_utc().timestamp() > hard_stop {
                    break;
                }
                if until_exceeded(rule.until.as_ref(), candidate, false) {
                    break;
                }
                if rule.byday.is_empty() || rule.byday.contains(&candidate.weekday()) {
                    accepted += 1;
                    output.push((
                        format_recurrence_id(candidate, mode),
                        candidate,
                        candidate + duration,
                    ));
                    if rule.count.is_some_and(|count| accepted >= count) {
                        break;
                    }
                }
                date = add_days(date, rule.interval)?;
            }
            Ok(output)
        }
        Frequency::Weekly => {
            let first_monday = add_days(
                start.date(),
                -(i64::from(start.date().weekday().num_days_from_monday())),
            )?;
            let weekdays = if rule.byday.is_empty() {
                vec![start.date().weekday()]
            } else {
                rule.byday.clone()
            };
            let mut output = Vec::new();
            let mut week_index = 0i64;
            let mut accepted = 0u64;
            loop {
                let week_start = add_days(first_monday, week_index.saturating_mul(7))?;
                for weekday in &weekdays {
                    let candidate_date =
                        add_days(week_start, i64::from(weekday.num_days_from_monday()))?;
                    let candidate = candidate_date.and_time(start.time());
                    if candidate < start {
                        continue;
                    }
                    if candidate.and_utc().timestamp() > hard_stop {
                        return Ok(output);
                    }
                    if until_exceeded(rule.until.as_ref(), candidate, false) {
                        return Ok(output);
                    }
                    accepted += 1;
                    output.push((
                        format_recurrence_id(candidate, mode),
                        candidate,
                        candidate + duration,
                    ));
                    if rule.count.is_some_and(|count| accepted >= count) {
                        return Ok(output);
                    }
                }
                week_index = week_index
                    .checked_add(rule.interval)
                    .ok_or_else(|| "recurrence is too large".to_string())?;
            }
        }
        Frequency::Monthly => {
            let mut month_index = 0i64;
            let mut output = Vec::new();
            let mut accepted = 0u64;
            loop {
                let (year, month) = add_months(start.year(), start.month(), month_index)?;
                let days = if rule.bymonthday.is_empty() {
                    vec![i32::try_from(start.day()).unwrap_or(0)]
                } else {
                    rule.bymonthday.clone()
                };
                for day in days {
                    let Some(candidate_date) = month_day(year, month, day) else {
                        continue;
                    };
                    let candidate = candidate_date.and_time(start.time());
                    if candidate < start {
                        continue;
                    }
                    if candidate.and_utc().timestamp() > hard_stop {
                        return Ok(output);
                    }
                    if until_exceeded(rule.until.as_ref(), candidate, false) {
                        return Ok(output);
                    }
                    accepted += 1;
                    output.push((
                        format_recurrence_id(candidate, mode),
                        candidate,
                        candidate + duration,
                    ));
                    if rule.count.is_some_and(|count| accepted >= count) {
                        return Ok(output);
                    }
                }
                month_index = month_index
                    .checked_add(rule.interval)
                    .ok_or_else(|| "recurrence is too large".to_string())?;
            }
        }
    }
}

fn all_day_candidates(
    start: NaiveDate,
    end: NaiveDate,
    rule: Option<&Rule>,
    window: &Window,
    overrides: &BTreeMap<String, OverridePlan>,
) -> Result<Vec<(String, NaiveDate, NaiveDate)>, String> {
    let Some(rule) = rule else {
        return Ok(vec![(start.format("%Y-%m-%d").to_string(), start, end)]);
    };
    let duration = end - start;
    let mut hard_stop = window.to_date_exclusive;
    for id in overrides.keys() {
        hard_stop = hard_stop.max(parse_date(id)?);
    }
    let mut output = Vec::new();
    let mut accepted = 0u64;
    match rule.frequency {
        Frequency::Daily => {
            let mut date = start;
            loop {
                if date > hard_stop {
                    break;
                }
                if until_exceeded(
                    rule.until.as_ref(),
                    date.and_time(chrono::NaiveTime::MIN),
                    true,
                ) {
                    break;
                }
                if rule.byday.is_empty() || rule.byday.contains(&date.weekday()) {
                    accepted += 1;
                    output.push((
                        date.format("%Y-%m-%d").to_string(),
                        date,
                        date.checked_add_signed(duration)
                            .ok_or_else(|| "date range is too large".to_string())?,
                    ));
                    if rule.count.is_some_and(|count| accepted >= count) {
                        break;
                    }
                }
                date = add_days(date, rule.interval)?;
            }
        }
        Frequency::Weekly => {
            let first_monday =
                add_days(start, -(i64::from(start.weekday().num_days_from_monday())))?;
            let weekdays = if rule.byday.is_empty() {
                vec![start.weekday()]
            } else {
                rule.byday.clone()
            };
            let mut week_index = 0i64;
            'weeks: loop {
                let week_start = add_days(first_monday, week_index.saturating_mul(7))?;
                for weekday in &weekdays {
                    let date = add_days(week_start, i64::from(weekday.num_days_from_monday()))?;
                    if date < start {
                        continue;
                    }
                    if date > hard_stop {
                        break 'weeks;
                    }
                    if until_exceeded(
                        rule.until.as_ref(),
                        date.and_time(chrono::NaiveTime::MIN),
                        true,
                    ) {
                        break 'weeks;
                    }
                    accepted += 1;
                    output.push((
                        date.format("%Y-%m-%d").to_string(),
                        date,
                        date.checked_add_signed(duration)
                            .ok_or_else(|| "date range is too large".to_string())?,
                    ));
                    if rule.count.is_some_and(|count| accepted >= count) {
                        break 'weeks;
                    }
                }
                week_index = week_index
                    .checked_add(rule.interval)
                    .ok_or_else(|| "recurrence is too large".to_string())?;
            }
        }
        Frequency::Monthly => {
            let mut month_index = 0i64;
            loop {
                let (year, month) = add_months(start.year(), start.month(), month_index)?;
                let days = if rule.bymonthday.is_empty() {
                    vec![i32::try_from(start.day()).unwrap_or(0)]
                } else {
                    rule.bymonthday.clone()
                };
                for day in days {
                    let Some(date) = month_day(year, month, day) else {
                        continue;
                    };
                    if date < start {
                        continue;
                    }
                    if date > hard_stop {
                        return Ok(output);
                    }
                    if until_exceeded(
                        rule.until.as_ref(),
                        date.and_time(chrono::NaiveTime::MIN),
                        true,
                    ) {
                        return Ok(output);
                    }
                    accepted += 1;
                    output.push((
                        date.format("%Y-%m-%d").to_string(),
                        date,
                        date.checked_add_signed(duration)
                            .ok_or_else(|| "date range is too large".to_string())?,
                    ));
                    if rule.count.is_some_and(|count| accepted >= count) {
                        return Ok(output);
                    }
                }
                month_index = month_index
                    .checked_add(rule.interval)
                    .ok_or_else(|| "recurrence is too large".to_string())?;
            }
        }
    }
    Ok(output)
}

fn validate_override_identities(
    seen: &BTreeSet<String>,
    overrides: &BTreeMap<String, OverridePlan>,
) -> Result<(), String> {
    if overrides
        .keys()
        .any(|recurrence_id| !seen.contains(recurrence_id))
    {
        return Err("occurrence override does not match a generated recurrence".to_string());
    }
    Ok(())
}

fn until_exceeded(until: Option<&Until>, candidate: NaiveDateTime, _all_day: bool) -> bool {
    match until {
        Some(Until::Date(date)) => candidate.date() > *date,
        Some(Until::DateTime(value)) => candidate > *value,
        None => false,
    }
}

fn to_utc(value: NaiveDateTime, mode: &TimeMode, zones: &ZoneBook) -> Result<i64, String> {
    match mode {
        TimeMode::Utc => Ok(value.and_utc().timestamp()),
        TimeMode::Zone(name) => zones.require(name)?.local_to_utc(value),
    }
}

fn format_utc(seconds: i64) -> Result<String, String> {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
        .ok_or_else(|| "UTC occurrence is out of range".to_string())
}

fn add_days(date: NaiveDate, days: i64) -> Result<NaiveDate, String> {
    if days >= 0 {
        date.checked_add_days(Days::new(days as u64))
    } else {
        date.checked_sub_days(Days::new(days.unsigned_abs()))
    }
    .ok_or_else(|| "recurrence date is out of range".to_string())
}

fn add_months(year: i32, month: u32, delta: i64) -> Result<(i32, u32), String> {
    let base = i64::from(year)
        .checked_mul(12)
        .and_then(|value| value.checked_add(i64::from(month) - 1))
        .and_then(|value| value.checked_add(delta))
        .ok_or_else(|| "recurrence month is out of range".to_string())?;
    let year = base.div_euclid(12);
    let month = base.rem_euclid(12) + 1;
    Ok((
        i32::try_from(year).map_err(|_| "recurrence year is out of range".to_string())?,
        u32::try_from(month).map_err(|_| "recurrence month is out of range".to_string())?,
    ))
}

fn month_day(year: i32, month: u32, requested: i32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    let last_day = i32::try_from((next - Duration::days(1)).day()).ok()?;
    let day = if requested > 0 {
        requested
    } else {
        last_day + requested + 1
    };
    if !(1..=last_day).contains(&day) {
        None
    } else {
        first.with_day(u32::try_from(day).ok()?)
    }
}

fn read_occurrences(path: &Path) -> Result<Vec<Occurrence>, String> {
    read_jsonl(path, "state shard")
}

fn verify_shard_metadata(path: &Path, metadata: &ShardMeta) -> bool {
    fingerprint_file(path)
        .map(|(byte_length, content_hash)| {
            byte_length == metadata.byte_length && content_hash == metadata.content_hash
        })
        .unwrap_or(false)
}

fn fingerprint_file(path: &Path) -> Result<(u64, String), String> {
    let mut file = File::open(path).map_err(|_| "could not read state shard".to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut byte_length = 0u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "could not read state shard".to_string())?;
        if count == 0 {
            break;
        }
        byte_length = byte_length
            .checked_add(u64::try_from(count).map_err(|_| "state shard is too large".to_string())?)
            .ok_or_else(|| "state shard is too large".to_string())?;
        hasher.update(&buffer[..count]);
    }
    Ok((byte_length, hex(&hasher.finalize())))
}

fn validate_shard(occurrences: &[Occurrence], uid: &str) -> Result<(), String> {
    let mut identities = BTreeSet::new();
    for occurrence in occurrences {
        if occurrence.uid != uid || !identities.insert(occurrence.recurrence_id.clone()) {
            return Err("state contains a duplicate or unexpected occurrence identity".to_string());
        }
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<Option<Manifest>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes =
        fs::read(path).map_err(|_| "could not read materialization manifest".to_string())?;
    match serde_json::from_slice(&bytes) {
        Ok(manifest) => Ok(Some(manifest)),
        Err(_) => Ok(None),
    }
}

struct PublicationWriter {
    final_path: PathBuf,
    temp_path: PathBuf,
    writer: BufWriter<File>,
}

impl PublicationWriter {
    fn new(final_path: &Path) -> Result<Self, String> {
        let temp_path = temp_path(final_path);
        remove_stale_temp(&temp_path)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|_| "could not create temporary publication".to_string())?;
        Ok(Self {
            final_path: final_path.to_path_buf(),
            temp_path,
            writer: BufWriter::new(file),
        })
    }

    fn append_shard(&mut self, path: &Path) -> Result<(), String> {
        let shard =
            File::open(path).map_err(|_| "could not read materialization shard".to_string())?;
        for line in BufReader::new(shard).lines() {
            let line = line.map_err(|_| "could not read materialization shard".to_string())?;
            self.writer
                .write_all(line.as_bytes())
                .and_then(|_| self.writer.write_all(b"\n"))
                .map_err(|_| "could not write JSONL publication".to_string())?;
        }
        Ok(())
    }

    fn finish(self) -> Result<(), String> {
        let mut writer = self.writer;
        writer
            .flush()
            .map_err(|_| "could not flush JSONL publication".to_string())?;
        writer
            .into_inner()
            .map_err(|_| "could not finalize JSONL publication".to_string())?
            .sync_all()
            .map_err(|_| "could not sync JSONL publication".to_string())?;
        fs::rename(&self.temp_path, &self.final_path)
            .map_err(|_| "could not atomically publish JSONL output".to_string())
    }
}

fn write_shard_and_append(
    path: &Path,
    occurrences: &[Occurrence],
    publication: &mut PublicationWriter,
) -> Result<(), String> {
    let temp = temp_path(path);
    remove_stale_temp(&temp)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| "could not create temporary materialization shard".to_string())?;
    let mut writer = BufWriter::new(file);
    for occurrence in occurrences {
        let encoded = serde_json::to_vec(occurrence)
            .map_err(|_| "could not encode materialization shard".to_string())?;
        writer
            .write_all(&encoded)
            .and_then(|_| writer.write_all(b"\n"))
            .map_err(|_| "could not write materialization shard".to_string())?;
        publication
            .writer
            .write_all(&encoded)
            .and_then(|_| publication.writer.write_all(b"\n"))
            .map_err(|_| "could not write JSONL publication".to_string())?;
    }
    writer
        .flush()
        .map_err(|_| "could not flush materialization shard".to_string())?;
    fs::rename(&temp, path).map_err(|_| "could not publish materialization shard".to_string())
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let temp = temp_path(path);
    remove_stale_temp(&temp)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| "could not create temporary manifest".to_string())?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|_| "could not encode materialization manifest".to_string())?;
    writer
        .write_all(b"\n")
        .map_err(|_| "could not write materialization manifest".to_string())?;
    writer
        .flush()
        .map_err(|_| "could not flush materialization manifest".to_string())?;
    writer
        .into_inner()
        .map_err(|_| "could not finalize materialization manifest".to_string())?
        .sync_all()
        .map_err(|_| "could not sync materialization manifest".to_string())?;
    fs::rename(&temp, path)
        .map_err(|_| "could not atomically publish materialization manifest".to_string())
}

fn remove_stale_temp(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path)
            .map_err(|_| "could not remove a stale temporary publication".to_string())?;
    }
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()))
}

fn hash_context(transitions: &[u8], from: &str, to: &str) -> u64 {
    let mut data = Vec::with_capacity(transitions.len() + from.len() + to.len() + 2);
    data.extend_from_slice(transitions);
    data.push(0);
    data.extend_from_slice(from.as_bytes());
    data.push(0);
    data.extend_from_slice(to.as_bytes());
    hash_bytes(&data)
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

pub fn format_diagnostics(result: &RunResult) -> String {
    serde_json::to_string(result).unwrap_or_else(|_| "{\"publication\":\"failed\"}".to_string())
}
