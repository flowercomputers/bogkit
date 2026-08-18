#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decimal {
    pub coefficient: i128,
    pub scale: u32,
}

impl Decimal {
    #[must_use]
    pub fn to_fixed(self) -> String {
        let negative = self.coefficient.is_negative();
        let mut digits = self.coefficient.unsigned_abs().to_string();
        if self.scale > 0 {
            let minimum = usize::try_from(self.scale).unwrap_or(usize::MAX) + 1;
            if digits.len() < minimum {
                digits.insert_str(0, &"0".repeat(minimum - digits.len()));
            }
            let split = digits.len() - usize::try_from(self.scale).unwrap_or(0);
            digits.insert(split, '.');
        }
        if negative {
            digits.insert(0, '-');
        }
        digits
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transform {
    multiplier_num: i128,
    multiplier_den: i128,
    offset_num: i128,
    offset_den: i128,
}

impl Transform {
    #[must_use]
    pub const fn new(
        multiplier_num: i128,
        multiplier_den: i128,
        offset_num: i128,
        offset_den: i128,
    ) -> Self {
        Self {
            multiplier_num,
            multiplier_den,
            offset_num,
            offset_den,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArithmeticError;

/// Applies a bounded rational affine transform and rounds once to `target_scale`.
///
/// # Errors
///
/// Returns an error when a denominator is invalid or checked `i128` arithmetic overflows.
pub fn convert_decimal(
    source: Decimal,
    transform: Transform,
    target_scale: u32,
) -> Result<Decimal, ArithmeticError> {
    if transform.multiplier_num <= 0 || transform.multiplier_den <= 0 || transform.offset_den <= 0 {
        return Err(ArithmeticError);
    }
    let source_den = checked_power_of_ten(source.scale)?;
    let left = source
        .coefficient
        .checked_mul(transform.multiplier_num)
        .and_then(|value| value.checked_mul(transform.offset_den))
        .ok_or(ArithmeticError)?;
    let right = transform
        .offset_num
        .checked_mul(source_den)
        .and_then(|value| value.checked_mul(transform.multiplier_den))
        .ok_or(ArithmeticError)?;
    let numerator = left.checked_add(right).ok_or(ArithmeticError)?;
    let denominator = source_den
        .checked_mul(transform.multiplier_den)
        .and_then(|value| value.checked_mul(transform.offset_den))
        .ok_or(ArithmeticError)?;
    let target_factor = checked_power_of_ten(target_scale)?;
    let scaled = numerator
        .checked_mul(target_factor)
        .ok_or(ArithmeticError)?;
    let mut coefficient = scaled / denominator;
    let remainder = scaled % denominator;
    let twice_remainder = remainder
        .checked_abs()
        .and_then(|value| value.checked_mul(2))
        .ok_or(ArithmeticError)?;
    let round_away =
        twice_remainder > denominator || (twice_remainder == denominator && coefficient % 2 != 0);
    if round_away {
        coefficient = coefficient
            .checked_add(if scaled.is_negative() { -1 } else { 1 })
            .ok_or(ArithmeticError)?;
    }
    Ok(Decimal {
        coefficient,
        scale: target_scale,
    })
}

fn checked_power_of_ten(scale: u32) -> Result<i128, ArithmeticError> {
    10_i128.checked_pow(scale).ok_or(ArithmeticError)
}

#[derive(Debug, Clone)]
pub struct InputPaths {
    pub units: PathBuf,
    pub analytes: PathBuf,
    pub mappings: PathBuf,
    pub observations: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    None,
    AfterRows(usize),
    BeforeRename,
    AfterRenameBeforeDirectorySync,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub total: usize,
    pub converted: usize,
    pub rejected: usize,
    pub output_sha256: String,
}

#[derive(Debug)]
pub struct RunError {
    pub code: &'static str,
    pub publication_state: PublicationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationState {
    NotPublished,
    PublishedDurabilityUncertain,
}

impl RunError {
    const fn new(code: &'static str) -> Self {
        Self {
            code,
            publication_state: PublicationState::NotPublished,
        }
    }

    const fn post_rename(code: &'static str) -> Self {
        Self {
            code,
            publication_state: PublicationState::PublishedDurabilityUncertain,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for RunError {}

/// Validates all input and atomically publishes a canonical report.
///
/// # Errors
///
/// Returns a stable run-level error for malformed input, invalid references, duplicate IDs,
/// injected exits, or publication failures.
pub fn run(
    inputs: &InputPaths,
    output: &Path,
    fail_point: FailPoint,
) -> Result<RunSummary, RunError> {
    let temporary = temporary_path(output)?;
    ensure_publication_paths_do_not_alias_inputs(inputs, output, &temporary)?;
    let units: Vec<Unit> = read_ndjson(&inputs.units)?;
    let analytes: Vec<Analyte> = read_ndjson(&inputs.analytes)?;
    let mappings: Vec<Mapping> = read_ndjson(&inputs.mappings)?;
    let reference = Reference::validate(units, analytes, mappings)?;

    let file = File::open(&inputs.observations).map_err(|_| RunError::new("READ_ERROR"))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::with_capacity(1_025);
    let mut seen_ids = HashSet::new();
    let mut rows = Vec::new();
    loop {
        let bytes = read_limited_line(&mut reader, &mut buffer)?;
        if bytes == 0 {
            break;
        }
        trim_line_ending(&mut buffer);
        if buffer.len() > 1_024 {
            return Err(RunError::new("OVERLONG_LINE"));
        }
        let observation: Observation =
            serde_json::from_slice(&buffer).map_err(|_| RunError::new("INVALID_NDJSON"))?;
        validate_observation_id(&observation.id)?;
        if !seen_ids.insert(observation.id.clone()) {
            return Err(RunError::new("DUPLICATE_OBSERVATION_ID"));
        }
        rows.push(reference.evaluate(observation));
        if matches!(fail_point, FailPoint::AfterRows(count) if rows.len() == count) {
            return Err(RunError::new("INJECTED_EXIT_AFTER_ROWS"));
        }
        buffer.clear();
    }
    rows.sort_unstable_by(|left, right| left.observation_id().cmp(right.observation_id()));

    let converted = rows
        .iter()
        .filter(|row| matches!(row, ReportRow::Normalized { .. }))
        .count();
    let rejected = rows.len() - converted;
    let output_parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let output_directory =
        File::open(output_parent).map_err(|_| RunError::new("OUTPUT_DIRECTORY_OPEN_ERROR"))?;
    let _ = fs::remove_file(&temporary);
    let temporary_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| RunError::new("WRITE_ERROR"))?;
    let mut writer = BufWriter::new(temporary_file);
    let mut hasher = Sha256::new();
    for row in &rows {
        let line = row.canonical_line()?;
        writer
            .write_all(&line)
            .and_then(|()| writer.write_all(b"\n"))
            .map_err(|_| RunError::new("WRITE_ERROR"))?;
        hasher.update(&line);
        hasher.update(b"\n");
    }
    writer.flush().map_err(|_| RunError::new("WRITE_ERROR"))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|_| RunError::new("WRITE_ERROR"))?;
    if fail_point == FailPoint::BeforeRename {
        return Err(RunError::new("INJECTED_EXIT_BEFORE_RENAME"));
    }
    fs::rename(&temporary, output).map_err(|_| RunError::new("RENAME_ERROR"))?;
    if fail_point == FailPoint::AfterRenameBeforeDirectorySync {
        return Err(RunError::post_rename("POST_RENAME_DURABILITY_UNCERTAIN"));
    }
    output_directory
        .sync_all()
        .map_err(|_| RunError::post_rename("POST_RENAME_DURABILITY_UNCERTAIN"))?;
    Ok(RunSummary {
        total: rows.len(),
        converted,
        rejected,
        output_sha256: format!("{:x}", hasher.finalize()),
    })
}

const MAX_TRANSFORM_COEFFICIENT: i128 = 1_000_000_000_000_000_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Unit {
    symbol: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Analyte {
    #[serde(rename = "analyte_code")]
    code: String,
    dimension: String,
    target_scale: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping {
    source_system: String,
    analyte_code: String,
    source_unit: String,
    target_unit: String,
    expected_dimension: String,
    multiplier_num: i128,
    multiplier_den: i128,
    offset_num: i128,
    offset_den: i128,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    #[serde(rename = "observation_id")]
    id: String,
    source_system: String,
    analyte_code: String,
    source_unit: String,
    declared_dimension: String,
    value: String,
}

#[derive(Debug)]
struct ValidatedMapping {
    target_unit: String,
    expected_dimension: String,
    target_scale: u32,
    transform: Transform,
}

type MappingKey = (String, String, String);

#[derive(Debug)]
struct Reference {
    mappings: HashMap<MappingKey, ValidatedMapping>,
}

impl Reference {
    fn validate(
        units: Vec<Unit>,
        analytes: Vec<Analyte>,
        mappings: Vec<Mapping>,
    ) -> Result<Self, RunError> {
        let mut unit_symbols = HashSet::new();
        for unit in units {
            if unit.symbol.is_empty() || !unit_symbols.insert(unit.symbol) {
                return Err(RunError::new("INVALID_UNIT_SNAPSHOT"));
            }
        }
        let mut analyte_by_code = HashMap::new();
        for analyte in analytes {
            if analyte.code.is_empty()
                || analyte.dimension.is_empty()
                || analyte.target_scale > 9
                || analyte_by_code
                    .insert(analyte.code.clone(), analyte)
                    .is_some()
            {
                return Err(RunError::new("INVALID_ANALYTE_SNAPSHOT"));
            }
        }
        let mut validated = HashMap::new();
        for mapping in mappings {
            let Some(analyte) = analyte_by_code.get(&mapping.analyte_code) else {
                return Err(RunError::new("UNKNOWN_ANALYTE"));
            };
            if !unit_symbols.contains(&mapping.source_unit)
                || !unit_symbols.contains(&mapping.target_unit)
            {
                return Err(RunError::new("UNKNOWN_UNIT_SYMBOL"));
            }
            if mapping.expected_dimension != analyte.dimension {
                return Err(RunError::new("INCONSISTENT_DIMENSION"));
            }
            if mapping.multiplier_num <= 0 || mapping.multiplier_den <= 0 || mapping.offset_den <= 0
            {
                return Err(RunError::new("INVALID_DENOMINATOR_OR_MULTIPLIER"));
            }
            for coefficient in [
                mapping.multiplier_num,
                mapping.multiplier_den,
                mapping.offset_num,
                mapping.offset_den,
            ] {
                if coefficient.unsigned_abs() > MAX_TRANSFORM_COEFFICIENT as u128 {
                    return Err(RunError::new("COEFFICIENT_OUT_OF_RANGE"));
                }
            }
            let key = (
                mapping.source_system,
                mapping.analyte_code,
                mapping.source_unit,
            );
            let value = ValidatedMapping {
                target_unit: mapping.target_unit,
                expected_dimension: mapping.expected_dimension,
                target_scale: analyte.target_scale,
                transform: Transform::new(
                    mapping.multiplier_num,
                    mapping.multiplier_den,
                    mapping.offset_num,
                    mapping.offset_den,
                ),
            };
            if validated.insert(key, value).is_some() {
                return Err(RunError::new("DUPLICATE_MAPPING_KEY"));
            }
        }
        Ok(Self {
            mappings: validated,
        })
    }

    fn evaluate(&self, observation: Observation) -> ReportRow {
        let id = observation.id;
        let key = (
            observation.source_system,
            observation.analyte_code,
            observation.source_unit,
        );
        let Some(mapping) = self.mappings.get(&key) else {
            return ReportRow::rejected(id, "UNKNOWN_MAPPING");
        };
        if observation.declared_dimension != mapping.expected_dimension {
            return ReportRow::rejected(id, "INCOMPATIBLE_DIMENSION");
        }
        let Ok(source) = parse_decimal(&observation.value) else {
            return ReportRow::rejected(id, "INVALID_VALUE");
        };
        let Ok(converted) = convert_decimal(source, mapping.transform, mapping.target_scale) else {
            return ReportRow::rejected(id, "ARITHMETIC_OVERFLOW");
        };
        ReportRow::Normalized {
            observation_id: id,
            target_unit: mapping.target_unit.clone(),
            value: converted.to_fixed(),
        }
    }
}

#[derive(Debug)]
enum ReportRow {
    Normalized {
        observation_id: String,
        target_unit: String,
        value: String,
    },
    Rejected {
        observation_id: String,
        reason: &'static str,
    },
}

impl ReportRow {
    fn rejected(observation_id: String, reason: &'static str) -> Self {
        Self::Rejected {
            observation_id,
            reason,
        }
    }

    fn observation_id(&self) -> &str {
        match self {
            Self::Normalized { observation_id, .. } | Self::Rejected { observation_id, .. } => {
                observation_id
            }
        }
    }

    fn canonical_line(&self) -> Result<Vec<u8>, RunError> {
        match self {
            Self::Normalized {
                observation_id,
                target_unit,
                value,
            } => serde_json::to_vec(&NormalizedOutput {
                observation_id,
                status: "normalized",
                target_unit,
                value,
            }),
            Self::Rejected {
                observation_id,
                reason,
            } => serde_json::to_vec(&RejectedOutput {
                observation_id,
                reason,
                status: "rejected",
            }),
        }
        .map_err(|_| RunError::new("SERIALIZE_ERROR"))
    }
}

#[derive(serde::Serialize)]
struct NormalizedOutput<'a> {
    observation_id: &'a str,
    status: &'static str,
    target_unit: &'a str,
    value: &'a str,
}

#[derive(serde::Serialize)]
struct RejectedOutput<'a> {
    observation_id: &'a str,
    reason: &'a str,
    status: &'static str,
}

fn read_ndjson<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>, RunError> {
    let file = File::open(path).map_err(|_| RunError::new("READ_ERROR"))?;
    let mut reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut buffer = Vec::with_capacity(1_025);
    loop {
        let bytes = read_limited_line(&mut reader, &mut buffer)?;
        if bytes == 0 {
            break;
        }
        trim_line_ending(&mut buffer);
        if buffer.len() > 1_024 {
            return Err(RunError::new("OVERLONG_LINE"));
        }
        rows.push(serde_json::from_slice(&buffer).map_err(|_| RunError::new("INVALID_NDJSON"))?);
        buffer.clear();
    }
    Ok(rows)
}

fn trim_line_ending(buffer: &mut Vec<u8>) {
    if buffer.last() == Some(&b'\n') {
        buffer.pop();
    }
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
}

fn read_limited_line<R: BufRead>(reader: &mut R, buffer: &mut Vec<u8>) -> Result<usize, RunError> {
    loop {
        let available = reader.fill_buf().map_err(|_| RunError::new("READ_ERROR"))?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let chunk_length = newline.map_or(available.len(), |position| position + 1);
        if buffer.len().saturating_add(chunk_length) > 1_025 {
            return Err(RunError::new("OVERLONG_LINE"));
        }
        buffer.extend_from_slice(&available[..chunk_length]);
        reader.consume(chunk_length);
        if newline.is_some() {
            return Ok(buffer.len());
        }
    }
}

fn validate_observation_id(id: &str) -> Result<(), RunError> {
    if id.is_empty() || !id.is_ascii() {
        return Err(RunError::new("INVALID_OBSERVATION_ID"));
    }
    Ok(())
}

fn temporary_path(output: &Path) -> Result<PathBuf, RunError> {
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| RunError::new("INVALID_OUTPUT_PATH"))?;
    Ok(output.with_file_name(format!(".{name}.temporary")))
}

fn ensure_publication_paths_do_not_alias_inputs(
    inputs: &InputPaths,
    output: &Path,
    temporary: &Path,
) -> Result<(), RunError> {
    for input in [
        inputs.units.as_path(),
        inputs.analytes.as_path(),
        inputs.mappings.as_path(),
        inputs.observations.as_path(),
    ] {
        if paths_alias(output, input) {
            return Err(RunError::new("OUTPUT_ALIASES_INPUT"));
        }
        if paths_alias(temporary, input) {
            return Err(RunError::new("OUTPUT_TEMPORARY_ALIASES_INPUT"));
        }
    }
    Ok(())
}

fn paths_alias(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    if let (Ok(left), Ok(right)) = (fs::canonicalize(left), fs::canonicalize(right))
        && left == right
    {
        return true;
    }
    same_existing_file(left, right)
}

#[cfg(unix)]
fn same_existing_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let (Ok(left), Ok(right)) = (fs::metadata(left), fs::metadata(right)) else {
        return false;
    };
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
const fn same_existing_file(_left: &Path, _right: &Path) -> bool {
    false
}

#[derive(Debug, Clone, Copy)]
pub struct GenerationConfig {
    pub seed: u64,
    pub ordinary: usize,
    pub boundary: usize,
    pub rejected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FixtureManifest {
    pub seed: u64,
    pub reference: ReferenceCounts,
    pub observations: ObservationCounts,
    pub digests: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReferenceCounts {
    pub units: usize,
    pub analytes: usize,
    pub mappings: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ObservationCounts {
    pub ordinary: usize,
    pub boundary: usize,
    pub rejected: usize,
    pub total: usize,
}

/// Writes a deterministic synthetic reference snapshot, observations, and SHA-256 manifest.
///
/// # Errors
///
/// Returns an error when counts overflow or a fixture file cannot be created or written.
pub fn generate_fixture(
    directory: &Path,
    config: GenerationConfig,
) -> Result<FixtureManifest, RunError> {
    fs::create_dir_all(directory).map_err(|_| RunError::new("WRITE_ERROR"))?;
    let mut digests = BTreeMap::new();
    digests.insert(
        "units.ndjson".to_string(),
        write_and_hash(&directory.join("units.ndjson"), |writer| {
            for unit in 0..250 {
                writeln!(writer, "{{\"symbol\":\"U{unit:03}\"}}")?;
            }
            Ok(())
        })?,
    );
    digests.insert(
        "analytes.ndjson".to_string(),
        write_and_hash(&directory.join("analytes.ndjson"), |writer| {
            for analyte in 0..300 {
                let dimension = analyte % 12;
                let scale = analyte % 10;
                writeln!(
                    writer,
                    "{{\"analyte_code\":\"A{analyte:03}\",\"dimension\":\"D{dimension:02}\",\"target_scale\":{scale}}}"
                )?;
            }
            Ok(())
        })?,
    );
    digests.insert(
        "mappings.ndjson".to_string(),
        write_and_hash(&directory.join("mappings.ndjson"), |writer| {
            for mapping in 0..12_000 {
                let source = mapping / 300;
                let analyte = mapping % 300;
                let unit = mapping % 250;
                let dimension = analyte % 12;
                writeln!(
                    writer,
                    "{{\"source_system\":\"G{source:03}\",\"analyte_code\":\"A{analyte:03}\",\"source_unit\":\"U{unit:03}\",\"target_unit\":\"U000\",\"expected_dimension\":\"D{dimension:02}\",\"multiplier_num\":1,\"multiplier_den\":1,\"offset_num\":0,\"offset_den\":1}}"
                )?;
            }
            Ok(())
        })?,
    );
    let total = config
        .ordinary
        .checked_add(config.boundary)
        .and_then(|count| count.checked_add(config.rejected))
        .ok_or_else(|| RunError::new("COUNT_OVERFLOW"))?;
    digests.insert(
        "observations.ndjson".to_string(),
        write_and_hash(&directory.join("observations.ndjson"), |writer| {
            for position in 0..total {
                let index = permuted_index(position, total, config.seed);
                let mapping = index % 12_000;
                let source = mapping / 300;
                let analyte = mapping % 300;
                let unit = mapping % 250;
                let dimension = analyte % 12;
                let (source_unit, value) = if index < config.ordinary {
                    (format!("U{unit:03}"), "1.25")
                } else if index < config.ordinary + config.boundary {
                    (format!("U{unit:03}"), "2.5")
                } else {
                    ("UNKNOWN".to_string(), "1")
                };
                writeln!(
                    writer,
                    "{{\"observation_id\":\"obs-{index:07}\",\"source_system\":\"G{source:03}\",\"analyte_code\":\"A{analyte:03}\",\"source_unit\":\"{source_unit}\",\"declared_dimension\":\"D{dimension:02}\",\"value\":\"{value}\"}}"
                )?;
            }
            Ok(())
        })?,
    );
    let manifest = FixtureManifest {
        seed: config.seed,
        reference: ReferenceCounts {
            units: 250,
            analytes: 300,
            mappings: 12_000,
        },
        observations: ObservationCounts {
            ordinary: config.ordinary,
            boundary: config.boundary,
            rejected: config.rejected,
            total,
        },
        digests,
    };
    let manifest_file =
        File::create(directory.join("manifest.json")).map_err(|_| RunError::new("WRITE_ERROR"))?;
    serde_json::to_writer_pretty(manifest_file, &manifest)
        .map_err(|_| RunError::new("WRITE_ERROR"))?;
    Ok(manifest)
}

fn write_and_hash(
    path: &Path,
    write_contents: impl FnOnce(&mut HashingWriter<BufWriter<File>>) -> std::io::Result<()>,
) -> Result<String, RunError> {
    let file = File::create(path).map_err(|_| RunError::new("WRITE_ERROR"))?;
    let mut writer = HashingWriter::new(BufWriter::new(file));
    write_contents(&mut writer).map_err(|_| RunError::new("WRITE_ERROR"))?;
    writer.flush().map_err(|_| RunError::new("WRITE_ERROR"))?;
    Ok(writer.finish())
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> String {
        format!("{:x}", self.hasher.finalize())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn permuted_index(position: usize, total: usize, seed: u64) -> usize {
    if total == 0 {
        return 0;
    }
    let mut multiplier = usize::try_from(seed | 1).unwrap_or(1) % total;
    if multiplier == 0 {
        multiplier = 1;
    }
    while greatest_common_divisor(multiplier, total) != 1 {
        multiplier = (multiplier + 2) % total;
        if multiplier == 0 {
            multiplier = 1;
        }
    }
    let offset = usize::try_from(seed % u64::try_from(total).unwrap_or(u64::MAX)).unwrap_or(0);
    (position.wrapping_mul(multiplier).wrapping_add(offset)) % total
}

const fn greatest_common_divisor(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseDecimalError;

/// Parses the accepted non-scientific decimal grammar without binary floating point.
///
/// # Errors
///
/// Returns an error for invalid grammar, over 29 significant digits, scale above nine, or an
/// `i128` coefficient overflow.
pub fn parse_decimal(input: &str) -> Result<Decimal, ParseDecimalError> {
    let (negative, unsigned) = input
        .strip_prefix('-')
        .map_or((false, input), |rest| (true, rest));
    if unsigned.is_empty() || unsigned.starts_with('+') || unsigned.matches('.').count() > 1 {
        return Err(ParseDecimalError);
    }
    let (whole, fractional) = unsigned
        .split_once('.')
        .map_or((unsigned, ""), |parts| parts);
    if whole.is_empty()
        || (!fractional.is_empty() && fractional.len() > 9)
        || (unsigned.contains('.') && fractional.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fractional.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ParseDecimalError);
    }
    let digits = format!("{whole}{fractional}");
    let significant_digits = digits.trim_start_matches('0').len().max(1);
    if significant_digits > 29 {
        return Err(ParseDecimalError);
    }
    let magnitude = digits.parse::<i128>().map_err(|_| ParseDecimalError)?;
    let coefficient = if negative { -magnitude } else { magnitude };
    Ok(Decimal {
        coefficient,
        scale: u32::try_from(fractional.len()).map_err(|_| ParseDecimalError)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{Decimal, Transform, convert_decimal, parse_decimal, read_limited_line};

    #[test]
    fn decimal_parser_preserves_base_ten_coefficient_and_scale() {
        let parsed = parse_decimal("-1234567890123456789012345678.9").expect("valid decimal");
        assert_eq!(
            parsed.coefficient,
            -12_345_678_901_234_567_890_123_456_789_i128
        );
        assert_eq!(parsed.scale, 1);
    }

    #[test]
    fn decimal_parser_rejects_outside_the_declared_grammar() {
        for invalid in [
            "",
            "-",
            ".1",
            "1.",
            "+1",
            "1e3",
            "NaN",
            "inf",
            "1.1234567890",
            "123456789012345678901234567890",
        ] {
            assert!(parse_decimal(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn affine_conversion_rounds_once_with_half_to_even() {
        let identity = Transform::new(1, 1, 0, 1);
        for (source, scale, expected) in [
            ("2.5", 0, "2"),
            ("3.5", 0, "4"),
            ("-2.5", 0, "-2"),
            ("-3.5", 0, "-4"),
            ("1.249", 2, "1.25"),
        ] {
            let actual = convert_decimal(parse_decimal(source).unwrap(), identity, scale)
                .expect("bounded conversion");
            assert_eq!(actual.to_fixed(), expected, "source {source}");
        }

        let non_integer_with_offset = Transform::new(3, 2, -1, 4);
        let actual = convert_decimal(parse_decimal("1.25").unwrap(), non_integer_with_offset, 3)
            .expect("bounded conversion");
        assert_eq!(actual.to_fixed(), "1.625");
    }

    #[test]
    fn twenty_thousand_generated_cases_match_independent_slow_reference() {
        let mut state = 0x5eed_2026_0818_u64;
        for case in 0..20_000_u32 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let target_scale = case % 10;
            let coefficient = if case % 97 == 0 {
                0
            } else if case % 13 == 0 {
                i128::from(case % 1_000) * 2 + 1
            } else {
                i128::from(state % 2_000_000_000_001) - 1_000_000_000_000
            };
            let source = Decimal {
                coefficient,
                scale: target_scale,
            };
            let transform = if case % 13 == 0 {
                Transform::new(1, 2, 0, 1)
            } else {
                Transform::new(
                    i128::from(state % 17 + 1),
                    i128::from(state.rotate_left(9) % 19 + 1),
                    i128::from(state.rotate_left(17) % 101) - 50,
                    i128::from(state.rotate_left(27) % 11 + 1),
                )
            };
            let fast = convert_decimal(source, transform, target_scale).expect("bounded case");
            let slow = slow_reference(source, transform, target_scale);
            assert_eq!(fast, slow, "generated case {case}");
        }

        let maximum = Decimal {
            coefficient: 99_999_999_999_999_999_999_999_999_999,
            scale: 9,
        };
        assert_eq!(
            convert_decimal(maximum, Transform::new(1, 1, 0, 1), 9).unwrap(),
            slow_reference(maximum, Transform::new(1, 1, 0, 1), 9)
        );
    }

    fn slow_reference(source: Decimal, transform: Transform, target_scale: u32) -> Decimal {
        let source_denominator = 10_i128.pow(source.scale);
        let multiplied_numerator = source.coefficient * transform.multiplier_num;
        let multiplied_denominator = source_denominator * transform.multiplier_den;
        let numerator = multiplied_numerator * transform.offset_den
            + transform.offset_num * multiplied_denominator;
        let denominator = multiplied_denominator * transform.offset_den;
        let scaled_magnitude = (numerator * 10_i128.pow(target_scale)).unsigned_abs();
        let denominator = u128::try_from(denominator).expect("positive denominator");
        let mut rounded_magnitude = scaled_magnitude / denominator;
        let remainder = scaled_magnitude % denominator;
        if remainder * 2 > denominator
            || (remainder * 2 == denominator && rounded_magnitude % 2 == 1)
        {
            rounded_magnitude += 1;
        }
        let rounded = i128::try_from(rounded_magnitude).expect("bounded reference");
        Decimal {
            coefficient: if numerator.is_negative() {
                -rounded
            } else {
                rounded
            },
            scale: target_scale,
        }
    }

    #[test]
    fn limited_reader_never_buffers_more_than_the_declared_line_limit() {
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(vec![b'x'; 3 * 1024 * 1024]));
        let mut buffer = Vec::with_capacity(1_025);

        let error = read_limited_line(&mut reader, &mut buffer).unwrap_err();

        assert_eq!(error.code, "OVERLONG_LINE");
        assert!(buffer.len() <= 1_025);
    }
}
