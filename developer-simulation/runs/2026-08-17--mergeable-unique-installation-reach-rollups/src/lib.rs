#![forbid(unsafe_code)]
// These pedantic lints conflict with bounded numeric domains and evidence-result
// structs in this self-contained prototype; all other Clippy lints stay denied.
#![allow(
    clippy::cast_precision_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::naive_bytecount,
    clippy::similar_names,
    clippy::struct_excessive_bools
)]

use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt::{Display, Formatter, Write as FmtWrite};
use std::io::{Read, Write};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BucketKey {
    pub tenant_id: u32,
    pub unix_hour: i64,
}

impl BucketKey {
    #[must_use]
    pub const fn new(tenant_id: u32, unix_hour: i64) -> Self {
        Self {
            tenant_id,
            unix_hour,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Record {
    pub tenant_id: u32,
    pub unix_hour: i64,
    pub installation_id: [u8; 16],
}

impl Record {
    #[must_use]
    pub const fn new(tenant_id: u32, unix_hour: i64, installation_id: [u8; 16]) -> Self {
        Self {
            tenant_id,
            unix_hour,
            installation_id,
        }
    }

    #[must_use]
    pub const fn key(self) -> BucketKey {
        BucketKey::new(self.tenant_id, self.unix_hour)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayWindow {
    start_hour: i64,
    end_hour: i64,
    min_tenant: u32,
    max_tenant: u32,
}

impl DayWindow {
    pub fn new(start_hour: i64, min_tenant: u32, max_tenant: u32) -> Result<Self, LabError> {
        if min_tenant > max_tenant {
            return Err(LabError::InvalidWindow);
        }
        let end_hour = start_hour.checked_add(23).ok_or(LabError::InvalidWindow)?;
        Ok(Self {
            start_hour,
            end_hour,
            min_tenant,
            max_tenant,
        })
    }

    fn validate(self, record: Record) -> Result<(), LabError> {
        if !(self.min_tenant..=self.max_tenant).contains(&record.tenant_id)
            || !(self.start_hour..=self.end_hour).contains(&record.unix_hour)
        {
            return Err(LabError::InvalidRecord(record.key()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabError {
    InvalidWindow,
    InvalidRecord(BucketKey),
    IncompatibleState,
    InvalidState(&'static str),
    InvalidStateFile(&'static str),
    InvalidRecordFile(&'static str),
    MissingOracle(BucketKey),
    Publication(&'static str),
    PublicationInterrupted,
}

impl Display for LabError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidWindow => f.write_str("invalid day window"),
            Self::InvalidRecord(key) => write!(
                f,
                "record outside declared window: tenant={} hour={}",
                key.tenant_id, key.unix_hour
            ),
            Self::IncompatibleState => f.write_str("incompatible sketch state"),
            Self::InvalidState(reason) => write!(f, "invalid sketch state: {reason}"),
            Self::InvalidStateFile(reason) => write!(f, "invalid state file: {reason}"),
            Self::InvalidRecordFile(reason) => write!(f, "invalid record file: {reason}"),
            Self::MissingOracle(key) => write!(
                f,
                "missing exact oracle bucket: tenant={} hour={}",
                key.tenant_id, key.unix_hour
            ),
            Self::Publication(reason) => write!(f, "report publication failed: {reason}"),
            Self::PublicationInterrupted => f.write_str("simulated interruption before rename"),
        }
    }
}

const HLL_PRECISION: u8 = 12;
const HLL_REGISTER_COUNT: usize = 1 << HLL_PRECISION;
const HLL_MAX_REGISTER: u8 = 65 - HLL_PRECISION;
const STATE_HEADER_LEN: usize = 28;
const STATE_VERSION: u8 = 1;
const HASH_ALGORITHM: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HllState {
    hash_seed: u64,
    registers: Box<[u8; HLL_REGISTER_COUNT]>,
}

impl HllState {
    #[must_use]
    pub fn new(hash_seed: u64) -> Self {
        Self {
            hash_seed,
            registers: Box::new([0; HLL_REGISTER_COUNT]),
        }
    }

    pub fn add(&mut self, installation_id: [u8; 16]) {
        let hash = hash_installation(installation_id, self.hash_seed);
        let index = usize::try_from(hash >> (u64::BITS - u32::from(HLL_PRECISION)))
            .expect("12-bit register index fits usize");
        let shifted = hash << HLL_PRECISION;
        let rank = (shifted.leading_zeros() + 1).min(u32::from(HLL_MAX_REGISTER));
        self.registers[index] =
            self.registers[index].max(u8::try_from(rank).expect("rank is capped at 53"));
    }

    pub fn merge(&mut self, other: &Self) -> Result<(), LabError> {
        if self.hash_seed != other.hash_seed {
            return Err(LabError::IncompatibleState);
        }
        for (register, incoming) in self.registers.iter_mut().zip(other.registers.iter()) {
            *register = (*register).max(*incoming);
        }
        Ok(())
    }

    #[must_use]
    pub fn estimate(&self) -> f64 {
        let register_count = 4_096.0;
        let harmonic_sum: f64 = self
            .registers
            .iter()
            .map(|&register| 2_f64.powi(-i32::from(register)))
            .sum();
        let alpha = 0.7213 / (1.0 + 1.079 / register_count);
        let raw = alpha * register_count * register_count / harmonic_sum;
        let zero_count = self
            .registers
            .iter()
            .filter(|&&register| register == 0)
            .count();
        if zero_count > 0 {
            let zero_count = u32::try_from(zero_count).expect("register count fits u32");
            let linear = register_count * (register_count / f64::from(zero_count)).ln();
            if linear <= 12_000.0 {
                return linear;
            }
        }
        raw
    }

    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(STATE_HEADER_LEN + HLL_REGISTER_COUNT);
        bytes.extend_from_slice(b"RCHS");
        bytes.push(STATE_VERSION);
        bytes.push(HLL_PRECISION);
        bytes.extend_from_slice(&HASH_ALGORITHM.to_le_bytes());
        bytes.extend_from_slice(&self.hash_seed.to_le_bytes());
        bytes.extend_from_slice(&4_096_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(self.registers.as_slice());
        let checksum = state_checksum(&bytes[..20], &bytes[STATE_HEADER_LEN..]);
        bytes[20..STATE_HEADER_LEN].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LabError> {
        if bytes.len() != STATE_HEADER_LEN + HLL_REGISTER_COUNT {
            return Err(LabError::InvalidState("wrong length"));
        }
        if &bytes[..4] != b"RCHS" {
            return Err(LabError::InvalidState("wrong magic"));
        }
        if bytes[4] != STATE_VERSION {
            return Err(LabError::InvalidState("unknown version"));
        }
        if bytes[5] != HLL_PRECISION {
            return Err(LabError::InvalidState("unsupported precision"));
        }
        let algorithm = u16::from_le_bytes(bytes[6..8].try_into().expect("fixed slice"));
        if algorithm != HASH_ALGORITHM {
            return Err(LabError::InvalidState("unknown hash algorithm"));
        }
        let payload_len = u32::from_le_bytes(bytes[16..20].try_into().expect("fixed slice"));
        if payload_len as usize != HLL_REGISTER_COUNT {
            return Err(LabError::InvalidState("wrong register count"));
        }
        let expected_checksum =
            u64::from_le_bytes(bytes[20..STATE_HEADER_LEN].try_into().expect("fixed slice"));
        if state_checksum(&bytes[..20], &bytes[STATE_HEADER_LEN..]) != expected_checksum {
            return Err(LabError::InvalidState("checksum mismatch"));
        }
        if bytes[STATE_HEADER_LEN..]
            .iter()
            .any(|&register| register > HLL_MAX_REGISTER)
        {
            return Err(LabError::InvalidState("impossible register value"));
        }
        let hash_seed = u64::from_le_bytes(bytes[8..16].try_into().expect("fixed slice"));
        let mut registers = Box::new([0_u8; HLL_REGISTER_COUNT]);
        registers.copy_from_slice(&bytes[STATE_HEADER_LEN..]);
        Ok(Self {
            hash_seed,
            registers,
        })
    }
}

fn hash_installation(id: [u8; 16], seed: u64) -> u64 {
    let low = u64::from_le_bytes(id[..8].try_into().expect("fixed slice"));
    let high = u64::from_le_bytes(id[8..].try_into().expect("fixed slice"));
    avalanche(
        avalanche(low ^ seed) ^ avalanche(high ^ seed.rotate_left(29) ^ 0x9e37_79b9_7f4a_7c15),
    )
}

fn avalanche(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn state_checksum(header: &[u8], registers: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in header.iter().chain(registers) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl Error for LabError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproxRollup {
    window: DayWindow,
    hash_seed: u64,
    buckets: BTreeMap<BucketKey, HllState>,
}

impl ApproxRollup {
    #[must_use]
    pub fn new(window: DayWindow, hash_seed: u64) -> Self {
        Self {
            window,
            hash_seed,
            buckets: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, record: Record) -> Result<(), LabError> {
        self.window.validate(record)?;
        self.buckets
            .entry(record.key())
            .or_insert_with(|| HllState::new(self.hash_seed))
            .add(record.installation_id);
        Ok(())
    }

    pub fn merge(&mut self, other: &Self) -> Result<(), LabError> {
        if self.window != other.window || self.hash_seed != other.hash_seed {
            return Err(LabError::IncompatibleState);
        }
        for (key, state) in &other.buckets {
            self.buckets
                .entry(*key)
                .or_insert_with(|| HllState::new(self.hash_seed))
                .merge(state)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn state_file_bytes(&self) -> Vec<u8> {
        let per_bucket = 12 + STATE_HEADER_LEN + HLL_REGISTER_COUNT;
        let mut bytes = Vec::with_capacity(20 + self.buckets.len() * per_bucket);
        bytes.extend_from_slice(b"RCHF");
        bytes.push(2);
        bytes.extend_from_slice(&[0; 3]);
        let bucket_count = u32::try_from(self.buckets.len()).expect("bucket count fits u32");
        bytes.extend_from_slice(&bucket_count.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        for (key, state) in &self.buckets {
            bytes.extend_from_slice(&key.tenant_id.to_le_bytes());
            bytes.extend_from_slice(&key.unix_hour.to_le_bytes());
            bytes.extend_from_slice(&state.to_bytes());
        }
        let checksum = state_checksum(&bytes[..12], &bytes[20..]);
        bytes[12..20].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn report_bytes(&self, exact: &ExactRollup) -> Result<Vec<u8>, LabError> {
        if self.window != exact.window {
            return Err(LabError::Publication("candidate/oracle bucket mismatch"));
        }
        let counts: BTreeMap<_, _> = exact
            .buckets
            .iter()
            .map(|(key, ids)| (*key, ids.len()))
            .collect();
        self.report_from_counts(&counts)
    }

    pub fn report_from_counts(
        &self,
        counts: &BTreeMap<BucketKey, usize>,
    ) -> Result<Vec<u8>, LabError> {
        if self.buckets.len() != counts.len() {
            return Err(LabError::Publication("candidate/oracle bucket mismatch"));
        }
        let mut report = String::from(
            "tenant_id\tunix_hour\texact_count\testimate\tsigned_relative_error\tabsolute_relative_error\tserialized_size\n",
        );
        for (key, exact_count) in counts {
            let state = self.buckets.get(key).ok_or(LabError::MissingOracle(*key))?;
            let estimate = state.estimate();
            let signed_error = (estimate - *exact_count as f64) / *exact_count as f64;
            writeln!(
                report,
                "{}\t{}\t{}\t{estimate:.6}\t{signed_error:.9}\t{:.9}\t{}",
                key.tenant_id,
                key.unix_hour,
                exact_count,
                signed_error.abs(),
                state.to_bytes().len()
            )
            .expect("writing to String cannot fail");
        }
        Ok(report.into_bytes())
    }

    pub fn from_state_file_bytes(
        window: DayWindow,
        hash_seed: u64,
        bytes: &[u8],
    ) -> Result<Self, LabError> {
        if bytes.len() < 20 || &bytes[..4] != b"RCHF" {
            return Err(LabError::InvalidStateFile("wrong header"));
        }
        if bytes[4] != 2 || bytes[5..8] != [0; 3] {
            return Err(LabError::InvalidStateFile("unsupported version"));
        }
        let count = usize::try_from(u32::from_le_bytes(
            bytes[8..12].try_into().expect("fixed slice"),
        ))
        .expect("u32 fits usize on supported targets");
        let row_len = 12 + STATE_HEADER_LEN + HLL_REGISTER_COUNT;
        let expected_len = 20_usize
            .checked_add(
                count
                    .checked_mul(row_len)
                    .ok_or(LabError::InvalidStateFile("length overflow"))?,
            )
            .ok_or(LabError::InvalidStateFile("length overflow"))?;
        if bytes.len() != expected_len {
            return Err(LabError::InvalidStateFile("wrong length"));
        }
        let expected_checksum = u64::from_le_bytes(bytes[12..20].try_into().expect("fixed slice"));
        if state_checksum(&bytes[..12], &bytes[20..]) != expected_checksum {
            return Err(LabError::InvalidStateFile("file checksum mismatch"));
        }
        let mut rollup = Self::new(window, hash_seed);
        let mut last_key = None;
        for row in bytes[20..].chunks_exact(row_len) {
            let tenant_id = u32::from_le_bytes(row[..4].try_into().expect("fixed slice"));
            let unix_hour = i64::from_le_bytes(row[4..12].try_into().expect("fixed slice"));
            let key = BucketKey::new(tenant_id, unix_hour);
            window.validate(Record::new(tenant_id, unix_hour, [0; 16]))?;
            if last_key.is_some_and(|previous| previous >= key) {
                return Err(LabError::InvalidStateFile("noncanonical or duplicate key"));
            }
            let state = HllState::from_bytes(&row[12..])?;
            if state.hash_seed != hash_seed {
                return Err(LabError::IncompatibleState);
            }
            rollup.buckets.insert(key, state);
            last_key = Some(key);
        }
        Ok(rollup)
    }

    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    #[must_use]
    pub fn max_serialized_bucket_size(&self) -> usize {
        self.buckets
            .values()
            .map(|state| state.to_bytes().len())
            .max()
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishMode {
    Commit,
    InterruptAfterTemp,
}

pub fn publish_report(
    path: &Path,
    approx: &ApproxRollup,
    exact: &ExactRollup,
    mode: PublishMode,
) -> Result<(), LabError> {
    let bytes = approx.report_bytes(exact)?;
    publish_atomic(path, &bytes, mode)
}

fn publish_atomic(path: &Path, bytes: &[u8], mode: PublishMode) -> Result<(), LabError> {
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::File::create(&temporary)
        .map_err(|_| LabError::Publication("create temporary report"))?;
    file.write_all(bytes)
        .map_err(|_| LabError::Publication("write temporary report"))?;
    file.sync_all()
        .map_err(|_| LabError::Publication("sync temporary report"))?;
    drop(file);
    if mode == PublishMode::InterruptAfterTemp {
        let _ = std::fs::remove_file(&temporary);
        return Err(LabError::PublicationInterrupted);
    }
    std::fs::rename(&temporary, path).map_err(|_| LabError::Publication("rename validated report"))
}

#[must_use]
pub fn digest_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub const ACCURACY_CARDINALITIES: [u64; 6] = [100, 500, 1_000, 5_000, 10_000, 50_000];
pub const ACCURACY_SEEDS: u32 = 20;
pub const ACCURACY_BUCKETS_PER_CARDINALITY: u32 = 24;

#[derive(Debug, Clone)]
pub struct AccuracyObservation {
    pub seed: u32,
    pub cardinality: u64,
    pub bucket: u32,
    pub estimate: f64,
    pub signed_relative_error: f64,
    pub absolute_relative_error: f64,
}

#[derive(Debug, Clone)]
pub struct AccuracySummary {
    pub median_absolute_error: f64,
    pub p95_absolute_error: f64,
    pub worst_absolute_error: f64,
    pub positive_count: usize,
    pub negative_count: usize,
    pub positive_mean_error: f64,
    pub negative_mean_error: f64,
    pub worst_positive_case: (u32, u64, u32, f64),
    pub worst_negative_case: (u32, u64, u32, f64),
}

#[derive(Debug, Clone)]
pub struct AccuracyRun {
    pub observations: Vec<AccuracyObservation>,
    pub summary: AccuracySummary,
}

impl AccuracyRun {
    #[must_use]
    pub fn to_tsv(&self) -> String {
        let mut output = String::from(
            "seed\tcardinality\tbucket\testimate\tsigned_relative_error\tabsolute_relative_error\n",
        );
        for observation in &self.observations {
            writeln!(
                output,
                "{}\t{}\t{}\t{:.6}\t{:.9}\t{:.9}",
                observation.seed,
                observation.cardinality,
                observation.bucket,
                observation.estimate,
                observation.signed_relative_error,
                observation.absolute_relative_error
            )
            .expect("writing to String cannot fail");
        }
        output
    }
}

#[must_use]
pub fn run_accuracy_matrix(hash_seed: u64, generator_seed: u64) -> AccuracyRun {
    let capacity = usize::try_from(
        u64::from(ACCURACY_SEEDS)
            * u64::from(ACCURACY_BUCKETS_PER_CARDINALITY)
            * ACCURACY_CARDINALITIES.len() as u64,
    )
    .expect("accuracy matrix fits usize");
    let mut observations = Vec::with_capacity(capacity);
    for seed in 0..ACCURACY_SEEDS {
        for &cardinality in &ACCURACY_CARDINALITIES {
            for bucket in 0..ACCURACY_BUCKETS_PER_CARDINALITY {
                let mut state = HllState::new(hash_seed);
                for id in 0..cardinality {
                    state.add(accuracy_id(generator_seed, seed, cardinality, bucket, id));
                }
                for id in 0..(cardinality / 4) {
                    state.add(accuracy_id(generator_seed, seed, cardinality, bucket, id));
                }
                let estimate = state.estimate();
                let signed_relative_error = (estimate - cardinality as f64) / cardinality as f64;
                observations.push(AccuracyObservation {
                    seed,
                    cardinality,
                    bucket,
                    estimate,
                    signed_relative_error,
                    absolute_relative_error: signed_relative_error.abs(),
                });
            }
        }
    }
    let summary = summarize_accuracy(&observations);
    AccuracyRun {
        observations,
        summary,
    }
}

fn accuracy_id(generator_seed: u64, seed: u32, cardinality: u64, bucket: u32, id: u64) -> [u8; 16] {
    let namespace = generator_seed
        ^ u64::from(seed).rotate_left(11)
        ^ cardinality.rotate_left(27)
        ^ u64::from(bucket).rotate_left(43);
    let input = namespace ^ id.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&avalanche(input).to_le_bytes());
    bytes[8..].copy_from_slice(&avalanche(input ^ 0xa409_3822_299f_31d0).to_le_bytes());
    bytes
}

fn summarize_accuracy(observations: &[AccuracyObservation]) -> AccuracySummary {
    let mut absolute: Vec<_> = observations
        .iter()
        .map(|observation| observation.absolute_relative_error)
        .collect();
    absolute.sort_by(f64::total_cmp);
    let positive: Vec<_> = observations
        .iter()
        .filter(|observation| observation.signed_relative_error >= 0.0)
        .collect();
    let negative: Vec<_> = observations
        .iter()
        .filter(|observation| observation.signed_relative_error < 0.0)
        .collect();
    let worst_positive = positive
        .iter()
        .max_by(|left, right| {
            left.signed_relative_error
                .total_cmp(&right.signed_relative_error)
        })
        .expect("matrix has positive errors");
    let worst_negative = negative
        .iter()
        .min_by(|left, right| {
            left.signed_relative_error
                .total_cmp(&right.signed_relative_error)
        })
        .expect("matrix has negative errors");
    AccuracySummary {
        median_absolute_error: nearest_rank(&absolute, 1, 2),
        p95_absolute_error: nearest_rank(&absolute, 95, 100),
        worst_absolute_error: *absolute.last().expect("matrix is nonempty"),
        positive_count: positive.len(),
        negative_count: negative.len(),
        positive_mean_error: positive
            .iter()
            .map(|observation| observation.signed_relative_error)
            .sum::<f64>()
            / positive.len() as f64,
        negative_mean_error: negative
            .iter()
            .map(|observation| observation.signed_relative_error)
            .sum::<f64>()
            / negative.len() as f64,
        worst_positive_case: (
            worst_positive.seed,
            worst_positive.cardinality,
            worst_positive.bucket,
            worst_positive.signed_relative_error,
        ),
        worst_negative_case: (
            worst_negative.seed,
            worst_negative.cardinality,
            worst_negative.bucket,
            worst_negative.signed_relative_error,
        ),
    }
}

#[derive(Debug, Clone)]
pub struct LoadErrorSummary {
    pub median_absolute_error: f64,
    pub p95_absolute_error: f64,
    pub worst_absolute_error: f64,
    pub positive_count: usize,
    pub negative_count: usize,
    pub positive_mean_error: f64,
    pub negative_mean_error: f64,
}

#[derive(Debug, Clone)]
pub struct LoadVerification {
    pub record_count: u64,
    pub unique_bucket_occurrences: usize,
    pub bucket_count: usize,
    pub cardinality_1k_buckets: usize,
    pub cardinality_5k_buckets: usize,
    pub cardinality_10k_buckets: usize,
    pub duplicates_byte_identical: bool,
    pub direct_and_sharded_byte_identical: bool,
    pub merge_permutation_digests: Vec<u64>,
    pub repeated_state_byte_identical: bool,
    pub repeated_report_byte_identical: bool,
    pub max_serialized_bucket_size: usize,
    pub total_serialized_bucket_state: usize,
    pub state_digest: u64,
    pub report_digest: u64,
    pub summary_digest: u64,
    pub errors: LoadErrorSummary,
    pub report_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BenchmarkResult {
    pub mode: &'static str,
    pub record_count: u64,
    pub bucket_count: usize,
    pub unique_bucket_occurrences: Option<usize>,
    pub max_serialized_bucket_size: Option<usize>,
    pub digest: u64,
}

pub fn run_exact_benchmark() -> Result<BenchmarkResult, LabError> {
    let window = DayWindow::new(0, 0, 239)?;
    let corpus = LoadCorpus::new(0x243f_6a88_85a3_08d3);
    let mut rollup = ExactRollup::new(window);
    let mut record_count = 0_u64;
    for shard in 0..LOAD_SHARDS {
        for record in corpus.shard(shard)? {
            rollup.add(record)?;
            record_count += 1;
        }
    }
    let counts = rollup.into_counts();
    let unique_bucket_occurrences = counts.values().sum();
    let mut canonical_counts = Vec::with_capacity(counts.len() * 20);
    for (key, count) in &counts {
        canonical_counts.extend_from_slice(&key.tenant_id.to_le_bytes());
        canonical_counts.extend_from_slice(&key.unix_hour.to_le_bytes());
        canonical_counts.extend_from_slice(&(*count as u64).to_le_bytes());
    }
    Ok(BenchmarkResult {
        mode: "exact",
        record_count,
        bucket_count: counts.len(),
        unique_bucket_occurrences: Some(unique_bucket_occurrences),
        max_serialized_bucket_size: None,
        digest: digest_bytes(&canonical_counts),
    })
}

pub fn run_candidate_benchmark() -> Result<BenchmarkResult, LabError> {
    let window = DayWindow::new(0, 0, 239)?;
    let corpus = LoadCorpus::new(0x243f_6a88_85a3_08d3);
    let rollup = aggregate_candidate_all(corpus, window, 0xbb67_ae85_84ca_a73b)?;
    let state_bytes = rollup.state_file_bytes();
    Ok(BenchmarkResult {
        mode: "candidate",
        record_count: LOAD_RECORDS,
        bucket_count: rollup.bucket_count(),
        unique_bucket_occurrences: None,
        max_serialized_bucket_size: Some(rollup.max_serialized_bucket_size()),
        digest: digest_bytes(&state_bytes),
    })
}

pub fn run_load_verification(temporary: &Path) -> Result<LoadVerification, LabError> {
    if temporary.exists() {
        std::fs::remove_dir_all(temporary)
            .map_err(|_| LabError::Publication("clear load temporary directory"))?;
    }
    std::fs::create_dir_all(temporary)
        .map_err(|_| LabError::Publication("create load temporary directory"))?;
    let result = run_load_verification_inner(temporary);
    let cleanup = std::fs::remove_dir_all(temporary)
        .map_err(|_| LabError::Publication("remove load temporary directory"));
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn run_load_verification_inner(temporary: &Path) -> Result<LoadVerification, LabError> {
    const ID_SEED: u64 = 0x243f_6a88_85a3_08d3;
    const HASH_SEED: u64 = 0xbb67_ae85_84ca_a73b;
    let window = DayWindow::new(0, 0, 239)?;
    let corpus = LoadCorpus::new(ID_SEED);

    let mut exact = ExactRollup::new(window);
    let mut record_count = 0_u64;
    for shard in 0..LOAD_SHARDS {
        for record in corpus.shard(shard)? {
            exact.add(record)?;
            record_count += 1;
        }
    }
    let counts = exact.into_counts();
    let unique_bucket_occurrences: usize = counts.values().sum();
    let cardinality_1k_buckets = counts.values().filter(|&&count| count == 1_000).count();
    let cardinality_5k_buckets = counts.values().filter(|&&count| count == 5_000).count();
    let cardinality_10k_buckets = counts.values().filter(|&&count| count == 10_000).count();

    let direct = aggregate_candidate_all(corpus, window, HASH_SEED)?;
    let direct_bytes = direct.state_file_bytes();
    let report_bytes = direct.report_from_counts(&counts)?;
    let errors = load_error_summary(&direct, &counts)?;
    let max_serialized_bucket_size = direct.max_serialized_bucket_size();
    let total_serialized_bucket_state = max_serialized_bucket_size * direct.bucket_count();
    let state_digest = digest_bytes(&direct_bytes);
    let report_digest = digest_bytes(&report_bytes);

    let mut unique_only = ApproxRollup::new(window, HASH_SEED);
    for index in 0..LOAD_UNIQUE_OCCURRENCES {
        unique_only.add(
            corpus
                .logical(index)
                .ok_or(LabError::InvalidRecordFile("missing unique record"))?,
        )?;
    }
    let duplicates_byte_identical = unique_only.state_file_bytes() == direct_bytes;
    drop(unique_only);
    drop(direct);

    for shard in 0..LOAD_SHARDS {
        let mut shard_rollup = ApproxRollup::new(window, HASH_SEED);
        for record in corpus.shard(shard)? {
            shard_rollup.add(record)?;
        }
        std::fs::write(
            temporary.join(format!("shard-{shard}.state")),
            shard_rollup.state_file_bytes(),
        )
        .map_err(|_| LabError::Publication("write temporary shard state"))?;
    }

    let direct_reference = ApproxRollup::from_state_file_bytes(window, HASH_SEED, &direct_bytes)?;
    let mut merged = ApproxRollup::new(window, HASH_SEED);
    for shard in 0..LOAD_SHARDS {
        let shard_rollup = read_shard_rollup(temporary, shard, window, HASH_SEED)?;
        merged.merge(&shard_rollup)?;
    }
    let direct_and_sharded_byte_identical = merged.state_file_bytes() == direct_bytes;
    drop(merged);

    let mut merge_permutation_digests = Vec::with_capacity(50);
    for seed in 0_u64..50 {
        let mut permuted = ApproxRollup::new(window, HASH_SEED);
        for shard in shuffled_shard_order(seed + 1) {
            let shard_rollup = read_shard_rollup(temporary, shard, window, HASH_SEED)?;
            permuted.merge(&shard_rollup)?;
        }
        if permuted != direct_reference {
            return Err(LabError::InvalidStateFile("merge permutation mismatch"));
        }
        merge_permutation_digests.push(digest_bytes(&permuted.state_file_bytes()));
    }

    let repeated = aggregate_candidate_all(corpus, window, HASH_SEED)?;
    let repeated_state_byte_identical = repeated.state_file_bytes() == direct_bytes;
    let repeated_report_byte_identical = repeated.report_from_counts(&counts)? == report_bytes;

    let summary_bytes = format!(
        "records={record_count}\nunique={unique_bucket_occurrences}\nbuckets={}\nstate_digest={state_digest:016x}\nreport_digest={report_digest:016x}\n",
        counts.len()
    );
    Ok(LoadVerification {
        record_count,
        unique_bucket_occurrences,
        bucket_count: counts.len(),
        cardinality_1k_buckets,
        cardinality_5k_buckets,
        cardinality_10k_buckets,
        duplicates_byte_identical,
        direct_and_sharded_byte_identical,
        merge_permutation_digests,
        repeated_state_byte_identical,
        repeated_report_byte_identical,
        max_serialized_bucket_size,
        total_serialized_bucket_state,
        state_digest,
        report_digest,
        summary_digest: digest_bytes(summary_bytes.as_bytes()),
        errors,
        report_bytes,
    })
}

fn aggregate_candidate_all(
    corpus: LoadCorpus,
    window: DayWindow,
    hash_seed: u64,
) -> Result<ApproxRollup, LabError> {
    let mut rollup = ApproxRollup::new(window, hash_seed);
    for shard in 0..LOAD_SHARDS {
        for record in corpus.shard(shard)? {
            rollup.add(record)?;
        }
    }
    Ok(rollup)
}

fn read_shard_rollup(
    temporary: &Path,
    shard: usize,
    window: DayWindow,
    hash_seed: u64,
) -> Result<ApproxRollup, LabError> {
    let bytes = std::fs::read(temporary.join(format!("shard-{shard}.state")))
        .map_err(|_| LabError::Publication("read temporary shard state"))?;
    ApproxRollup::from_state_file_bytes(window, hash_seed, &bytes)
}

fn shuffled_shard_order(seed: u64) -> [usize; LOAD_SHARDS] {
    let mut order = [0, 1, 2, 3, 4, 5, 6, 7];
    let mut state = seed;
    for index in (1..LOAD_SHARDS).rev() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let modulus = u64::try_from(index + 1).expect("shard index fits u64");
        let swap = usize::try_from(state.wrapping_mul(0x2545_f491_4f6c_dd1d) % modulus)
            .expect("swap index fits usize");
        order.swap(index, swap);
    }
    order
}

fn load_error_summary(
    rollup: &ApproxRollup,
    counts: &BTreeMap<BucketKey, usize>,
) -> Result<LoadErrorSummary, LabError> {
    let mut signed = Vec::with_capacity(counts.len());
    for (key, exact_count) in counts {
        let state = rollup
            .buckets
            .get(key)
            .ok_or(LabError::MissingOracle(*key))?;
        signed.push((state.estimate() - *exact_count as f64) / *exact_count as f64);
    }
    let mut absolute: Vec<_> = signed.iter().map(|error| error.abs()).collect();
    absolute.sort_by(f64::total_cmp);
    let positive: Vec<_> = signed
        .iter()
        .copied()
        .filter(|error| *error >= 0.0)
        .collect();
    let negative: Vec<_> = signed
        .iter()
        .copied()
        .filter(|error| *error < 0.0)
        .collect();
    Ok(LoadErrorSummary {
        median_absolute_error: nearest_rank(&absolute, 1, 2),
        p95_absolute_error: nearest_rank(&absolute, 95, 100),
        worst_absolute_error: *absolute.last().expect("load has buckets"),
        positive_count: positive.len(),
        negative_count: negative.len(),
        positive_mean_error: positive.iter().sum::<f64>() / positive.len() as f64,
        negative_mean_error: negative.iter().sum::<f64>() / negative.len() as f64,
    })
}

#[derive(Debug, Clone)]
pub struct AdversarialVerification {
    pub id_pattern_cases: usize,
    pub worst_absolute_error: f64,
    pub reverse_order_byte_identical: bool,
    pub thousand_duplicates_byte_identical: bool,
    pub hot_shard_merge_byte_identical: bool,
    pub empty_input_is_empty: bool,
    pub one_bucket_is_one_bucket: bool,
}

pub fn run_adversarial_verification() -> Result<AdversarialVerification, LabError> {
    const HASH_SEED: u64 = 0xbb67_ae85_84ca_a73b;
    let sequential: Vec<_> = (0_u64..10_000).map(sequential_id).collect();
    let shared_prefix: Vec<_> = (0_u8..=u8::MAX)
        .map(|last| {
            let mut id = [0xab; 16];
            id[15] = last;
            id
        })
        .collect();
    let one_changing_byte: Vec<_> = std::iter::once([0_u8; 16])
        .chain((0_usize..16).flat_map(|position| {
            (1_u8..=u8::MAX).map(move |value| {
                let mut id = [0_u8; 16];
                id[position] = value;
                id
            })
        }))
        .collect();
    let mut reverse = sequential.clone();
    reverse.reverse();
    let patterns = [&sequential, &shared_prefix, &one_changing_byte, &reverse];
    let mut worst_absolute_error = 0.0_f64;
    for pattern in patterns {
        let mut state = HllState::new(HASH_SEED);
        for &id in pattern {
            state.add(id);
        }
        let error = (state.estimate() - pattern.len() as f64).abs() / pattern.len() as f64;
        worst_absolute_error = worst_absolute_error.max(error);
    }

    let mut forward_state = HllState::new(HASH_SEED);
    let mut reverse_state = HllState::new(HASH_SEED);
    for &id in &sequential {
        forward_state.add(id);
    }
    for &id in &reverse {
        reverse_state.add(id);
    }
    let reverse_order_byte_identical = forward_state.to_bytes() == reverse_state.to_bytes();

    let mut duplicated = HllState::new(HASH_SEED);
    for &id in sequential.iter().take(128) {
        duplicated.add(id);
    }
    let before_duplicates = duplicated.to_bytes();
    for _ in 0..1_000 {
        for &id in sequential.iter().take(128) {
            duplicated.add(id);
        }
    }
    let thousand_duplicates_byte_identical = duplicated.to_bytes() == before_duplicates;

    let window = DayWindow::new(0, 0, 1)?;
    let mut direct = ApproxRollup::new(window, HASH_SEED);
    let mut shards: Vec<_> = (0..LOAD_SHARDS)
        .map(|_| ApproxRollup::new(window, HASH_SEED))
        .collect();
    for value in 0_u64..2_000 {
        let record = Record::new(0, 0, sequential_id(value));
        direct.add(record)?;
        shards[0].add(record)?;
    }
    for value in 0_u64..100 {
        let record = Record::new(1, 0, sequential_id(value ^ (1_u64 << 63)));
        direct.add(record)?;
        let shard = 1 + usize::try_from(value % 7).expect("value modulo seven fits usize");
        shards[shard].add(record)?;
    }
    let mut hot_merged = ApproxRollup::new(window, HASH_SEED);
    for shard in shards {
        hot_merged.merge(&shard)?;
    }
    let hot_shard_merge_byte_identical = hot_merged.state_file_bytes() == direct.state_file_bytes();

    let empty = ApproxRollup::new(window, HASH_SEED);
    let empty_input_is_empty = empty.bucket_count() == 0 && empty.state_file_bytes().len() == 20;
    let mut one = ApproxRollup::new(window, HASH_SEED);
    one.add(Record::new(0, 0, [7; 16]))?;

    Ok(AdversarialVerification {
        id_pattern_cases: 4,
        worst_absolute_error,
        reverse_order_byte_identical,
        thousand_duplicates_byte_identical,
        hot_shard_merge_byte_identical,
        empty_input_is_empty,
        one_bucket_is_one_bucket: one.bucket_count() == 1,
    })
}

fn sequential_id(value: u64) -> [u8; 16] {
    let mut id = [0_u8; 16];
    id[8..].copy_from_slice(&value.to_be_bytes());
    id
}

fn nearest_rank(sorted: &[f64], numerator: usize, denominator: usize) -> f64 {
    let rank = sorted.len().saturating_mul(numerator).div_ceil(denominator);
    sorted[rank.saturating_sub(1)]
}

pub const LOAD_UNIQUE_OCCURRENCES: u64 = 10_800_000;
pub const LOAD_RECORDS: u64 = 12_000_000;
pub const LOAD_SHARDS: usize = 8;
const LOAD_RECORDS_PER_SHARD: u64 = LOAD_RECORDS / LOAD_SHARDS as u64;
const UNIQUE_PER_TENANT: u64 = 45_000;
const SHUFFLE_MULTIPLIER: u64 = 1_000_003;

#[derive(Debug, Clone, Copy)]
pub struct LoadCorpus {
    id_seed: u64,
}

impl LoadCorpus {
    #[must_use]
    pub const fn new(id_seed: u64) -> Self {
        Self { id_seed }
    }

    #[must_use]
    pub fn logical(self, index: u64) -> Option<Record> {
        if index >= LOAD_RECORDS {
            return None;
        }
        let unique_index = if index < LOAD_UNIQUE_OCCURRENCES {
            index
        } else {
            (index - LOAD_UNIQUE_OCCURRENCES).wrapping_mul(SHUFFLE_MULTIPLIER)
                % LOAD_UNIQUE_OCCURRENCES
        };
        let tenant_id = u32::try_from(unique_index / UNIQUE_PER_TENANT).ok()?;
        let within_tenant = unique_index % UNIQUE_PER_TENANT;
        let (unix_hour, within_bucket) = if within_tenant < 20_000 {
            (within_tenant / 1_000, within_tenant % 1_000)
        } else if within_tenant < 35_000 {
            let offset = within_tenant - 20_000;
            (20 + offset / 5_000, offset % 5_000)
        } else {
            (23, within_tenant - 35_000)
        };
        let bucket_ordinal = u64::from(tenant_id) * 24 + unix_hour;
        let id_input = (bucket_ordinal << 32) ^ within_bucket ^ self.id_seed;
        let low = avalanche(id_input);
        let high = avalanche(id_input ^ 0xd1b5_4a32_d192_ed03);
        let mut installation_id = [0_u8; 16];
        installation_id[..8].copy_from_slice(&low.to_le_bytes());
        installation_id[8..].copy_from_slice(&high.to_le_bytes());
        Some(Record::new(
            tenant_id,
            i64::try_from(unix_hour).expect("hour is at most 23"),
            installation_id,
        ))
    }

    pub fn shard(self, shard: usize) -> Result<ShardRecords, LabError> {
        if shard >= LOAD_SHARDS {
            return Err(LabError::InvalidRecordFile("shard index out of range"));
        }
        Ok(ShardRecords {
            corpus: self,
            shard,
            position: 0,
        })
    }
}

pub struct ShardRecords {
    corpus: LoadCorpus,
    shard: usize,
    position: u64,
}

impl Iterator for ShardRecords {
    type Item = Record;

    fn next(&mut self) -> Option<Self::Item> {
        if self.position >= LOAD_RECORDS_PER_SHARD {
            return None;
        }
        let offset = (self.shard as u64 * 104_729 + 17) % LOAD_RECORDS_PER_SHARD;
        let shuffled =
            (self.position.wrapping_mul(SHUFFLE_MULTIPLIER) + offset) % LOAD_RECORDS_PER_SHARD;
        let logical_index = shuffled * LOAD_SHARDS as u64 + self.shard as u64;
        self.position += 1;
        self.corpus.logical(logical_index)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = LOAD_RECORDS_PER_SHARD - self.position;
        let remaining = usize::try_from(remaining).expect("shard length fits usize");
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ShardRecords {}

const RECORD_HEADER_LEN: usize = 16;
const RECORD_LEN: usize = 28;

#[must_use]
pub fn encode_record_file(records: &[Record]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RECORD_HEADER_LEN + RECORD_LEN * records.len());
    bytes.extend_from_slice(b"RREC");
    bytes.push(1);
    bytes.extend_from_slice(&[0; 3]);
    bytes.extend_from_slice(&(records.len() as u64).to_le_bytes());
    for record in records {
        bytes.extend_from_slice(&record.tenant_id.to_le_bytes());
        bytes.extend_from_slice(&record.unix_hour.to_le_bytes());
        bytes.extend_from_slice(&record.installation_id);
    }
    bytes
}

pub fn stream_record_file<R, F>(
    mut reader: R,
    window: DayWindow,
    mut consume: F,
) -> Result<u64, LabError>
where
    R: Read,
    F: FnMut(Record) -> Result<(), LabError>,
{
    let mut header = [0_u8; RECORD_HEADER_LEN];
    read_fully(&mut reader, &mut header, "truncated header")?;
    if &header[..4] != b"RREC" {
        return Err(LabError::InvalidRecordFile("wrong magic"));
    }
    if header[4] != 1 {
        return Err(LabError::InvalidRecordFile("unknown version"));
    }
    if header[5..8] != [0; 3] {
        return Err(LabError::InvalidRecordFile("nonzero reserved bytes"));
    }
    let count = u64::from_le_bytes(header[8..16].try_into().expect("fixed slice"));
    let mut encoded = [0_u8; RECORD_LEN];
    for _ in 0..count {
        read_fully(&mut reader, &mut encoded, "truncated record")?;
        let tenant_id = u32::from_le_bytes(encoded[..4].try_into().expect("fixed slice"));
        let unix_hour = i64::from_le_bytes(encoded[4..12].try_into().expect("fixed slice"));
        let installation_id = encoded[12..].try_into().expect("fixed slice");
        let record = Record::new(tenant_id, unix_hour, installation_id);
        window.validate(record)?;
        consume(record)?;
    }
    let mut trailing = [0_u8; 1];
    match reader.read(&mut trailing) {
        Ok(0) => Ok(count),
        Ok(_) => Err(LabError::InvalidRecordFile("trailing bytes")),
        Err(_) => Err(LabError::InvalidRecordFile("read failure")),
    }
}

fn read_fully(
    reader: &mut impl Read,
    bytes: &mut [u8],
    reason: &'static str,
) -> Result<(), LabError> {
    reader
        .read_exact(bytes)
        .map_err(|_| LabError::InvalidRecordFile(reason))
}

#[derive(Debug)]
pub struct ExactRollup {
    window: DayWindow,
    buckets: BTreeMap<BucketKey, HashSet<[u8; 16]>>,
}

impl ExactRollup {
    #[must_use]
    pub fn new(window: DayWindow) -> Self {
        Self {
            window,
            buckets: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, record: Record) -> Result<(), LabError> {
        self.window.validate(record)?;
        self.buckets
            .entry(record.key())
            .or_default()
            .insert(record.installation_id);
        Ok(())
    }

    #[must_use]
    pub fn count(&self, key: BucketKey) -> Option<usize> {
        self.buckets.get(&key).map(HashSet::len)
    }

    #[must_use]
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    #[must_use]
    pub fn into_counts(self) -> BTreeMap<BucketKey, usize> {
        self.buckets
            .into_iter()
            .map(|(key, ids)| (key, ids.len()))
            .collect()
    }
}
