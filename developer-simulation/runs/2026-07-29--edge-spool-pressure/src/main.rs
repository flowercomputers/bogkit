use std::collections::{BTreeMap, HashSet, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fold::pipeline::terminal::{self, Bag, Table};
use fold::pipeline::{Aggregate, FilterMap, KeyBy};
use fold::stream::Stream;
use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;
const BASELINE_CAPACITY: u64 = 256 * MIB;
const BASELINE_FILE_SIZE: u64 = 4 * MIB;
const MILLION: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
enum Priority {
    Critical = 0,
    Operational = 1,
    Debug = 2,
}

impl Priority {
    fn name(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Operational => "operational",
            Self::Debug => "debug",
        }
    }

    fn from_rank(rank: u8) -> Self {
        match rank {
            0 => Self::Critical,
            1 => Self::Operational,
            _ => Self::Debug,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
enum Category {
    Security = 0,
    HardwareFailure = 1,
    Operations = 2,
    Diagnostics = 3,
}

impl Category {
    fn name(self) -> &'static str {
        match self {
            Self::Security => "security",
            Self::HardwareFailure => "hardware_failure",
            Self::Operations => "operations",
            Self::Diagnostics => "diagnostics",
        }
    }

    fn from_rank(rank: u8) -> Self {
        match rank {
            0 => Self::Security,
            1 => Self::HardwareFailure,
            2 => Self::Operations,
            _ => Self::Diagnostics,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
enum DropReason {
    IncomingLowerPriority = 0,
    EvictedForHigherPriority = 1,
    EventExceedsLogicalQuota = 2,
}

impl DropReason {
    fn name(self) -> &'static str {
        match self {
            Self::IncomingLowerPriority => "incoming_lower_priority",
            Self::EvictedForHigherPriority => "evicted_for_higher_priority",
            Self::EventExceedsLogicalQuota => "event_exceeds_logical_quota",
        }
    }

    fn from_rank(rank: u8) -> Self {
        match rank {
            0 => Self::IncomingLowerPriority,
            1 => Self::EvictedForHigherPriority,
            _ => Self::EventExceedsLogicalQuota,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Event {
    // This field is first so Bag's documented postcard ordering yields
    // critical, then operational, then debug records.
    priority_rank: u8,
    id_be: [u8; 8],
    gateway_id: [u8; 8],
    timestamp_ms_be: [u8; 8],
    category_rank: u8,
    accounted_bytes: u32,
    payload: Vec<u8>,
}

impl Event {
    fn id(&self) -> u64 {
        u64::from_be_bytes(self.id_be)
    }

    fn priority(&self) -> Priority {
        Priority::from_rank(self.priority_rank)
    }

    fn category(&self) -> Category {
        Category::from_rank(self.category_rank)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UploadIntent {
    attempt: u64,
    events: Vec<Event>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct QueueKey {
    priority_rank: u8,
    category_rank: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct QueueStats {
    count: i64,
    bytes: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct DropFact {
    priority_rank: u8,
    category_rank: u8,
    reason_rank: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct DropKey {
    priority_rank: u8,
    category_rank: u8,
    reason_rank: u8,
}

#[derive(Debug, Clone)]
enum Mutation {
    Queue(Event),
    Drop(DropFact),
    Upload(UploadIntent),
}

fn mutation_event(value: &Mutation) -> Option<Event> {
    match value {
        Mutation::Queue(event) => Some(event.clone()),
        _ => None,
    }
}

fn mutation_drop(value: &Mutation) -> Option<DropFact> {
    match value {
        Mutation::Drop(drop) => Some(*drop),
        _ => None,
    }
}

fn mutation_upload(value: &Mutation) -> Option<UploadIntent> {
    match value {
        Mutation::Upload(intent) => Some(intent.clone()),
        _ => None,
    }
}

fn queue_key(event: &Event) -> QueueKey {
    QueueKey {
        priority_rank: event.priority_rank,
        category_rank: event.category_rank,
    }
}

fn queue_step(stats: &mut QueueStats, event: &Event, delta: isize) {
    stats.count += delta as i64;
    stats.bytes += i64::from(event.accounted_bytes) * delta as i64;
}

fn drop_key(drop: &DropFact) -> DropKey {
    DropKey {
        priority_rank: drop.priority_rank,
        category_rank: drop.category_rank,
        reason_rank: drop.reason_rank,
    }
}

fn drop_step(count: &mut i64, _drop: &DropFact, delta: isize) {
    *count += delta as i64;
}

type EventBranch = FilterMap<fn(&Mutation) -> Option<Event>, Bag<Event>, Mutation, Event>;
type QueueAggregate = Aggregate<
    QueueKey,
    Event,
    QueueStats,
    fn(&mut QueueStats, &Event, isize),
    Table<QueueKey, QueueStats>,
>;
type QueueBranch = FilterMap<
    fn(&Mutation) -> Option<Event>,
    KeyBy<fn(&Event) -> QueueKey, QueueAggregate, QueueKey, Event>,
    Mutation,
    Event,
>;
type DropAggregate =
    Aggregate<DropKey, DropFact, i64, fn(&mut i64, &DropFact, isize), Table<DropKey, i64>>;
type DropBranch = FilterMap<
    fn(&Mutation) -> Option<DropFact>,
    KeyBy<fn(&DropFact) -> DropKey, DropAggregate, DropKey, DropFact>,
    Mutation,
    DropFact,
>;
type IntentBranch =
    FilterMap<fn(&Mutation) -> Option<UploadIntent>, Bag<UploadIntent>, Mutation, UploadIntent>;
type SpoolPipeline = (EventBranch, QueueBranch, DropBranch, IntentBranch);

fn pipeline() -> SpoolPipeline {
    (
        FilterMap::new(
            mutation_event as fn(&Mutation) -> Option<Event>,
            terminal::Bag::new("queued_events"),
        ),
        FilterMap::new(
            mutation_event as fn(&Mutation) -> Option<Event>,
            KeyBy::new(
                queue_key as fn(&Event) -> QueueKey,
                Aggregate::new(
                    "queue_stats_aggregate",
                    queue_step as fn(&mut QueueStats, &Event, isize),
                    terminal::Table::new("queue_stats"),
                ),
            ),
        ),
        FilterMap::new(
            mutation_drop as fn(&Mutation) -> Option<DropFact>,
            KeyBy::new(
                drop_key as fn(&DropFact) -> DropKey,
                Aggregate::new(
                    "drop_counts_aggregate",
                    drop_step as fn(&mut i64, &DropFact, isize),
                    terminal::Table::new("drop_counts"),
                ),
            ),
        ),
        FilterMap::new(
            mutation_upload as fn(&Mutation) -> Option<UploadIntent>,
            terminal::Bag::new("upload_intents"),
        ),
    )
}

struct Spool {
    stream: Stream<Mutation, SpoolPipeline>,
    logical_quota: u64,
}

#[derive(Debug, Default)]
struct SpoolSnapshot {
    retained_count: u64,
    logical_bytes: u64,
    retained_by_priority: BTreeMap<&'static str, u64>,
    drops: BTreeMap<String, u64>,
    intent: Option<UploadIntent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnqueueOutcome {
    Retained,
    Dropped,
}

impl Spool {
    fn open(path: &Path, logical_quota: u64) -> Self {
        Self {
            stream: Stream::new(path, pipeline()),
            logical_quota,
        }
    }

    fn checkpoint(&mut self) {
        self.stream.checkpoint();
    }

    fn snapshot(&self) -> SpoolSnapshot {
        self.stream.rtx(|(_events, queue_stats, drops, intents)| {
            let mut snapshot = SpoolSnapshot::default();
            for (key, stats) in queue_stats.iter() {
                let count = u64::try_from(stats.count).expect("negative queue count");
                let bytes = u64::try_from(stats.bytes).expect("negative queue bytes");
                snapshot.retained_count += count;
                snapshot.logical_bytes += bytes;
                *snapshot
                    .retained_by_priority
                    .entry(Priority::from_rank(key.priority_rank).name())
                    .or_default() += count;
            }
            for (key, count) in drops.iter() {
                let label = format!(
                    "{}/{}/{}",
                    Priority::from_rank(key.priority_rank).name(),
                    Category::from_rank(key.category_rank).name(),
                    DropReason::from_rank(key.reason_rank).name()
                );
                snapshot
                    .drops
                    .insert(label, u64::try_from(count).expect("negative drop count"));
            }
            snapshot.intent = intents.iter().next().map(|(intent, _)| intent);

            snapshot
        })
    }

    fn verify_consistency(&self) {
        let expected = self.snapshot().retained_count;
        let bag_count = self.stream.rtx(|(events, _, _, _)| {
            events
                .iter()
                .map(|(_, multiplicity)| {
                    u64::try_from(multiplicity).expect("negative multiplicity")
                })
                .sum::<u64>()
        });
        assert_eq!(
            bag_count, expected,
            "event Bag and aggregate count diverged"
        );
    }

    fn enqueue(&mut self, event: Event) -> EnqueueOutcome {
        let snapshot = self.snapshot();
        let event_bytes = u64::from(event.accounted_bytes);

        if event_bytes > self.logical_quota {
            self.record_drop(&event, DropReason::EventExceedsLogicalQuota);
            return EnqueueOutcome::Dropped;
        }

        if snapshot.logical_bytes + event_bytes <= self.logical_quota {
            self.stream
                .wtx(|tx| tx.insert(&Mutation::Queue(event.clone())));
            return EnqueueOutcome::Retained;
        }

        let needed = snapshot.logical_bytes + event_bytes - self.logical_quota;
        let eviction_ranks: &[u8] = match event.priority() {
            Priority::Critical => &[Priority::Debug as u8, Priority::Operational as u8],
            Priority::Operational => &[Priority::Debug as u8],
            Priority::Debug => &[],
        };

        let evictions = self.stream.rtx(|(events, _, _, _)| {
            let mut chosen = Vec::new();
            let mut reclaimed = 0_u64;
            for wanted_rank in eviction_ranks {
                for (candidate, multiplicity) in events.iter() {
                    if candidate.priority_rank != *wanted_rank {
                        continue;
                    }
                    for _ in 0..multiplicity {
                        reclaimed += u64::from(candidate.accounted_bytes);
                        chosen.push(candidate.clone());
                        if reclaimed >= needed {
                            return chosen;
                        }
                    }
                }
            }
            chosen
        });

        let reclaimed: u64 = evictions
            .iter()
            .map(|candidate| u64::from(candidate.accounted_bytes))
            .sum();
        if reclaimed < needed {
            self.record_drop(&event, DropReason::IncomingLowerPriority);
            return EnqueueOutcome::Dropped;
        }

        self.stream.wtx(|tx| {
            for candidate in &evictions {
                tx.remove(&Mutation::Queue(candidate.clone()));
                tx.insert(&Mutation::Drop(DropFact {
                    priority_rank: candidate.priority_rank,
                    category_rank: candidate.category_rank,
                    reason_rank: DropReason::EvictedForHigherPriority as u8,
                }));
            }
            tx.insert(&Mutation::Queue(event));
        });
        EnqueueOutcome::Retained
    }

    fn record_drop(&mut self, event: &Event, reason: DropReason) {
        self.stream.wtx(|tx| {
            tx.insert(&Mutation::Drop(DropFact {
                priority_rank: event.priority_rank,
                category_rank: event.category_rank,
                reason_rank: reason as u8,
            }))
        });
    }

    fn ordered_events(&self, limit: usize) -> Vec<Event> {
        self.stream.rtx(|(events, _, _, _)| {
            let mut selected = Vec::with_capacity(limit);
            for (event, multiplicity) in events.iter() {
                for _ in 0..multiplicity {
                    selected.push(event.clone());
                    if selected.len() == limit {
                        return selected;
                    }
                }
            }
            selected
        })
    }

    fn prepare_upload(&mut self, limit: usize, attempt: u64) -> Option<UploadIntent> {
        if let Some(intent) = self.snapshot().intent {
            return Some(intent);
        }
        let events = self.ordered_events(limit);
        if events.is_empty() {
            return None;
        }
        let intent = UploadIntent { attempt, events };
        self.stream
            .wtx(|tx| tx.insert(&Mutation::Upload(intent.clone())));
        Some(intent)
    }

    fn acknowledge_upload(&mut self, intent: &UploadIntent) {
        self.stream.wtx(|tx| {
            for event in &intent.events {
                tx.remove(&Mutation::Queue(event.clone()));
            }
            tx.remove(&Mutation::Upload(intent.clone()));
        });
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct BaselineBucket {
    debug: u64,
    operational: u64,
    critical: u64,
    security: u64,
    hardware_failure: u64,
}

impl BaselineBucket {
    fn add(&mut self, event: &Event) {
        match event.priority() {
            Priority::Debug => self.debug += 1,
            Priority::Operational => self.operational += 1,
            Priority::Critical => self.critical += 1,
        }
        match event.category() {
            Category::Security => self.security += 1,
            Category::HardwareFailure => self.hardware_failure += 1,
            _ => {}
        }
    }

    fn merge(&mut self, other: Self) {
        self.debug += other.debug;
        self.operational += other.operational;
        self.critical += other.critical;
        self.security += other.security;
        self.hardware_failure += other.hardware_failure;
    }

    fn total(self) -> u64 {
        self.debug + self.operational + self.critical
    }
}

#[derive(Debug, Default)]
struct FileSummary {
    bytes: u64,
    events: BaselineBucket,
}

#[derive(Debug)]
struct BaselineResult {
    generated: BaselineBucket,
    retained: BaselineBucket,
    dropped_oldest: BaselineBucket,
    retained_bytes: u64,
    files: usize,
    possible_duplicates_after_mid_file_disconnect: u64,
}

fn simulate_baseline(count: u64) -> BaselineResult {
    let mut files = VecDeque::new();
    let mut current = FileSummary::default();
    let mut generated = BaselineBucket::default();
    let mut dropped_oldest = BaselineBucket::default();
    let mut retained_bytes = 0_u64;

    for sequence in 0..count {
        let event = synthetic_event(sequence);
        let line_bytes = u64::from(event.accounted_bytes) + 32;
        if current.bytes > 0 && current.bytes + line_bytes > BASELINE_FILE_SIZE {
            retained_bytes += current.bytes;
            files.push_back(current);
            current = FileSummary::default();
        }
        current.bytes += line_bytes;
        current.events.add(&event);
        generated.add(&event);

        while retained_bytes + current.bytes > BASELINE_CAPACITY {
            let Some(deleted) = files.pop_front() else {
                break;
            };
            retained_bytes -= deleted.bytes;
            dropped_oldest.merge(deleted.events);
        }
    }

    if current.bytes > 0 {
        retained_bytes += current.bytes;
        files.push_back(current);
    }
    while retained_bytes > BASELINE_CAPACITY {
        let deleted = files
            .pop_front()
            .expect("baseline queue unexpectedly empty");
        retained_bytes -= deleted.bytes;
        dropped_oldest.merge(deleted.events);
    }

    let mut retained = BaselineBucket::default();
    for file in &files {
        retained.merge(file.events);
    }
    let possible_duplicates_after_mid_file_disconnect =
        files.front().map_or(0, |file| file.events.total());

    BaselineResult {
        generated,
        retained,
        dropped_oldest,
        retained_bytes,
        files: files.len(),
        possible_duplicates_after_mid_file_disconnect,
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn synthetic_event(sequence: u64) -> Event {
    let percentile = sequence % 100;
    let (priority, category) = if percentile < 3 {
        let category = if sequence.is_multiple_of(2) {
            Category::Security
        } else {
            Category::HardwareFailure
        };
        (Priority::Critical, category)
    } else if percentile < 15 {
        (Priority::Operational, Category::Operations)
    } else {
        (Priority::Debug, Category::Diagnostics)
    };
    let payload_len = if sequence.is_multiple_of(10_000) {
        8 * 1024
    } else {
        100 + usize::try_from(splitmix64(sequence) % 301).expect("payload size")
    };
    let prefix = format!("{{\"event_id\":{sequence},\"data\":\"");
    let suffix = "\"}";
    let filler_len = payload_len.saturating_sub(prefix.len() + suffix.len());
    let mut payload = Vec::with_capacity(prefix.len() + filler_len + suffix.len());
    payload.extend_from_slice(prefix.as_bytes());
    payload.extend(std::iter::repeat_n(b'x', filler_len));
    payload.extend_from_slice(suffix.as_bytes());

    Event {
        priority_rank: priority as u8,
        id_be: sequence.to_be_bytes(),
        gateway_id: 7_u64.to_be_bytes(),
        timestamp_ms_be: (1_700_000_000_000_u64 + sequence).to_be_bytes(),
        category_rank: category as u8,
        accounted_bytes: u32::try_from(payload.len() + 64).expect("event too large"),
        payload,
    }
}

fn append_collector(path: &Path, events: &[Event]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    for event in events {
        writeln!(file, "{}", event.id())?;
    }
    file.sync_all()
}

#[derive(Debug, Default)]
struct CollectorSummary {
    deliveries: u64,
    unique: u64,
    duplicates: u64,
}

fn collector_summary(path: &Path) -> std::io::Result<CollectorSummary> {
    let file = File::open(path)?;
    let mut seen = HashSet::new();
    let mut deliveries = 0_u64;
    let mut duplicates = 0_u64;
    for line in BufReader::new(file).lines() {
        let id: u64 = line?.parse().expect("collector id is numeric");
        deliveries += 1;
        if !seen.insert(id) {
            duplicates += 1;
        }
    }
    Ok(CollectorSummary {
        deliveries,
        unique: u64::try_from(seen.len()).expect("collector length"),
        duplicates,
    })
}

#[derive(Debug, Default)]
struct DirectoryUsage {
    apparent_bytes: u64,
    allocated_bytes: u64,
}

fn directory_usage(path: &Path) -> std::io::Result<DirectoryUsage> {
    fn visit(path: &Path, usage: &mut DirectoryUsage) -> std::io::Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                visit(&entry.path(), usage)?;
            } else {
                usage.apparent_bytes += metadata.len();
                usage.allocated_bytes += metadata.blocks() * 512;
            }
        }
        Ok(())
    }

    let mut usage = DirectoryUsage::default();
    visit(path, &mut usage)?;
    Ok(usage)
}

fn unique_demo_root() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis();
    env::temp_dir().join(format!("edge-spool-pressure-{}-{now}", std::process::id()))
}

fn print_baseline(result: &BaselineResult) {
    println!(
        "baseline: generated={} retained={} bytes={} files={} dropped_oldest={}",
        result.generated.total(),
        result.retained.total(),
        result.retained_bytes,
        result.files,
        result.dropped_oldest.total()
    );
    println!(
        "baseline: dropped critical={} security={} hardware_failure={}",
        result.dropped_oldest.critical,
        result.dropped_oldest.security,
        result.dropped_oldest.hardware_failure
    );
    println!(
        "baseline: modeled whole-file retry exposes up to {} retained events to duplication",
        result.possible_duplicates_after_mid_file_disconnect
    );
}

fn run_demo() -> Result<(), Box<dyn std::error::Error>> {
    let root = unique_demo_root();
    fs::create_dir_all(&root)?;
    println!("demo_root={}", root.display());

    let baseline_started = Instant::now();
    let baseline = simulate_baseline(MILLION);
    print_baseline(&baseline);
    println!(
        "baseline_model_elapsed_ms={}",
        baseline_started.elapsed().as_millis()
    );
    assert_eq!(baseline.generated.total(), MILLION);
    assert!(baseline.dropped_oldest.critical > 0);
    assert!(baseline.retained_bytes <= BASELINE_CAPACITY);

    let candidate_path = root.join("candidate");
    let logical_quota = 4 * MIB;
    let mut spool = Spool::open(&candidate_path, logical_quota);
    let ingest_started = Instant::now();
    for sequence in 0..20_000 {
        spool.enqueue(synthetic_event(sequence));
    }
    spool.checkpoint();
    spool.verify_consistency();
    let candidate = spool.snapshot();
    let candidate_usage = directory_usage(&candidate_path)?;
    println!(
        "candidate_representative: generated=20000 retained={} logical_bytes={} allocated_bytes={} apparent_bytes={} elapsed_ms={}",
        candidate.retained_count,
        candidate.logical_bytes,
        candidate_usage.allocated_bytes,
        candidate_usage.apparent_bytes,
        ingest_started.elapsed().as_millis()
    );
    println!(
        "candidate_representative: retained_by_priority={:?}",
        candidate.retained_by_priority
    );
    println!("candidate_representative: drops={:?}", candidate.drops);
    assert!(candidate.logical_bytes <= logical_quota);
    assert_eq!(
        candidate.retained_by_priority.get("critical").copied(),
        Some(600)
    );

    let crash_write_path = root.join("crash-write");
    let mut crash_spool = Spool::open(&crash_write_path, MIB);
    crash_spool.enqueue(synthetic_event(9_000_000));
    crash_spool.checkpoint();
    drop(crash_spool);
    let crash_status = Command::new(env::current_exe()?)
        .arg("child-crash-write")
        .arg(&crash_write_path)
        .status()?;
    assert_eq!(crash_status.code(), Some(72));
    let recovered_write_spool = Spool::open(&crash_write_path, MIB);
    let recovered_after_write_crash = recovered_write_spool.snapshot();
    let write_ids: Vec<_> = recovered_write_spool
        .ordered_events(10)
        .iter()
        .map(Event::id)
        .collect();
    println!(
        "write_crash_recovery: child_exit={:?} retained={} ids_match={}",
        crash_status.code(),
        recovered_after_write_crash.retained_count,
        write_ids == [9_000_000]
    );
    assert_eq!(recovered_after_write_crash.retained_count, 1);
    assert_eq!(write_ids, [9_000_000]);

    let upload_path = root.join("upload-crash");
    let collector_path = root.join("collector.log");
    let mut upload_spool = Spool::open(&upload_path, 8 * MIB);
    let mut expected_upload_ids = Vec::new();
    for sequence in 0..120 {
        let event = synthetic_event(10_000_000 + sequence);
        expected_upload_ids.push(event.id());
        upload_spool.enqueue(event);
    }
    upload_spool.checkpoint();
    drop(upload_spool);
    let upload_crash_status = Command::new(env::current_exe()?)
        .arg("child-crash-upload")
        .arg(&upload_path)
        .arg(&collector_path)
        .status()?;
    assert_eq!(upload_crash_status.code(), Some(73));

    let recovery_started = Instant::now();
    let mut recovered_spool = Spool::open(&upload_path, 8 * MIB);
    let recovered = recovered_spool.snapshot();
    recovered_spool.verify_consistency();
    let mut recovered_upload_ids: Vec<_> = recovered_spool
        .ordered_events(120)
        .iter()
        .map(Event::id)
        .collect();
    expected_upload_ids.sort_unstable();
    recovered_upload_ids.sort_unstable();
    let recovery_elapsed = recovery_started.elapsed();
    let intent = recovered.intent.expect("durable upload intent");
    println!(
        "upload_crash_recovery: retained={} ids_match={} possible_duplicates={} recovery_ms={}",
        recovered.retained_count,
        recovered_upload_ids == expected_upload_ids,
        intent.events.len(),
        recovery_elapsed.as_millis()
    );
    assert_eq!(recovered.retained_count, 120);
    assert_eq!(recovered_upload_ids, expected_upload_ids);
    assert_eq!(intent.events.len(), 25);
    assert!(recovery_elapsed.as_secs_f64() < 2.0);

    append_collector(&collector_path, &intent.events)?;
    recovered_spool.acknowledge_upload(&intent);
    recovered_spool.checkpoint();
    recovered_spool.verify_consistency();
    let collector = collector_summary(&collector_path)?;
    let after_retry = recovered_spool.snapshot();
    println!(
        "upload_retry: deliveries={} unique={} actual_duplicates={} retained_after_ack={}",
        collector.deliveries, collector.unique, collector.duplicates, after_retry.retained_count
    );
    assert_eq!(collector.deliveries, 32);
    assert_eq!(collector.unique, 25);
    assert_eq!(collector.duplicates, 7);
    assert_eq!(after_retry.retained_count, 95);

    let quota_path = root.join("quota-probe");
    let probe = run_quota_probe(&quota_path)?;
    println!(
        "quota_probe: logical_quota={} logical_bytes={} allocated_bytes={} apparent_bytes={} strict_quota_satisfied={}",
        probe.logical_quota,
        probe.logical_bytes,
        probe.allocated_bytes,
        probe.apparent_bytes,
        probe.allocated_bytes <= probe.logical_quota
    );
    assert!(probe.logical_bytes <= probe.logical_quota);
    assert!(
        probe.allocated_bytes > probe.logical_quota,
        "probe did not reproduce physical quota gap"
    );

    println!("decision=NO_FIT_FOR_STRICT_256_MIB_BOUND");
    println!(
        "reason=Fold's public interface has no documented hard allocated-byte quota guarantee"
    );
    Ok(())
}

#[derive(Debug)]
struct QuotaProbe {
    logical_quota: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    apparent_bytes: u64,
}

fn run_quota_probe(path: &Path) -> Result<QuotaProbe, Box<dyn std::error::Error>> {
    let logical_quota = MIB;
    let mut spool = Spool::open(path, logical_quota);

    for sequence in 0..180 {
        let mut event = synthetic_event(20_000_000 + sequence);
        event.priority_rank = Priority::Debug as u8;
        event.category_rank = Category::Diagnostics as u8;
        event.payload.resize(8 * 1024, b'd');
        event.accounted_bytes = u32::try_from(event.payload.len() + 64)?;
        spool.enqueue(event);
    }
    for sequence in 0..180 {
        let mut event = synthetic_event(30_000_000 + sequence);
        event.priority_rank = Priority::Critical as u8;
        event.category_rank = Category::Security as u8;
        event.payload.resize(8 * 1024, b'c');
        event.accounted_bytes = u32::try_from(event.payload.len() + 64)?;
        spool.enqueue(event);
    }
    spool.checkpoint();
    spool.verify_consistency();
    let logical_bytes = spool.snapshot().logical_bytes;
    drop(spool);
    let usage = directory_usage(path)?;
    Ok(QuotaProbe {
        logical_quota,
        logical_bytes,
        allocated_bytes: usage.allocated_bytes,
        apparent_bytes: usage.apparent_bytes,
    })
}

fn child_crash_write(path: &Path) -> ! {
    let mut spool = Spool::open(path, MIB);
    spool.stream.wtx(|tx| {
        for sequence in 0..100 {
            tx.insert(&Mutation::Queue(synthetic_event(40_000_000 + sequence)));
            if sequence == 49 {
                std::process::exit(72);
            }
        }
    });
    std::process::exit(74);
}

fn child_crash_upload(path: &Path, collector_path: &Path) -> ! {
    let mut spool = Spool::open(path, 8 * MIB);
    let intent = spool
        .prepare_upload(25, 1)
        .expect("events available for upload");
    spool.checkpoint();
    append_collector(collector_path, &intent.events[..7]).expect("collector append");
    std::process::exit(73);
}

fn usage() {
    eprintln!("usage: edge-spool-pressure [demo|baseline|quota-probe <new-dir>]");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str).unwrap_or("demo") {
        "demo" => run_demo(),
        "baseline" => {
            let started = Instant::now();
            let result = simulate_baseline(MILLION);
            print_baseline(&result);
            println!(
                "baseline_model_elapsed_ms={}",
                started.elapsed().as_millis()
            );
            Ok(())
        }
        "quota-probe" => match args.get(2) {
            Some(path) => run_quota_probe(Path::new(path)).map(|probe| {
                println!(
                    "logical_quota={} logical_bytes={} allocated_bytes={} apparent_bytes={} strict_quota_satisfied={}",
                    probe.logical_quota,
                    probe.logical_bytes,
                    probe.allocated_bytes,
                    probe.apparent_bytes,
                    probe.allocated_bytes <= probe.logical_quota
                );
            }),
            None => {
                usage();
                return ExitCode::from(2);
            }
        },
        "child-crash-write" => match args.get(2) {
            Some(path) => child_crash_write(Path::new(path)),
            None => {
                usage();
                return ExitCode::from(2);
            }
        },
        "child-crash-upload" => match (args.get(2), args.get(3)) {
            (Some(path), Some(collector)) => {
                child_crash_upload(Path::new(path), Path::new(collector))
            }
            _ => {
                usage();
                return ExitCode::from(2);
            }
        },
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_path(name: &str) -> PathBuf {
        unique_demo_root().join(name)
    }

    #[test]
    fn million_event_baseline_is_deterministic_and_loses_critical_events() {
        let first = simulate_baseline(MILLION);
        let second = simulate_baseline(MILLION);
        assert_eq!(first.generated.total(), MILLION);
        assert_eq!(first.retained.total(), second.retained.total());
        assert_eq!(
            first.dropped_oldest.critical,
            second.dropped_oldest.critical
        );
        assert!(first.retained_bytes <= BASELINE_CAPACITY);
        assert!(first.dropped_oldest.critical > 0);
        assert!(first.dropped_oldest.security > 0);
        assert!(first.dropped_oldest.hardware_failure > 0);
    }

    #[test]
    fn bag_order_prioritizes_critical_then_operational_then_debug() {
        let path = test_path("ordering");
        let mut spool = Spool::open(&path, MIB);
        spool.enqueue(synthetic_event(99));
        spool.enqueue(synthetic_event(4));
        spool.enqueue(synthetic_event(0));
        let priorities: Vec<_> = spool
            .ordered_events(3)
            .into_iter()
            .map(|event| event.priority())
            .collect();
        assert_eq!(
            priorities,
            vec![Priority::Critical, Priority::Operational, Priority::Debug]
        );
    }

    #[test]
    fn critical_event_evicts_debug_and_drop_is_accounted() {
        let path = test_path("eviction");
        let mut debug = synthetic_event(99);
        debug.accounted_bytes = 600;
        let mut critical = synthetic_event(0);
        critical.accounted_bytes = 600;
        let mut spool = Spool::open(&path, 600);
        assert_eq!(spool.enqueue(debug), EnqueueOutcome::Retained);
        assert_eq!(spool.enqueue(critical), EnqueueOutcome::Retained);
        let snapshot = spool.snapshot();
        assert_eq!(snapshot.retained_count, 1);
        assert_eq!(
            snapshot.retained_by_priority.get("critical").copied(),
            Some(1)
        );
        assert_eq!(
            snapshot
                .drops
                .get("debug/diagnostics/evicted_for_higher_priority")
                .copied(),
            Some(1)
        );
    }

    #[test]
    fn panic_rolls_back_all_fold_views() {
        let path = test_path("panic");
        let mut spool = Spool::open(&path, MIB);
        spool.enqueue(synthetic_event(1));
        let before = spool.snapshot();
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spool.stream.wtx(|tx| {
                tx.insert(&Mutation::Queue(synthetic_event(2)));
                tx.insert(&Mutation::Drop(DropFact {
                    priority_rank: Priority::Debug as u8,
                    category_rank: Category::Diagnostics as u8,
                    reason_rank: DropReason::IncomingLowerPriority as u8,
                }));
                panic!("injected");
            });
        }));
        assert!(panic_result.is_err());
        let after = spool.snapshot();
        assert_eq!(after.retained_count, before.retained_count);
        assert_eq!(after.logical_bytes, before.logical_bytes);
        assert_eq!(after.drops, before.drops);
    }

    #[test]
    fn upload_intent_and_retained_events_survive_reopen() {
        let path = test_path("reopen");
        {
            let mut spool = Spool::open(&path, MIB);
            for sequence in 0..20 {
                spool.enqueue(synthetic_event(sequence));
            }
            let intent = spool.prepare_upload(7, 44).expect("upload intent");
            assert_eq!(intent.events.len(), 7);
            spool.checkpoint();
        }
        let reopened = Spool::open(&path, MIB);
        let snapshot = reopened.snapshot();
        assert_eq!(snapshot.retained_count, 20);
        let mut ids: Vec<_> = reopened.ordered_events(20).iter().map(Event::id).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..20).collect::<Vec<_>>());
        let intent = snapshot.intent.expect("intent recovered");
        assert_eq!(intent.attempt, 44);
        assert_eq!(intent.events.len(), 7);
    }

    #[test]
    fn generator_mix_is_exact_for_one_million() {
        let result = simulate_baseline(MILLION);
        assert_eq!(result.generated.critical, 30_000);
        assert_eq!(result.generated.operational, 120_000);
        assert_eq!(result.generated.debug, 850_000);
    }

    #[test]
    fn payload_is_valid_json_shape_and_in_range() {
        for sequence in [0, 1, 9_999, 10_000, 88_888] {
            let event = synthetic_event(sequence);
            assert!(event.payload.starts_with(b"{\"event_id\":"));
            assert!(event.payload.ends_with(b"\"}"));
            assert!((100..=8 * 1024).contains(&event.payload.len()));
        }
    }

    #[test]
    fn collector_reports_actual_duplicates() {
        let path = test_path("collector.log");
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        let events: Vec<_> = (0..5).map(synthetic_event).collect();
        append_collector(&path, &events[..3]).expect("first request");
        append_collector(&path, &events).expect("retry");
        let summary = collector_summary(&path).expect("collector summary");
        assert_eq!(summary.deliveries, 8);
        assert_eq!(summary.unique, 5);
        assert_eq!(summary.duplicates, 3);
    }

    #[test]
    fn logical_quota_probe_exposes_physical_gap() {
        let path = test_path("quota");
        let probe = run_quota_probe(&path).expect("quota probe");
        assert!(probe.logical_bytes <= probe.logical_quota);
        assert!(probe.allocated_bytes > probe.logical_quota);
    }

    #[test]
    fn drop_key_labels_round_trip() {
        let mut expected = HashMap::new();
        expected.insert(Priority::Critical as u8, "critical");
        expected.insert(Priority::Operational as u8, "operational");
        expected.insert(Priority::Debug as u8, "debug");
        for (rank, label) in expected {
            assert_eq!(Priority::from_rank(rank).name(), label);
        }
    }
}
