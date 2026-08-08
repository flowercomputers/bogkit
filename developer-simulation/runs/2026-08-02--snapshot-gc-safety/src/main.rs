use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

type AnyResult<T> = Result<T, Box<dyn Error>>;
type Hash = [u8; 32];

const CHUNK_HASHES: usize = 65_536;
const CRASH_EXIT: i32 = 86;

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> AnyResult<()> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    match command.as_str() {
        "fixture" => {
            let root = required_path(&mut args, "fixture root")?;
            let config = FixtureConfig::parse(args)?;
            fixture(&root, &config)
        }
        "plan" => {
            let root = required_path(&mut args, "repository root")?;
            let plan = required_arg(&mut args, "plan name")?;
            no_extra(args)?;
            create_plan(&root, &plan)
        }
        "apply" => {
            let root = required_path(&mut args, "repository root")?;
            let plan = required_arg(&mut args, "plan name")?;
            no_extra(args)?;
            apply(&root, &plan)
        }
        "resume" => {
            let root = required_path(&mut args, "repository root")?;
            let plan = required_arg(&mut args, "plan name")?;
            no_extra(args)?;
            resume(&root, &plan)
        }
        "verify-plan" => {
            let root = required_path(&mut args, "repository root")?;
            let plan = required_arg(&mut args, "plan name")?;
            no_extra(args)?;
            verify_plan(&root, &plan)
        }
        "publish" => {
            let root = required_path(&mut args, "repository root")?;
            let manifest = required_arg(&mut args, "manifest file name")?;
            let hashes: Vec<String> = args.collect();
            publish(&root, &manifest, &hashes)
        }
        "status" => {
            let root = required_path(&mut args, "repository root")?;
            let plan = required_arg(&mut args, "plan name")?;
            no_extra(args)?;
            status(&root, &plan)
        }
        _ => Err(usage().into()),
    }
}

fn usage() -> String {
    "usage: snapshot-gc-safety <fixture|plan|apply|resume|verify-plan|publish|status> ..."
        .to_owned()
}

fn required_arg(args: &mut impl Iterator<Item = String>, label: &str) -> AnyResult<String> {
    args.next().ok_or_else(|| format!("missing {label}").into())
}

fn required_path(args: &mut impl Iterator<Item = String>, label: &str) -> AnyResult<PathBuf> {
    Ok(PathBuf::from(required_arg(args, label)?))
}

fn no_extra(mut args: impl Iterator<Item = String>) -> AnyResult<()> {
    if let Some(extra) = args.next() {
        Err(format!("unexpected argument: {extra}").into())
    } else {
        Ok(())
    }
}

#[derive(Debug)]
struct FixtureConfig {
    manifests: usize,
    references: usize,
    unique: usize,
    inventory: usize,
    missing: usize,
}

impl Default for FixtureConfig {
    fn default() -> Self {
        Self {
            manifests: 10_000,
            references: 1_000_000,
            unique: 250_000,
            inventory: 300_000,
            missing: 1_000,
        }
    }
}

impl FixtureConfig {
    fn parse(mut args: impl Iterator<Item = String>) -> AnyResult<Self> {
        let mut config = Self::default();
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?
                .parse::<usize>()?;
            match flag.as_str() {
                "--manifests" => config.manifests = value,
                "--references" => config.references = value,
                "--unique" => config.unique = value,
                "--inventory" => config.inventory = value,
                "--missing" => config.missing = value,
                _ => return Err(format!("unknown fixture flag: {flag}").into()),
            }
        }
        if config.manifests == 0
            || config.unique == 0
            || config.references < config.unique
            || config.inventory < config.unique - config.missing
            || config.missing > config.unique
        {
            return Err("invalid fixture dimensions".into());
        }
        Ok(config)
    }
}

fn fixture(root: &Path, config: &FixtureConfig) -> AnyResult<()> {
    if root.exists() && fs::read_dir(root)?.next().is_some() {
        return Err(format!("fixture root must be absent or empty: {}", root.display()).into());
    }
    let manifests = root.join("manifests");
    let blobs = root.join("blobs");
    fs::create_dir_all(&manifests)?;
    fs::create_dir_all(&blobs)?;

    let per_manifest = config.references.div_ceil(config.manifests);
    let mut record = 0_usize;
    for manifest_number in 0..config.manifests {
        let path = manifests.join(format!("snapshot-{manifest_number:08}.jsonl"));
        let mut output = BufWriter::new(File::create(path)?);
        for _ in 0..per_manifest {
            if record == config.references {
                break;
            }
            let object = if record < config.unique {
                record
            } else {
                record % config.unique
            };
            writeln!(output, "{{\"hash\":\"{}\"}}", hash_number(object))?;
            record += 1;
        }
    }

    let referenced_present = config.unique - config.missing;
    for object in 0..referenced_present {
        File::create(blobs.join(hash_number(object)))?;
    }
    let unreachable = config.inventory - referenced_present;
    for offset in 0..unreachable {
        File::create(blobs.join(hash_number(config.unique + offset)))?;
    }
    sync_directory(&manifests)?;
    sync_directory(&blobs)?;
    println!(
        "fixture: {} manifests, {} references, {} unique, {} inventory, {} missing, {} unreachable",
        config.manifests,
        config.references,
        config.unique,
        config.inventory,
        config.missing,
        unreachable
    );
    Ok(())
}

fn hash_number(value: usize) -> String {
    format!("{value:064x}")
}

fn state_root(root: &Path) -> PathBuf {
    root.join(".snapshot-gc")
}

fn plan_dir(root: &Path, plan: &str) -> AnyResult<PathBuf> {
    validate_name(plan, "plan")?;
    Ok(state_root(root).join("plans").join(plan))
}

fn validate_name(name: &str, kind: &str) -> AnyResult<()> {
    if name.is_empty()
        || name.len() > 100
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || name == "."
        || name == ".."
    {
        return Err(format!("invalid {kind} name: {name}").into());
    }
    Ok(())
}

fn create_plan(root: &Path, plan: &str) -> AnyResult<()> {
    let started = Instant::now();
    validate_repository(root)?;
    let final_dir = plan_dir(root, plan)?;
    if final_dir.join("meta.json").is_file() {
        println!("plan {plan} already exists; unchanged");
        return Ok(());
    }
    let plans = final_dir
        .parent()
        .ok_or("plan path has no parent")?
        .to_path_buf();
    fs::create_dir_all(&plans)?;
    let building = plans.join(format!(".{plan}.building"));
    if building.exists() {
        fs::remove_dir_all(&building)?;
    }
    fs::create_dir(&building)?;

    let result = build_plan(root, plan, &building, started);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&building);
        return Err(error);
    }
    fs::rename(&building, &final_dir)?;
    sync_directory(&plans)?;
    println!("plan {plan} committed at {}", final_dir.display());
    Ok(())
}

fn build_plan(root: &Path, plan: &str, building: &Path, started: Instant) -> AnyResult<()> {
    let manifests = committed_manifests(root)?;
    let mut manifest_set = BufWriter::new(File::create(building.join("manifest-set.txt"))?);
    for manifest in &manifests {
        let metadata = fs::metadata(manifest)?;
        writeln!(
            manifest_set,
            "{}\t{}",
            metadata.len(),
            manifest
                .file_name()
                .ok_or("manifest has no file name")?
                .to_string_lossy()
        )?;
    }
    manifest_set.flush()?;

    let references_path = building.join("references.bin");
    let reference_records = build_references(&manifests, building, &references_path)?;

    if let Ok(value) = env::var("SNAPSHOT_GC_TEST_PAUSE_BEFORE_CANDIDATES_MS") {
        thread::sleep(Duration::from_millis(value.parse()?));
    }

    let inventory_path = building.join("inventory.bin");
    let inventory_count = build_inventory(root, building, &inventory_path)?;
    let candidates_path = building.join("candidates.bin");
    let (unique_references, candidates) =
        sorted_difference(&inventory_path, &references_path, &candidates_path)?;
    fs::remove_file(inventory_path)?;

    let meta = serde_json::json!({
        "version": 1,
        "plan": plan,
        "manifests": manifests.len(),
        "reference_records": reference_records,
        "unique_references": unique_references,
        "inventory": inventory_count,
        "candidates": candidates,
        "elapsed_ms": started.elapsed().as_millis(),
    });
    let mut meta_file = File::create(building.join("meta.json"))?;
    serde_json::to_writer_pretty(&mut meta_file, &meta)?;
    writeln!(meta_file)?;
    meta_file.sync_all()?;
    sync_directory(building)?;
    println!(
        "planned {candidates} candidates from {reference_records} records and {inventory_count} blobs in {:.3}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn validate_repository(root: &Path) -> AnyResult<()> {
    for name in ["manifests", "blobs"] {
        let path = root.join(name);
        if !path.is_dir() {
            return Err(format!("missing directory: {}", path.display()).into());
        }
    }
    Ok(())
}

fn committed_manifests(root: &Path) -> AnyResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root.join("manifests"))? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|value| value == "jsonl")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn build_references(manifests: &[PathBuf], scratch: &Path, output: &Path) -> AnyResult<u64> {
    let chunks = scratch.join("reference-chunks");
    let mut sorter = HashSorter::new(&chunks)?;
    let mut records = 0_u64;
    for manifest in manifests {
        let before = fs::metadata(manifest)?;
        let source = File::open(manifest)?;
        let mut reader = BufReader::new(source);
        let mut line = Vec::new();
        let mut record_number = 0_u64;
        loop {
            line.clear();
            let bytes = reader.read_until(b'\n', &mut line)?;
            if bytes == 0 {
                break;
            }
            record_number += 1;
            if line.last() != Some(&b'\n') {
                return Err(malformed(manifest, record_number, "truncated JSONL record"));
            }
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let value: serde_json::Value = serde_json::from_slice(&line)
                .map_err(|error| malformed(manifest, record_number, &error.to_string()))?;
            let hash = value
                .as_object()
                .and_then(|object| object.get("hash"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| malformed(manifest, record_number, "missing string field `hash`"))?;
            sorter.push(parse_hash(hash).map_err(|error| {
                malformed(manifest, record_number, &format!("invalid hash: {error}"))
            })?)?;
            records += 1;
        }
        let after = fs::metadata(manifest)?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(malformed(
                manifest,
                record_number.max(1),
                "manifest changed while being read",
            ));
        }
    }
    sorter.finish(output)?;
    Ok(records)
}

fn malformed(path: &Path, record: u64, detail: &str) -> Box<dyn Error> {
    format!(
        "malformed committed manifest {} record {record}: {detail}",
        path.display()
    )
    .into()
}

fn build_inventory(root: &Path, scratch: &Path, output: &Path) -> AnyResult<u64> {
    let chunks = scratch.join("inventory-chunks");
    let mut sorter = HashSorter::new(&chunks)?;
    let mut count = 0_u64;
    for entry in fs::read_dir(root.join("blobs"))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Ok(hash) = parse_hash(name) {
            sorter.push(hash)?;
            count += 1;
        }
    }
    sorter.finish(output)?;
    Ok(count)
}

struct HashSorter {
    directory: PathBuf,
    buffer: Vec<Hash>,
    chunks: Vec<PathBuf>,
}

impl HashSorter {
    fn new(directory: &Path) -> AnyResult<Self> {
        if directory.exists() {
            fs::remove_dir_all(directory)?;
        }
        fs::create_dir(directory)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            buffer: Vec::with_capacity(CHUNK_HASHES),
            chunks: Vec::new(),
        })
    }

    fn push(&mut self, hash: Hash) -> AnyResult<()> {
        self.buffer.push(hash);
        if self.buffer.len() == CHUNK_HASHES {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> AnyResult<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer.sort_unstable();
        self.buffer.dedup();
        let path = self
            .directory
            .join(format!("chunk-{:08}.bin", self.chunks.len()));
        let mut output = BufWriter::new(File::create(&path)?);
        for hash in &self.buffer {
            output.write_all(hash)?;
        }
        output.flush()?;
        self.chunks.push(path);
        self.buffer.clear();
        Ok(())
    }

    fn finish(mut self, output: &Path) -> AnyResult<u64> {
        self.flush()?;
        let mut readers: Vec<BufReader<File>> = self
            .chunks
            .iter()
            .map(File::open)
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .map(BufReader::new)
            .collect();
        let mut heap = BinaryHeap::new();
        for (index, reader) in readers.iter_mut().enumerate() {
            if let Some(hash) = read_hash(reader)? {
                heap.push(Reverse((hash, index)));
            }
        }
        let mut destination = BufWriter::new(File::create(output)?);
        let mut last = None;
        let mut count = 0_u64;
        while let Some(Reverse((hash, index))) = heap.pop() {
            if last != Some(hash) {
                destination.write_all(&hash)?;
                last = Some(hash);
                count += 1;
            }
            if let Some(next) = read_hash(&mut readers[index])? {
                heap.push(Reverse((next, index)));
            }
        }
        destination.flush()?;
        destination.get_ref().sync_all()?;
        drop(destination);
        drop(readers);
        fs::remove_dir_all(&self.directory)?;
        Ok(count)
    }
}

fn read_hash(reader: &mut impl Read) -> io::Result<Option<Hash>> {
    let mut hash = [0_u8; 32];
    let mut read = 0;
    while read < hash.len() {
        let bytes = reader.read(&mut hash[read..])?;
        if bytes == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "partial binary hash",
            ));
        }
        read += bytes;
    }
    Ok(Some(hash))
}

fn sorted_difference(inventory: &Path, references: &Path, output: &Path) -> AnyResult<(u64, u64)> {
    let mut inventory = BufReader::new(File::open(inventory)?);
    let mut references = BufReader::new(File::open(references)?);
    let mut output = BufWriter::new(File::create(output)?);
    let mut reference = read_hash(&mut references)?;
    let mut unique_references = u64::from(reference.is_some());
    let mut candidates = 0_u64;
    while let Some(hash) = read_hash(&mut inventory)? {
        while reference.is_some_and(|value| value < hash) {
            reference = read_hash(&mut references)?;
            unique_references += u64::from(reference.is_some());
        }
        if reference != Some(hash) {
            output.write_all(&hash)?;
            candidates += 1;
        }
    }
    while read_hash(&mut references)?.is_some() {
        unique_references += 1;
    }
    output.flush()?;
    output.get_ref().sync_all()?;
    Ok((unique_references, candidates))
}

fn parse_hash(value: &str) -> AnyResult<Hash> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected exactly 64 hexadecimal characters".into());
    }
    let mut hash = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        hash[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Ok(hash)
}

fn hex_value(byte: u8) -> AnyResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("non-hexadecimal character".into()),
    }
}

fn display_hash(hash: &Hash) -> String {
    let mut value = String::with_capacity(64);
    for byte in hash {
        write!(value, "{byte:02x}").expect("writing into a String cannot fail");
    }
    value
}

fn lock_publication(root: &Path) -> AnyResult<File> {
    let state = state_root(root);
    fs::create_dir_all(&state)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("publication.lock"))?;
    lock.lock()?;
    Ok(lock)
}

fn apply(root: &Path, plan: &str) -> AnyResult<()> {
    validate_repository(root)?;
    let directory = require_plan(root, plan)?;
    if directory.join("complete").is_file() {
        println!("plan {plan} is already complete; unchanged");
        return Ok(());
    }
    if directory.join("quarantined").is_file() {
        println!("plan {plan} is already quarantined; run resume to finalize");
        return Ok(());
    }
    quarantine_phase(
        root,
        plan,
        &directory,
        &mut CrashInjector::from_environment(),
    )
}

fn resume(root: &Path, plan: &str) -> AnyResult<()> {
    validate_repository(root)?;
    let directory = require_plan(root, plan)?;
    if directory.join("complete").is_file() {
        println!("plan {plan} is already complete; unchanged");
        return Ok(());
    }
    let mut crashes = CrashInjector::from_environment();
    if !directory.join("quarantined").is_file() {
        quarantine_phase(root, plan, &directory, &mut crashes)?;
    }
    finalize_phase(root, plan, &directory, &mut crashes)
}

fn require_plan(root: &Path, plan: &str) -> AnyResult<PathBuf> {
    let directory = plan_dir(root, plan)?;
    if !directory.join("meta.json").is_file() || !directory.join("candidates.bin").is_file() {
        return Err(format!("plan {plan} does not exist or is incomplete").into());
    }
    Ok(directory)
}

fn quarantine_phase(
    root: &Path,
    plan: &str,
    directory: &Path,
    crashes: &mut CrashInjector,
) -> AnyResult<()> {
    let started = Instant::now();
    let _lock = lock_publication(root)?;
    let live = rebuild_live_references(root, directory)?;
    let quarantine = directory.join("quarantine");
    fs::create_dir_all(&quarantine)?;
    let mut candidates = BufReader::new(File::open(directory.join("candidates.bin"))?);
    let mut references = SortedMembership::new(&live)?;
    let mut moved = 0_u64;
    while let Some(hash) = read_hash(&mut candidates)? {
        let source = root.join("blobs").join(display_hash(&hash));
        let target = quarantine.join(display_hash(&hash));
        if references.contains(hash)? {
            if target.is_file() && !source.exists() {
                fs::rename(&target, &source)?;
                crashes.boundary("quarantine-live-restore");
            }
        } else if source.is_file() && !target.exists() {
            fs::rename(&source, &target)?;
            moved += 1;
            crashes.boundary("quarantine-move");
        }
    }
    write_marker(&directory.join("quarantined"), &format!("moved={moved}\n"))?;
    crashes.boundary("quarantine-marker");
    println!(
        "apply {plan}: quarantined {moved} blobs in {:.3}s; run resume to finalize",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn finalize_phase(
    root: &Path,
    plan: &str,
    directory: &Path,
    crashes: &mut CrashInjector,
) -> AnyResult<()> {
    let started = Instant::now();
    let _lock = lock_publication(root)?;
    let live = rebuild_live_references(root, directory)?;
    let quarantine = directory.join("quarantine");
    fs::create_dir_all(&quarantine)?;
    let mut candidates = BufReader::new(File::open(directory.join("candidates.bin"))?);
    let mut references = SortedMembership::new(&live)?;
    let mut removed = 0_u64;
    let mut restored = 0_u64;
    while let Some(hash) = read_hash(&mut candidates)? {
        let source = quarantine.join(display_hash(&hash));
        if !source.is_file() {
            continue;
        }
        let live_blob = root.join("blobs").join(display_hash(&hash));
        if references.contains(hash)? {
            if !live_blob.exists() {
                fs::rename(&source, &live_blob)?;
                restored += 1;
                crashes.boundary("finalize-live-restore");
            }
        } else {
            fs::remove_file(&source)?;
            removed += 1;
            crashes.boundary("finalize-remove");
        }
    }
    write_marker(
        &directory.join("complete"),
        &format!("removed={removed}\nrestored={restored}\n"),
    )?;
    crashes.boundary("complete-marker");
    println!(
        "resume {plan}: removed {removed}, restored {restored} in {:.3}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn rebuild_live_references(root: &Path, directory: &Path) -> AnyResult<PathBuf> {
    let manifests = committed_manifests(root)?;
    let scratch = directory.join("live-build");
    if scratch.exists() {
        fs::remove_dir_all(&scratch)?;
    }
    fs::create_dir(&scratch)?;
    let output = directory.join("live-references.bin");
    let result = build_references(&manifests, &scratch, &output);
    let _ = fs::remove_dir_all(&scratch);
    result?;
    Ok(output)
}

struct SortedMembership {
    reader: BufReader<File>,
    current: Option<Hash>,
}

impl SortedMembership {
    fn new(path: &Path) -> AnyResult<Self> {
        let mut reader = BufReader::new(File::open(path)?);
        let current = read_hash(&mut reader)?;
        Ok(Self { reader, current })
    }

    fn contains(&mut self, wanted: Hash) -> AnyResult<bool> {
        while self.current.is_some_and(|value| value < wanted) {
            self.current = read_hash(&mut self.reader)?;
        }
        Ok(self.current == Some(wanted))
    }
}

struct CrashInjector {
    crash_after: Option<u64>,
    boundaries: u64,
}

impl CrashInjector {
    fn from_environment() -> Self {
        let crash_after = env::var("SNAPSHOT_GC_CRASH_AFTER")
            .ok()
            .and_then(|value| value.parse().ok());
        Self {
            crash_after,
            boundaries: 0,
        }
    }

    fn boundary(&mut self, label: &str) {
        self.boundaries += 1;
        if self.crash_after == Some(self.boundaries) {
            eprintln!(
                "injected crash after boundary {} ({label})",
                self.boundaries
            );
            process::exit(CRASH_EXIT);
        }
    }
}

fn write_marker(path: &Path, contents: &str) -> AnyResult<()> {
    let file_name = path.file_name().ok_or("marker has no file name")?;
    let temporary = path.with_file_name(format!(".{}.tmp", file_name.to_string_lossy()));
    let mut file = File::create(&temporary)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_directory(path.parent().ok_or("marker has no parent")?)?;
    Ok(())
}

fn publish(root: &Path, manifest: &str, values: &[String]) -> AnyResult<()> {
    validate_repository(root)?;
    validate_name(manifest, "manifest")?;
    if Path::new(manifest)
        .extension()
        .is_none_or(|value| value != "jsonl")
    {
        return Err("published manifest name must end in .jsonl".into());
    }
    if values.is_empty() {
        return Err("publish requires at least one hash".into());
    }
    let hashes: Vec<Hash> = values
        .iter()
        .map(|value| parse_hash(value))
        .collect::<AnyResult<Vec<_>>>()?;
    let manifests = root.join("manifests");
    let final_path = manifests.join(manifest);
    let temporary = manifests.join(format!(".{manifest}.{}.tmp", process::id()));
    let mut output = BufWriter::new(File::create(&temporary)?);
    for value in values {
        writeln!(output, "{{\"hash\":\"{}\"}}", value.to_ascii_lowercase())?;
    }
    output.flush()?;
    output.get_ref().sync_all()?;

    let _lock = lock_publication(root)?;
    if final_path.exists() {
        let _ = fs::remove_file(&temporary);
        return Err(format!("manifest already exists: {}", final_path.display()).into());
    }
    for hash in hashes {
        ensure_blob_available(root, &hash)?;
    }
    fs::rename(&temporary, &final_path)?;
    sync_directory(&manifests)?;
    println!("published {}", final_path.display());
    Ok(())
}

fn ensure_blob_available(root: &Path, hash: &Hash) -> AnyResult<()> {
    let name = display_hash(hash);
    let live = root.join("blobs").join(&name);
    if live.is_file() {
        return Ok(());
    }
    let plans = state_root(root).join("plans");
    if plans.is_dir() {
        for entry in fs::read_dir(plans)? {
            let candidate = entry?.path().join("quarantine").join(&name);
            if candidate.is_file() {
                fs::rename(&candidate, &live)?;
                sync_directory(candidate.parent().ok_or("quarantine has no parent")?)?;
                sync_directory(&root.join("blobs"))?;
                return Ok(());
            }
        }
    }
    Err(format!("cannot publish reference to unavailable blob {name}").into())
}

fn verify_plan(root: &Path, plan: &str) -> AnyResult<()> {
    validate_repository(root)?;
    let directory = require_plan(root, plan)?;
    let mut referenced = HashSet::new();
    for manifest in committed_manifests(root)? {
        load_manifest_for_oracle(&manifest, &mut referenced)?;
    }
    let mut planned = HashSet::new();
    let mut reader = BufReader::new(File::open(directory.join("candidates.bin"))?);
    while let Some(hash) = read_hash(&mut reader)? {
        planned.insert(hash);
    }
    let mut eligible = 0_usize;
    for entry in fs::read_dir(root.join("blobs"))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Ok(hash) = parse_hash(&name) else {
            continue;
        };
        if !referenced.contains(&hash) {
            eligible += 1;
            if !planned.remove(&hash) {
                return Err(
                    format!("oracle mismatch: eligible blob {name} is absent from plan").into(),
                );
            }
        } else if planned.contains(&hash) {
            return Err(format!("oracle mismatch: referenced blob {name} is in plan").into());
        }
    }
    if let Some(extra) = planned.iter().next() {
        return Err(format!(
            "oracle mismatch: non-inventory candidate {}",
            display_hash(extra)
        )
        .into());
    }
    println!(
        "oracle match: all {eligible} eligible unreachable blobs selected; zero referenced blobs selected"
    );
    Ok(())
}

fn load_manifest_for_oracle(path: &Path, referenced: &mut HashSet<Hash>) -> AnyResult<()> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    let mut record = 0_u64;
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        record += 1;
        if !line.ends_with('\n') {
            return Err(malformed(path, record, "truncated JSONL record"));
        }
        let value: serde_json::Value = serde_json::from_str(&line)
            .map_err(|error| malformed(path, record, &error.to_string()))?;
        let hash = value
            .get("hash")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| malformed(path, record, "missing string field `hash`"))?;
        referenced.insert(parse_hash(hash)?);
    }
    Ok(())
}

fn status(root: &Path, plan: &str) -> AnyResult<()> {
    let directory = require_plan(root, plan)?;
    let phase = if directory.join("complete").is_file() {
        "complete"
    } else if directory.join("quarantined").is_file() {
        "quarantined"
    } else {
        "planned"
    };
    println!("plan {plan}: {phase}");
    Ok(())
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip_and_case_normalization() {
        let input = "0123456789abcdef0123456789ABCDEF0123456789abcdef0123456789ABCDEF";
        let parsed = parse_hash(input).unwrap();
        assert_eq!(
            display_hash(&parsed),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn rejects_wrong_hash_shapes() {
        assert!(parse_hash("abc").is_err());
        assert!(parse_hash(&"z".repeat(64)).is_err());
    }

    #[test]
    fn validates_names() {
        assert!(validate_name("nightly-2026_08.02", "plan").is_ok());
        assert!(validate_name("../escape", "plan").is_err());
        assert!(validate_name("", "plan").is_err());
    }
}
