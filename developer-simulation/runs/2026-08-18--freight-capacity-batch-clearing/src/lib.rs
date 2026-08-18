use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::Path,
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_LINE_BYTES: usize = 4_096;
const MAX_QUANTITY: u64 = 1_000_000;
const MAX_PRICE_CENTS: u64 = 10_000_000;
const MAX_PROPOSAL_BYTES: u64 = 160 * 1024 * 1024;
const FIXTURE_DATA_SEED: u64 = 0xd37e_5eed_2026_0818;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub order_id: String,
    pub lane_id: String,
    pub service_date: String,
    pub side: Side,
    pub submitted_sequence: u64,
    pub quantity: u64,
    pub limit_price_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MarketKey {
    pub lane_id: String,
    pub service_date: String,
}

impl MarketKey {
    #[must_use]
    pub fn new(lane_id: &str, service_date: &str) -> Self {
        Self {
            lane_id: lane_id.to_owned(),
            service_date: service_date.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    pub lane_id: String,
    pub service_date: String,
    pub fill_index: u64,
    pub buy_id: String,
    pub sell_id: String,
    pub quantity: u64,
    pub price_cents: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub fills: Vec<Fill>,
    pub gross_value_cents: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePoint {
    None,
    AfterValidation,
    AfterOnePercent,
    AfterHalf,
    AfterTempSync,
    AfterRenameBeforeDirectorySync,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunErrorState {
    NotPublished,
    PublishedDurabilityUncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError {
    state: RunErrorState,
    message: String,
}

impl RunError {
    fn not_published(message: impl Into<String>) -> Self {
        Self {
            state: RunErrorState::NotPublished,
            message: message.into(),
        }
    }

    fn durability_uncertain(message: impl Into<String>) -> Self {
        Self {
            state: RunErrorState::PublishedDurabilityUncertain,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn state(&self) -> RunErrorState {
        self.state
    }

    #[must_use]
    pub const fn is_durability_uncertain(&self) -> bool {
        matches!(self.state, RunErrorState::PublishedDurabilityUncertain)
    }

    #[must_use]
    pub fn contains(&self, pattern: &str) -> bool {
        self.message.contains(pattern)
    }
}

impl From<String> for RunError {
    fn from(message: String) -> Self {
        Self::not_published(message)
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match self.state {
            RunErrorState::NotPublished => "NOT_PUBLISHED",
            RunErrorState::PublishedDurabilityUncertain => "PUBLISHED_DURABILITY_UNCERTAIN",
        };
        write!(formatter, "{state}: {}", self.message)
    }
}

impl std::error::Error for RunError {}

#[derive(Debug, Clone)]
pub struct RunPaths {
    pub orders: PathBuf,
    pub manifest: PathBuf,
    pub proposal: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunResult {
    pub order_count: usize,
    pub market_count: usize,
    pub fill_count: usize,
    pub gross_value_cents: u128,
    pub proposal_sha256: String,
    pub proposal_bytes: u64,
    pub peak_resident_memory_bytes: Option<u64>,
}

#[must_use]
pub fn parse_linux_peak_rss_bytes(status: &str) -> Option<u64> {
    let kibibytes = status.lines().find_map(|line| {
        line.strip_prefix("VmHWM:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    kibibytes.checked_mul(1_024)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixtureSpec {
    pub market_count: usize,
    pub buy_count: usize,
    pub sell_count: usize,
    pub seed: u64,
}

#[derive(Debug, Clone)]
pub struct GeneratedFixture {
    pub paths: RunPaths,
    pub manifest: Manifest,
}

/// Writes a deterministic snapshot and its reference manifest.
///
/// # Errors
///
/// Returns an error for invalid counts, arithmetic overflow, I/O failure, or
/// if the generated snapshot does not validate and clear successfully.
pub fn generate_fixture(directory: &Path, spec: FixtureSpec) -> Result<GeneratedFixture, String> {
    if spec.market_count == 0
        || spec.buy_count < spec.market_count
        || spec.sell_count < spec.market_count
    {
        return Err("fixture needs at least one buy and sell per market".to_owned());
    }
    let order_count = spec
        .buy_count
        .checked_add(spec.sell_count)
        .ok_or_else(|| "fixture order count overflow".to_owned())?;
    fs::create_dir_all(directory)
        .map_err(|error| format!("create fixture directory {}: {error}", directory.display()))?;
    let paths = RunPaths {
        orders: directory.join("orders.ndjson"),
        manifest: directory.join("manifest.json"),
        proposal: directory.join("proposal.ndjson"),
    };
    let orders_file = File::create(&paths.orders)
        .map_err(|error| format!("create {}: {error}", paths.orders.display()))?;
    let mut writer = BufWriter::new(orders_file);
    let mut digest = Sha256::new();
    let total = u64::try_from(order_count).map_err(|_| "too many fixture orders")?;
    let mut multiplier = (spec.seed % total).max(1);
    while gcd(multiplier, total) != 1 {
        multiplier = (multiplier + 1) % total;
        if multiplier == 0 {
            multiplier = 1;
        }
    }
    let offset = spec.seed.rotate_left(17) % total;
    for position in 0..total {
        let original = multiplier
            .checked_mul(position)
            .ok_or_else(|| "fixture permutation overflow".to_owned())?
            .wrapping_add(offset)
            % total;
        let buy_count_u64 = u64::try_from(spec.buy_count).map_err(|_| "too many buys")?;
        let (side, side_index) = if original < buy_count_u64 {
            (Side::Buy, original)
        } else {
            (Side::Sell, original - buy_count_u64)
        };
        let market_count_u64 = u64::try_from(spec.market_count).map_err(|_| "too many markets")?;
        let market = side_index % market_count_u64;
        let random = mix64(FIXTURE_DATA_SEED ^ original);
        let order = Order {
            order_id: match side {
                Side::Buy => format!("buy-{side_index:010}"),
                Side::Sell => format!("sell-{side_index:010}"),
            },
            lane_id: format!("lane-{market:05}"),
            service_date: format!("2026-09-{:02}", 1 + market % 28),
            side,
            submitted_sequence: random % 100_000,
            quantity: 1 + random.rotate_left(13) % MAX_QUANTITY,
            limit_price_cents: match side {
                Side::Buy => 5_000_001 + random.rotate_left(29) % 5_000_000,
                Side::Sell => 1 + random.rotate_left(29) % 5_000_000,
            },
        };
        let mut line =
            serde_json::to_vec(&order).map_err(|error| format!("serialize order: {error}"))?;
        line.push(b'\n');
        digest.update(&line);
        writer
            .write_all(&line)
            .map_err(|error| format!("write {}: {error}", paths.orders.display()))?;
    }
    writer
        .flush()
        .map_err(|error| format!("flush {}: {error}", paths.orders.display()))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| format!("sync {}: {error}", paths.orders.display()))?;

    let mut manifest = Manifest {
        order_count,
        buy_count: spec.buy_count,
        sell_count: spec.sell_count,
        market_count: spec.market_count,
        max_fill_count: order_count - spec.market_count,
        expected_gross_value_cents: None,
        orders_sha256: Some(format!("{:x}", digest.finalize())),
    };
    let orders_file =
        File::open(&paths.orders).map_err(|error| format!("open generated orders: {error}"))?;
    let batch = validate_orders(BufReader::new(orders_file), &manifest)?;
    manifest.expected_gross_value_cents = Some(clear_batch(&batch, &manifest)?.gross_value_cents);
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("serialize manifest: {error}"))?;
    manifest_bytes.push(b'\n');
    fs::write(&paths.manifest, manifest_bytes)
        .map_err(|error| format!("write {}: {error}", paths.manifest.display()))?;
    Ok(GeneratedFixture { paths, manifest })
}

const fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

const fn mix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Validates, clears, checks, and atomically publishes one snapshot.
///
/// # Errors
///
/// Returns an error for input, validation, clearing, invariant, publication,
/// or deliberately injected failures. The final path is untouched until the
/// atomic rename.
pub fn run_files(paths: &RunPaths, failure: FailurePoint) -> Result<RunResult, RunError> {
    reject_input_aliases(paths)?;
    let manifest_file = File::open(&paths.manifest)
        .map_err(|error| format!("open manifest {}: {error}", paths.manifest.display()))?;
    let manifest: Manifest = serde_json::from_reader(BufReader::new(manifest_file))
        .map_err(|error| format!("parse manifest {}: {error}", paths.manifest.display()))?;
    let expected_digest = manifest
        .orders_sha256
        .as_deref()
        .ok_or_else(|| "orders_sha256 is required".to_owned())?;
    if expected_digest.len() != 64 || !expected_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("orders_sha256 must be valid 64-hex".to_owned().into());
    }
    if manifest.expected_gross_value_cents.is_none() {
        return Err("expected_gross_value_cents is required".to_owned().into());
    }
    let actual_digest = sha256_file(&paths.orders)?;
    if !actual_digest.eq_ignore_ascii_case(expected_digest) {
        return Err("orders SHA-256 does not match manifest".to_owned().into());
    }
    let orders_file = File::open(&paths.orders)
        .map_err(|error| format!("open orders {}: {error}", paths.orders.display()))?;
    let batch = validate_orders(BufReader::new(orders_file), &manifest)?;
    if failure == FailurePoint::AfterValidation {
        return Err("injected exit after validation".to_owned().into());
    }

    let proposal = clear_batch_with_failure(&batch, &manifest, failure)?;
    let parent = normalized_parent(&paths.proposal)?;
    let directory = File::open(parent)
        .map_err(|error| format!("open proposal directory {}: {error}", parent.display()))?;
    let (temporary_path, temporary_file) = create_sibling_temp(&paths.proposal, parent)?;
    let publication = write_and_publish(
        &temporary_path,
        temporary_file,
        &paths.proposal,
        &directory,
        &proposal,
        failure,
    );
    if publication.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    let (proposal_sha256, proposal_bytes) = publication?;
    Ok(RunResult {
        order_count: batch.order_count(),
        market_count: batch.market_count(),
        fill_count: proposal.fills.len(),
        gross_value_cents: proposal.gross_value_cents,
        proposal_sha256,
        proposal_bytes,
        peak_resident_memory_bytes: linux_peak_rss_bytes(),
    })
}

fn reject_input_aliases(paths: &RunPaths) -> Result<(), String> {
    for input in [&paths.orders, &paths.manifest] {
        let input_resolved = fs::canonicalize(input)
            .map_err(|error| format!("resolve input {}: {error}", input.display()))?;
        if let Ok(proposal_resolved) = fs::canonicalize(&paths.proposal)
            && proposal_resolved == input_resolved
        {
            return Err(format!(
                "proposal path aliases an input: {}",
                input.display()
            ));
        }
        if same_existing_file(&paths.proposal, input)? {
            return Err(format!(
                "proposal path aliases an input: {}",
                input.display()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn same_existing_file(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;

    let left_metadata = match fs::metadata(left) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("inspect proposal identity: {error}")),
    };
    let right_metadata = fs::metadata(right)
        .map_err(|error| format!("inspect input identity {}: {error}", right.display()))?;
    Ok(left_metadata.dev() == right_metadata.dev() && left_metadata.ino() == right_metadata.ino())
}

#[cfg(not(unix))]
fn same_existing_file(left: &Path, right: &Path) -> Result<bool, String> {
    let left_resolved = match fs::canonicalize(left) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("inspect proposal identity: {error}")),
    };
    let right_resolved = fs::canonicalize(right)
        .map_err(|error| format!("inspect input identity {}: {error}", right.display()))?;
    Ok(left_resolved == right_resolved)
}

fn linux_peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    parse_linux_peak_rss_bytes(&status)
}

fn clear_batch_with_failure(
    batch: &ValidatedBatch,
    manifest: &Manifest,
    failure: FailurePoint,
) -> Result<Proposal, String> {
    let one_percent = batch.market_count().div_ceil(100).max(1);
    let half = batch.market_count().div_ceil(2).max(1);
    let mut fills = Vec::new();
    for (index, (key, orders)) in batch.markets.iter().enumerate() {
        fills.extend(clear_market(key, orders)?);
        if fills.len() > manifest.max_fill_count {
            return Err("proposal exceeds maximum fill count".to_owned());
        }
        let completed = index + 1;
        if failure == FailurePoint::AfterOnePercent && completed >= one_percent {
            return Err("injected exit after one percent of markets".to_owned());
        }
        if failure == FailurePoint::AfterHalf && completed >= half {
            return Err("injected exit after half of markets".to_owned());
        }
    }
    let gross_value_cents = fills.iter().try_fold(0_u128, |gross, fill| {
        let fill_value = u128::from(fill.quantity)
            .checked_mul(u128::from(fill.price_cents))
            .ok_or_else(|| "gross value multiplication overflow".to_owned())?;
        gross
            .checked_add(fill_value)
            .ok_or_else(|| "gross value addition overflow".to_owned())
    })?;
    let proposal = Proposal {
        fills,
        gross_value_cents,
    };
    validate_proposal(batch, &proposal, manifest)?;
    Ok(proposal)
}

fn normalized_parent(path: &Path) -> Result<&Path, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "proposal path has no parent".to_owned())?;
    if parent.as_os_str().is_empty() {
        Ok(Path::new("."))
    } else {
        Ok(parent)
    }
}

fn create_sibling_temp(final_path: &Path, parent: &Path) -> Result<(PathBuf, File), String> {
    let name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "proposal filename must be UTF-8".to_owned())?;
    for attempt in 0..100_u32 {
        let path = parent.join(format!(".{name}.tmp-{}-{attempt}", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("create sibling temporary file: {error}")),
        }
    }
    Err("could not reserve sibling temporary filename".to_owned())
}

fn write_and_publish(
    temporary_path: &Path,
    temporary_file: File,
    final_path: &Path,
    directory: &File,
    proposal: &Proposal,
    failure: FailurePoint,
) -> Result<(String, u64), RunError> {
    let mut writer = BufWriter::new(temporary_file);
    let mut digest = Sha256::new();
    let mut bytes_written = 0_u64;
    for fill in &proposal.fills {
        let mut line =
            serde_json::to_vec(fill).map_err(|error| format!("serialize fill: {error}"))?;
        line.push(b'\n');
        let line_len =
            u64::try_from(line.len()).map_err(|_| "proposal line too large".to_owned())?;
        bytes_written = bytes_written
            .checked_add(line_len)
            .ok_or_else(|| "proposal byte count overflow".to_owned())?;
        if bytes_written > MAX_PROPOSAL_BYTES {
            return Err("proposal exceeds 160 MiB".to_owned().into());
        }
        digest.update(&line);
        writer
            .write_all(&line)
            .map_err(|error| format!("write temporary proposal: {error}"))?;
    }
    writer
        .flush()
        .map_err(|error| format!("flush temporary proposal: {error}"))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|error| format!("sync temporary proposal: {error}"))?;
    if failure == FailurePoint::AfterTempSync {
        return Err("injected exit after temporary-file sync".to_owned().into());
    }
    drop(writer);
    fs::rename(temporary_path, final_path)
        .map_err(|error| format!("atomic proposal rename: {error}"))?;
    if failure == FailurePoint::AfterRenameBeforeDirectorySync {
        return Err(RunError::durability_uncertain(
            "proposal published but directory durability is uncertain: injected after rename",
        ));
    }
    directory.sync_all().map_err(|error| {
        RunError::durability_uncertain(format!(
            "proposal published but directory durability is uncertain: {error}"
        ))
    })?;
    Ok((format!("{:x}", digest.finalize()), bytes_written))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub order_count: usize,
    pub buy_count: usize,
    pub sell_count: usize,
    pub market_count: usize,
    pub max_fill_count: usize,
    pub expected_gross_value_cents: Option<u128>,
    pub orders_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatedBatch {
    markets: BTreeMap<MarketKey, Vec<Order>>,
    order_count: usize,
}

impl ValidatedBatch {
    #[must_use]
    pub const fn order_count(&self) -> usize {
        self.order_count
    }

    #[must_use]
    pub fn market_count(&self) -> usize {
        self.markets.len()
    }
}

/// Clears every market in canonical market-key order.
///
/// # Errors
///
/// Returns an error if fill bounds, checked arithmetic, reference-manifest
/// value, or allocation invariants fail.
pub fn clear_batch(batch: &ValidatedBatch, manifest: &Manifest) -> Result<Proposal, String> {
    let mut fills = Vec::new();
    for (key, orders) in &batch.markets {
        fills.extend(clear_market(key, orders)?);
        if fills.len() > manifest.max_fill_count {
            return Err("proposal exceeds maximum fill count".to_owned());
        }
    }
    let gross_value_cents = fills.iter().try_fold(0_u128, |gross, fill| {
        let value = u128::from(fill.quantity)
            .checked_mul(u128::from(fill.price_cents))
            .ok_or_else(|| "gross value multiplication overflow".to_owned())?;
        gross
            .checked_add(value)
            .ok_or_else(|| "gross value addition overflow".to_owned())
    })?;
    let proposal = Proposal {
        fills,
        gross_value_cents,
    };
    validate_proposal(batch, &proposal, manifest)?;
    Ok(proposal)
}

/// Rechecks all fills against the validated input and canonical matcher.
///
/// # Errors
///
/// Returns an error for a bound, identity, market, price, quantity, priority,
/// conservation, ordering, arithmetic, or manifest mismatch.
pub fn validate_proposal(
    batch: &ValidatedBatch,
    proposal: &Proposal,
    manifest: &Manifest,
) -> Result<(), String> {
    if proposal.fills.len() > manifest.max_fill_count {
        return Err("proposal exceeds maximum fill count".to_owned());
    }
    let mut fill_cursor = 0_usize;
    let mut gross = 0_u128;
    for (key, orders) in &batch.markets {
        let expected = clear_market(key, orders)?;
        let fill_end = fill_cursor
            .checked_add(expected.len())
            .ok_or_else(|| "proposal fill cursor overflow".to_owned())?;
        let actual = proposal
            .fills
            .get(fill_cursor..fill_end)
            .ok_or_else(|| "proposal is missing canonical fills".to_owned())?;
        if actual != expected {
            return Err("proposal bypasses priority or is not canonically ordered".to_owned());
        }
        let mut orders_by_id = std::collections::HashMap::new();
        let mut remaining = std::collections::HashMap::new();
        for order in orders {
            orders_by_id.insert(order.order_id.as_str(), order);
            remaining.insert(order.order_id.as_str(), order.quantity);
        }
        for fill in actual {
            if fill.quantity == 0 {
                return Err("fill quantity must be positive".to_owned());
            }
            let buy = orders_by_id
                .get(fill.buy_id.as_str())
                .ok_or_else(|| "fill references unknown buy".to_owned())?;
            let sell = orders_by_id
                .get(fill.sell_id.as_str())
                .ok_or_else(|| "fill references unknown sell".to_owned())?;
            if buy.side != Side::Buy || sell.side != Side::Sell {
                return Err("fill references wrong order sides".to_owned());
            }
            if buy.lane_id != fill.lane_id
                || sell.lane_id != fill.lane_id
                || buy.service_date != fill.service_date
                || sell.service_date != fill.service_date
            {
                return Err("fill crosses market boundaries".to_owned());
            }
            if buy.limit_price_cents < sell.limit_price_cents
                || fill.price_cents != sell.limit_price_cents
            {
                return Err("fill violates crossing or seller-ask price".to_owned());
            }
            let buy_remaining = remaining
                .get_mut(fill.buy_id.as_str())
                .ok_or_else(|| "buy remaining quantity missing".to_owned())?;
            if fill.quantity > *buy_remaining {
                return Err("fill exceeds buy remaining quantity".to_owned());
            }
            *buy_remaining -= fill.quantity;
            let sell_remaining = remaining
                .get_mut(fill.sell_id.as_str())
                .ok_or_else(|| "sell remaining quantity missing".to_owned())?;
            if fill.quantity > *sell_remaining {
                return Err("fill exceeds sell remaining quantity".to_owned());
            }
            *sell_remaining -= fill.quantity;
            gross = gross
                .checked_add(
                    u128::from(fill.quantity)
                        .checked_mul(u128::from(fill.price_cents))
                        .ok_or_else(|| "gross value multiplication overflow".to_owned())?,
                )
                .ok_or_else(|| "gross value addition overflow".to_owned())?;
        }
        fill_cursor = fill_end;
    }
    if fill_cursor != proposal.fills.len() {
        return Err("proposal contains fills after the canonical market sequence".to_owned());
    }
    if gross != proposal.gross_value_cents {
        return Err("proposal gross value does not match fills".to_owned());
    }
    if manifest
        .expected_gross_value_cents
        .is_some_and(|expected_gross| expected_gross != gross)
    {
        return Err("proposal gross value does not match reference manifest".to_owned());
    }
    Ok(())
}

/// Parses and validates the complete immutable order stream.
///
/// # Errors
///
/// Returns an error for malformed input, invalid fields, duplicates,
/// incomplete markets, or inconsistent manifest counts and bounds.
pub fn validate_orders<R: BufRead>(
    mut reader: R,
    manifest: &Manifest,
) -> Result<ValidatedBatch, String> {
    let mut markets: BTreeMap<MarketKey, Vec<Order>> = BTreeMap::new();
    let mut order_count = 0_usize;
    let mut buy_count = 0_usize;
    let mut sell_count = 0_usize;
    let mut order_ids = HashSet::new();
    let mut buffer = Vec::with_capacity(MAX_LINE_BYTES + 1);
    let mut index = 0_usize;
    loop {
        buffer.clear();
        let bytes_read = read_bounded_line(&mut reader, &mut buffer, index + 1)?;
        if bytes_read == 0 {
            break;
        }
        index += 1;
        if buffer.last() != Some(&b'\n') {
            return Err(format!("line {index}: missing final newline"));
        }
        buffer.pop();
        if buffer.len() > MAX_LINE_BYTES {
            return Err(format!("line {index}: line too long"));
        }
        let line = std::str::from_utf8(&buffer)
            .map_err(|error| format!("line {index}: invalid UTF-8: {error}"))?;
        let order: Order =
            serde_json::from_str(line).map_err(|error| format!("line {index}: {error}"))?;
        validate_order(&order).map_err(|error| format!("line {index}: {error}"))?;
        if !order_ids.insert(order.order_id.clone()) {
            return Err(format!("duplicate order_id: {}", order.order_id));
        }
        order_count += 1;
        match order.side {
            Side::Buy => buy_count += 1,
            Side::Sell => sell_count += 1,
        }
        markets
            .entry(MarketKey {
                lane_id: order.lane_id.clone(),
                service_date: order.service_date.clone(),
            })
            .or_default()
            .push(order);
    }
    if order_count != manifest.order_count
        || buy_count != manifest.buy_count
        || sell_count != manifest.sell_count
        || markets.len() != manifest.market_count
    {
        return Err("manifest counts do not match orders".to_owned());
    }
    for orders in markets.values() {
        let has_buy = orders.iter().any(|order| order.side == Side::Buy);
        let has_sell = orders.iter().any(|order| order.side == Side::Sell);
        if !has_buy || !has_sell {
            return Err("every market must contain a buy and a sell".to_owned());
        }
    }
    let derived_fill_cap = order_count
        .checked_sub(markets.len())
        .ok_or_else(|| "invalid market count".to_owned())?;
    if manifest.max_fill_count != derived_fill_cap {
        return Err("manifest maximum fill count is inconsistent".to_owned());
    }
    Ok(ValidatedBatch {
        markets,
        order_count,
    })
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    line_number: usize,
) -> Result<usize, String> {
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("line {line_number}: {error}"))?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk_length = newline.map_or(available.len(), |position| position + 1);
        if buffer.len().saturating_add(chunk_length) > MAX_LINE_BYTES + 1 {
            return Err(format!("line {line_number}: line too long"));
        }
        buffer.extend_from_slice(&available[..chunk_length]);
        reader.consume(chunk_length);
        if newline.is_some() {
            return Ok(buffer.len());
        }
    }
}

fn validate_order(order: &Order) -> Result<(), String> {
    if order.order_id.is_empty() {
        return Err("order_id must be nonempty".to_owned());
    }
    if order.lane_id.is_empty() {
        return Err("lane_id must be nonempty".to_owned());
    }
    if !order.order_id.is_ascii() || !order.lane_id.is_ascii() {
        return Err("order_id and lane_id must be ASCII".to_owned());
    }
    if !valid_date(&order.service_date) {
        return Err("service_date must be a canonical YYYY-MM-DD date".to_owned());
    }
    if !(1..=MAX_QUANTITY).contains(&order.quantity) {
        return Err("quantity must be in 1..=1000000".to_owned());
    }
    if !(1..=MAX_PRICE_CENTS).contains(&order.limit_price_cents) {
        return Err("limit_price_cents must be in 1..=10000000".to_owned());
    }
    Ok(())
}

fn valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return false;
    }
    let number = |start: usize, end: usize| -> Option<u32> {
        std::str::from_utf8(&bytes[start..end]).ok()?.parse().ok()
    };
    let (Some(year), Some(month), Some(day)) = (number(0, 4), number(5, 7), number(8, 10)) else {
        return false;
    };
    if year == 0 || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days).contains(&day)
}

/// Applies the exact price-time matcher to one market.
///
/// # Errors
///
/// Returns an error if the fill index cannot be represented as `u64`.
pub fn clear_market(key: &MarketKey, orders: &[Order]) -> Result<Vec<Fill>, String> {
    let mut buys: Vec<(&Order, u64)> = orders
        .iter()
        .filter(|order| order.side == Side::Buy)
        .map(|order| (order, order.quantity))
        .collect();
    let mut sells: Vec<(&Order, u64)> = orders
        .iter()
        .filter(|order| order.side == Side::Sell)
        .map(|order| (order, order.quantity))
        .collect();

    buys.sort_by(|(a, _), (b, _)| {
        b.limit_price_cents
            .cmp(&a.limit_price_cents)
            .then_with(|| a.submitted_sequence.cmp(&b.submitted_sequence))
            .then_with(|| a.order_id.as_bytes().cmp(b.order_id.as_bytes()))
    });
    sells.sort_by(|(a, _), (b, _)| {
        a.limit_price_cents
            .cmp(&b.limit_price_cents)
            .then_with(|| a.submitted_sequence.cmp(&b.submitted_sequence))
            .then_with(|| a.order_id.as_bytes().cmp(b.order_id.as_bytes()))
    });

    let (mut buy_index, mut sell_index) = (0, 0);
    let mut fills = Vec::new();
    while buy_index < buys.len() && sell_index < sells.len() {
        let (buy, buy_remaining) = &mut buys[buy_index];
        let (sell, sell_remaining) = &mut sells[sell_index];
        if buy.limit_price_cents < sell.limit_price_cents {
            break;
        }
        let quantity = (*buy_remaining).min(*sell_remaining);
        fills.push(Fill {
            lane_id: key.lane_id.clone(),
            service_date: key.service_date.clone(),
            fill_index: u64::try_from(fills.len()).map_err(|_| "too many fills")?,
            buy_id: buy.order_id.clone(),
            sell_id: sell.order_id.clone(),
            quantity,
            price_cents: sell.limit_price_cents,
        });
        *buy_remaining -= quantity;
        *sell_remaining -= quantity;
        if *buy_remaining == 0 {
            buy_index += 1;
        }
        if *sell_remaining == 0 {
            sell_index += 1;
        }
    }
    Ok(fills)
}

/// Deliberately scans all active orders on every step as an independent oracle.
///
/// # Errors
///
/// Returns an error if the fill index cannot be represented as `u64`.
pub fn slow_reference_market(key: &MarketKey, orders: &[Order]) -> Result<Vec<Fill>, String> {
    let mut remaining: Vec<u64> = orders.iter().map(|order| order.quantity).collect();
    let mut fills = Vec::new();
    loop {
        let mut best_buy = None;
        let mut best_sell = None;
        for (index, order) in orders.iter().enumerate() {
            if remaining[index] == 0 {
                continue;
            }
            match order.side {
                Side::Buy => {
                    let is_better = best_buy.is_none_or(|previous: usize| {
                        let prior = &orders[previous];
                        order.limit_price_cents > prior.limit_price_cents
                            || (order.limit_price_cents == prior.limit_price_cents
                                && (order.submitted_sequence < prior.submitted_sequence
                                    || (order.submitted_sequence == prior.submitted_sequence
                                        && order.order_id.as_bytes() < prior.order_id.as_bytes())))
                    });
                    if is_better {
                        best_buy = Some(index);
                    }
                }
                Side::Sell => {
                    let is_better = best_sell.is_none_or(|previous: usize| {
                        let prior = &orders[previous];
                        order.limit_price_cents < prior.limit_price_cents
                            || (order.limit_price_cents == prior.limit_price_cents
                                && (order.submitted_sequence < prior.submitted_sequence
                                    || (order.submitted_sequence == prior.submitted_sequence
                                        && order.order_id.as_bytes() < prior.order_id.as_bytes())))
                    });
                    if is_better {
                        best_sell = Some(index);
                    }
                }
            }
        }
        let (Some(buy_index), Some(sell_index)) = (best_buy, best_sell) else {
            break;
        };
        let buy = &orders[buy_index];
        let sell = &orders[sell_index];
        if buy.limit_price_cents < sell.limit_price_cents {
            break;
        }
        let quantity = remaining[buy_index].min(remaining[sell_index]);
        fills.push(Fill {
            lane_id: key.lane_id.clone(),
            service_date: key.service_date.clone(),
            fill_index: u64::try_from(fills.len()).map_err(|_| "too many fills")?,
            buy_id: buy.order_id.clone(),
            sell_id: sell.order_id.clone(),
            quantity,
            price_cents: sell.limit_price_cents,
        });
        remaining[buy_index] -= quantity;
        remaining[sell_index] -= quantity;
    }
    Ok(fills)
}
