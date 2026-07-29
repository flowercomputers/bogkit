use std::cell::RefCell;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const FLASH_BYTES: usize = 128 * 1024;
pub const ERASE_BLOCK_BYTES: usize = 4 * 1024;
pub const ERASE_BLOCKS: usize = FLASH_BYTES / ERASE_BLOCK_BYTES;
pub const MIN_CONFIG_BYTES: usize = 2 * 1024;
pub const MAX_CONFIG_BYTES: usize = 24 * 1024;
pub const SCAN_BLOCK_LIMIT: usize = 32;
pub const JOURNAL_WORKING_MEMORY_CEILING_BYTES: usize = 1024;
const _: () = assert!(JOURNAL_WORKING_MEMORY_CEILING_BYTES <= 16 * 1024);

const MAGIC: [u8; 4] = *b"BJR1";
const FORMAT_VERSION: u16 = 1;
const HEADER_BYTES: usize = 32;
const COMMIT_OFFSET: usize = HEADER_BYTES;
const PAYLOAD_OFFSET: usize = COMMIT_OFFSET + 1;
const IO_CHUNK_BYTES: usize = 256;
const COMMITTED: u8 = 0x00;

#[derive(Debug)]
pub enum JournalError {
    Io(std::io::Error),
    InvalidFlashSize(u64),
    OutOfBounds,
    NorBitSetAttempt,
    InvalidConfigLength(usize),
    StaleRevision { current: u64, proposed: u64 },
    SourceLengthMismatch,
    Capacity,
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::InvalidFlashSize(size) => {
                write!(f, "flash image is {size} bytes; expected {FLASH_BYTES}")
            }
            Self::OutOfBounds => write!(f, "flash access is out of bounds"),
            Self::NorBitSetAttempt => write!(f, "NOR programming attempted a 0-to-1 change"),
            Self::InvalidConfigLength(size) => write!(
                f,
                "configuration is {size} bytes; expected {MIN_CONFIG_BYTES}..={MAX_CONFIG_BYTES}"
            ),
            Self::StaleRevision { current, proposed } => write!(
                f,
                "revision {proposed} is not newer than active revision {current}"
            ),
            Self::SourceLengthMismatch => {
                write!(
                    f,
                    "configuration source length does not match its declared length"
                )
            }
            Self::Capacity => write!(f, "journal record would overlap the active record"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<std::io::Error> for JournalError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub trait NorRead {
    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), JournalError>;
}

pub trait Nor: NorRead {
    fn program(&mut self, offset: usize, data: &[u8]) -> Result<(), JournalError>;
    fn erase_block(&mut self, block: usize) -> Result<(), JournalError>;
}

pub struct FileNor {
    file: RefCell<File>,
    erase_counts: [u32; ERASE_BLOCKS],
}

impl FileNor {
    pub fn create_fresh(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)?;
        let erased = [0xff; ERASE_BLOCK_BYTES];
        for _ in 0..ERASE_BLOCKS {
            file.write_all(&erased)?;
        }
        file.sync_all()?;
        Ok(Self {
            file: RefCell::new(file),
            erase_counts: [0; ERASE_BLOCKS],
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let size = file.metadata()?.len();
        if size != FLASH_BYTES as u64 {
            return Err(JournalError::InvalidFlashSize(size));
        }
        Ok(Self {
            file: RefCell::new(file),
            erase_counts: [0; ERASE_BLOCKS],
        })
    }

    pub fn erase_counts(&self) -> &[u32; ERASE_BLOCKS] {
        &self.erase_counts
    }
}

impl NorRead for FileNor {
    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), JournalError> {
        check_range(offset, output.len())?;
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start(offset as u64))?;
        file.read_exact(output)?;
        Ok(())
    }
}

impl Nor for FileNor {
    fn program(&mut self, offset: usize, data: &[u8]) -> Result<(), JournalError> {
        check_range(offset, data.len())?;
        let mut old = [0u8; IO_CHUNK_BYTES];
        let mut file = self.file.borrow_mut();

        for (chunk_index, chunk) in data.chunks(IO_CHUNK_BYTES).enumerate() {
            let chunk_offset = offset + chunk_index * IO_CHUNK_BYTES;
            file.seek(SeekFrom::Start(chunk_offset as u64))?;
            file.read_exact(&mut old[..chunk.len()])?;
            if old[..chunk.len()]
                .iter()
                .zip(chunk)
                .any(|(before, after)| before & after != *after)
            {
                return Err(JournalError::NorBitSetAttempt);
            }
        }

        for (chunk_index, chunk) in data.chunks(IO_CHUNK_BYTES).enumerate() {
            let chunk_offset = offset + chunk_index * IO_CHUNK_BYTES;
            file.seek(SeekFrom::Start(chunk_offset as u64))?;
            file.write_all(chunk)?;
        }
        file.sync_data()?;
        Ok(())
    }

    fn erase_block(&mut self, block: usize) -> Result<(), JournalError> {
        if block >= ERASE_BLOCKS {
            return Err(JournalError::OutOfBounds);
        }
        self.erase_counts[block] += 1;
        let erased = [0xff; IO_CHUNK_BYTES];
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start((block * ERASE_BLOCK_BYTES) as u64))?;
        for _ in 0..(ERASE_BLOCK_BYTES / IO_CHUNK_BYTES) {
            file.write_all(&erased)?;
        }
        file.sync_data()?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct MemoryNor {
    bytes: Vec<u8>,
    erase_counts: [u32; ERASE_BLOCKS],
}

impl MemoryNor {
    pub fn new() -> Self {
        Self {
            bytes: vec![0xff; FLASH_BYTES],
            erase_counts: [0; ERASE_BLOCKS],
        }
    }

    pub fn erase_counts(&self) -> &[u32; ERASE_BLOCKS] {
        &self.erase_counts
    }
}

impl Default for MemoryNor {
    fn default() -> Self {
        Self::new()
    }
}

impl NorRead for MemoryNor {
    fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), JournalError> {
        check_range(offset, output.len())?;
        output.copy_from_slice(&self.bytes[offset..offset + output.len()]);
        Ok(())
    }
}

impl Nor for MemoryNor {
    fn program(&mut self, offset: usize, data: &[u8]) -> Result<(), JournalError> {
        check_range(offset, data.len())?;
        for (before, after) in self.bytes[offset..offset + data.len()].iter_mut().zip(data) {
            if *before & after != *after {
                return Err(JournalError::NorBitSetAttempt);
            }
            *before = *after;
        }
        Ok(())
    }

    fn erase_block(&mut self, block: usize) -> Result<(), JournalError> {
        if block >= ERASE_BLOCKS {
            return Err(JournalError::OutOfBounds);
        }
        self.erase_counts[block] += 1;
        let start = block * ERASE_BLOCK_BYTES;
        self.bytes[start..start + ERASE_BLOCK_BYTES].fill(0xff);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigMeta {
    pub revision: u64,
    pub payload_len: usize,
    pub payload_crc32: u32,
    pub start_block: usize,
    pub span_blocks: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootResult {
    pub active: Option<ConfigMeta>,
    pub scanned_blocks: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Header {
    revision: u64,
    payload_len: usize,
    payload_crc32: u32,
    span_blocks: usize,
}

impl Header {
    fn encode(self) -> [u8; HEADER_BYTES] {
        let mut output = [0xff; HEADER_BYTES];
        output[0..4].copy_from_slice(&MAGIC);
        output[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        output[6..8].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        output[8..16].copy_from_slice(&self.revision.to_le_bytes());
        output[16..20].copy_from_slice(&(self.payload_len as u32).to_le_bytes());
        output[20..24].copy_from_slice(&self.payload_crc32.to_le_bytes());
        output[24..26].copy_from_slice(&(self.span_blocks as u16).to_le_bytes());
        output[26..28].copy_from_slice(&0u16.to_le_bytes());
        let checksum = crc32(&output[..28]);
        output[28..32].copy_from_slice(&checksum.to_le_bytes());
        output
    }

    fn decode(input: &[u8; HEADER_BYTES]) -> Option<Self> {
        if input[0..4] != MAGIC
            || u16::from_le_bytes(input[4..6].try_into().ok()?) != FORMAT_VERSION
            || u16::from_le_bytes(input[6..8].try_into().ok()?) != HEADER_BYTES as u16
            || u16::from_le_bytes(input[26..28].try_into().ok()?) != 0
            || u32::from_le_bytes(input[28..32].try_into().ok()?) != crc32(&input[..28])
        {
            return None;
        }

        let revision = u64::from_le_bytes(input[8..16].try_into().ok()?);
        let payload_len = u32::from_le_bytes(input[16..20].try_into().ok()?) as usize;
        let payload_crc32 = u32::from_le_bytes(input[20..24].try_into().ok()?);
        let span_blocks = u16::from_le_bytes(input[24..26].try_into().ok()?) as usize;

        if revision == 0
            || !(MIN_CONFIG_BYTES..=MAX_CONFIG_BYTES).contains(&payload_len)
            || span_blocks != blocks_for_payload(payload_len)
        {
            return None;
        }

        Some(Self {
            revision,
            payload_len,
            payload_crc32,
            span_blocks,
        })
    }
}

pub fn scan(flash: &impl NorRead) -> Result<BootResult, JournalError> {
    let mut active = None;

    for block in 0..ERASE_BLOCKS {
        if let Some(candidate) = read_candidate(flash, block)?
            && active
                .as_ref()
                .is_none_or(|current: &ConfigMeta| candidate.revision > current.revision)
        {
            active = Some(candidate);
        }
    }

    Ok(BootResult {
        active,
        scanned_blocks: ERASE_BLOCKS,
    })
}

pub fn write_config(
    flash: &mut impl Nor,
    revision: u64,
    payload_len: usize,
    source: &mut impl Read,
) -> Result<ConfigMeta, JournalError> {
    if !(MIN_CONFIG_BYTES..=MAX_CONFIG_BYTES).contains(&payload_len) {
        return Err(JournalError::InvalidConfigLength(payload_len));
    }

    let current = scan(flash)?.active;
    if let Some(active) = current
        && revision <= active.revision
    {
        return Err(JournalError::StaleRevision {
            current: active.revision,
            proposed: revision,
        });
    }
    if revision == 0 {
        return Err(JournalError::StaleRevision {
            current: current.map_or(0, |active| active.revision),
            proposed: revision,
        });
    }

    let span_blocks = blocks_for_payload(payload_len);
    let start_block = current
        .map(|active| (active.start_block + active.span_blocks) % ERASE_BLOCKS)
        .unwrap_or(0);
    if let Some(active) = current
        && block_runs_overlap(
            active.start_block,
            active.span_blocks,
            start_block,
            span_blocks,
        )
    {
        return Err(JournalError::Capacity);
    }

    for relative_block in 0..span_blocks {
        flash.erase_block((start_block + relative_block) % ERASE_BLOCKS)?;
    }

    let mut crc = Crc32::new();
    let mut remaining = payload_len;
    let mut written = 0;
    let mut buffer = [0u8; IO_CHUNK_BYTES];
    while remaining != 0 {
        let wanted = remaining.min(buffer.len());
        read_exact_source(source, &mut buffer[..wanted])?;
        crc.update(&buffer[..wanted]);
        program_wrapped(
            flash,
            start_block * ERASE_BLOCK_BYTES + PAYLOAD_OFFSET + written,
            &buffer[..wanted],
        )?;
        written += wanted;
        remaining -= wanted;
    }
    if source.read(&mut buffer[..1])? != 0 {
        return Err(JournalError::SourceLengthMismatch);
    }

    let payload_crc32 = crc.finish();
    let header = Header {
        revision,
        payload_len,
        payload_crc32,
        span_blocks,
    }
    .encode();
    flash.program(start_block * ERASE_BLOCK_BYTES, &header)?;
    flash.program(
        start_block * ERASE_BLOCK_BYTES + COMMIT_OFFSET,
        &[COMMITTED],
    )?;

    Ok(ConfigMeta {
        revision,
        payload_len,
        payload_crc32,
        start_block,
        span_blocks,
    })
}

pub fn verify_pattern(
    flash: &impl NorRead,
    config: ConfigMeta,
    seed: u8,
) -> Result<bool, JournalError> {
    let mut buffer = [0u8; IO_CHUNK_BYTES];
    let mut checked = 0;
    while checked < config.payload_len {
        let count = (config.payload_len - checked).min(buffer.len());
        read_wrapped(
            flash,
            config.start_block * ERASE_BLOCK_BYTES + PAYLOAD_OFFSET + checked,
            &mut buffer[..count],
        )?;
        if buffer[..count]
            .iter()
            .enumerate()
            .any(|(index, byte)| *byte != pattern_byte(seed, checked + index))
        {
            return Ok(false);
        }
        checked += count;
    }
    Ok(true)
}

pub fn corrupt_payload_byte(
    flash: &mut impl Nor,
    config: ConfigMeta,
    starting_at: usize,
) -> Result<usize, JournalError> {
    for relative in starting_at..config.payload_len {
        let address =
            (config.start_block * ERASE_BLOCK_BYTES + PAYLOAD_OFFSET + relative) % FLASH_BYTES;
        let mut before = [0u8; 1];
        flash.read(address, &mut before)?;
        if before[0] != 0 {
            flash.program(address, &[0])?;
            return Ok(relative);
        }
    }
    Err(JournalError::NorBitSetAttempt)
}

#[derive(Clone, Debug)]
pub struct PatternReader {
    seed: u8,
    length: usize,
    position: usize,
}

impl PatternReader {
    pub fn new(seed: u8, length: usize) -> Self {
        Self {
            seed,
            length,
            position: 0,
        }
    }
}

impl Read for PatternReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = (self.length - self.position).min(output.len());
        for (index, byte) in output[..count].iter_mut().enumerate() {
            *byte = pattern_byte(self.seed, self.position + index);
        }
        self.position += count;
        Ok(count)
    }
}

fn read_candidate(
    flash: &impl NorRead,
    start_block: usize,
) -> Result<Option<ConfigMeta>, JournalError> {
    let base = start_block * ERASE_BLOCK_BYTES;
    let mut commit = [0u8; 1];
    flash.read(base + COMMIT_OFFSET, &mut commit)?;
    if commit[0] != COMMITTED {
        return Ok(None);
    }

    let mut encoded = [0u8; HEADER_BYTES];
    flash.read(base, &mut encoded)?;
    let Some(header) = Header::decode(&encoded) else {
        return Ok(None);
    };

    let mut crc = Crc32::new();
    let mut buffer = [0u8; IO_CHUNK_BYTES];
    let mut checked = 0;
    while checked < header.payload_len {
        let count = (header.payload_len - checked).min(buffer.len());
        read_wrapped(flash, base + PAYLOAD_OFFSET + checked, &mut buffer[..count])?;
        crc.update(&buffer[..count]);
        checked += count;
    }
    if crc.finish() != header.payload_crc32 {
        return Ok(None);
    }

    Ok(Some(ConfigMeta {
        revision: header.revision,
        payload_len: header.payload_len,
        payload_crc32: header.payload_crc32,
        start_block,
        span_blocks: header.span_blocks,
    }))
}

fn read_wrapped(
    flash: &impl NorRead,
    offset: usize,
    output: &mut [u8],
) -> Result<(), JournalError> {
    let normalized = offset % FLASH_BYTES;
    let first = output.len().min(FLASH_BYTES - normalized);
    flash.read(normalized, &mut output[..first])?;
    if first < output.len() {
        flash.read(0, &mut output[first..])?;
    }
    Ok(())
}

fn program_wrapped(flash: &mut impl Nor, offset: usize, data: &[u8]) -> Result<(), JournalError> {
    let normalized = offset % FLASH_BYTES;
    let first = data.len().min(FLASH_BYTES - normalized);
    flash.program(normalized, &data[..first])?;
    if first < data.len() {
        flash.program(0, &data[first..])?;
    }
    Ok(())
}

fn blocks_for_payload(payload_len: usize) -> usize {
    (PAYLOAD_OFFSET + payload_len).div_ceil(ERASE_BLOCK_BYTES)
}

fn block_runs_overlap(a_start: usize, a_len: usize, b_start: usize, b_len: usize) -> bool {
    (0..a_len).any(|a| {
        let block = (a_start + a) % ERASE_BLOCKS;
        (0..b_len).any(|b| block == (b_start + b) % ERASE_BLOCKS)
    })
}

fn check_range(offset: usize, length: usize) -> Result<(), JournalError> {
    if offset
        .checked_add(length)
        .is_none_or(|end| end > FLASH_BYTES)
    {
        return Err(JournalError::OutOfBounds);
    }
    Ok(())
}

fn read_exact_source(source: &mut impl Read, output: &mut [u8]) -> Result<(), JournalError> {
    let mut filled = 0;
    while filled < output.len() {
        match source.read(&mut output[filled..])? {
            0 => return Err(JournalError::SourceLengthMismatch),
            count => filled += count,
        }
    }
    Ok(())
}

fn pattern_byte(seed: u8, index: usize) -> u8 {
    seed.wrapping_add((index as u8).wrapping_mul(37))
}

struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Self(0xffff_ffff)
    }

    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u32::from(*byte);
            for _ in 0..8 {
                self.0 = (self.0 >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(self.0 & 1)));
            }
        }
    }

    fn finish(self) -> u32 {
        !self.0
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(bytes);
    crc.finish()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    struct BoundaryAuditNor {
        inner: MemoryNor,
        enabled: bool,
        old_revision: u64,
        new_revision: u64,
        boundaries: usize,
        old_boundaries: usize,
        new_boundaries: usize,
        invalid_boundaries: usize,
        unexpected_boundaries: usize,
    }

    impl BoundaryAuditNor {
        fn new() -> Self {
            Self {
                inner: MemoryNor::new(),
                enabled: false,
                old_revision: 0,
                new_revision: 0,
                boundaries: 0,
                old_boundaries: 0,
                new_boundaries: 0,
                invalid_boundaries: 0,
                unexpected_boundaries: 0,
            }
        }

        fn audit(&mut self) {
            if !self.enabled {
                return;
            }
            self.boundaries += 1;
            match scan(self).and_then(|boot| boot.active.ok_or(JournalError::Capacity)) {
                Ok(config) if config.revision == self.old_revision => self.old_boundaries += 1,
                Ok(config) if config.revision == self.new_revision => self.new_boundaries += 1,
                Ok(_) => self.unexpected_boundaries += 1,
                Err(_) => self.invalid_boundaries += 1,
            }
        }
    }

    impl NorRead for BoundaryAuditNor {
        fn read(&self, offset: usize, output: &mut [u8]) -> Result<(), JournalError> {
            self.inner.read(offset, output)
        }
    }

    impl Nor for BoundaryAuditNor {
        fn program(&mut self, offset: usize, data: &[u8]) -> Result<(), JournalError> {
            check_range(offset, data.len())?;
            for (index, after) in data.iter().enumerate() {
                let before = &mut self.inner.bytes[offset + index];
                if *before & after != *after {
                    return Err(JournalError::NorBitSetAttempt);
                }
                *before = *after;
                self.audit();
            }
            Ok(())
        }

        fn erase_block(&mut self, block: usize) -> Result<(), JournalError> {
            if block >= ERASE_BLOCKS {
                return Err(JournalError::OutOfBounds);
            }
            self.inner.erase_counts[block] += 1;
            let start = block * ERASE_BLOCK_BYTES;
            for offset in start..start + ERASE_BLOCK_BYTES {
                self.inner.bytes[offset] = 0xff;
                self.audit();
            }
            Ok(())
        }
    }

    #[test]
    fn baseline_single_blob_has_a_boundary_with_no_valid_configuration() {
        let mut flash = MemoryNor::new();
        baseline_write(&mut flash, 1, MIN_CONFIG_BYTES, 7).unwrap();
        assert_eq!(scan(&flash).unwrap().active.unwrap().revision, 1);

        // The first byte of a torn erase destroys the only header before
        // any byte of the replacement exists.
        flash.bytes[0] = 0xff;
        assert!(scan(&flash).unwrap().active.is_none());
    }

    #[test]
    fn every_byte_boundary_boots_the_complete_old_or_new_configuration() {
        for new_length in [MIN_CONFIG_BYTES, MAX_CONFIG_BYTES] {
            let mut flash = BoundaryAuditNor::new();
            write_config(
                &mut flash,
                1,
                MIN_CONFIG_BYTES,
                &mut PatternReader::new(11, MIN_CONFIG_BYTES),
            )
            .unwrap();

            flash.enabled = true;
            flash.old_revision = 1;
            flash.new_revision = 2;
            let new_config = write_config(
                &mut flash,
                2,
                new_length,
                &mut PatternReader::new(29, new_length),
            )
            .unwrap();

            let expected_boundaries =
                new_config.span_blocks * ERASE_BLOCK_BYTES + new_length + HEADER_BYTES + 1;
            assert_eq!(flash.boundaries, expected_boundaries);
            assert_eq!(flash.invalid_boundaries, 0);
            assert_eq!(flash.unexpected_boundaries, 0);
            assert_eq!(flash.new_boundaries, 1);
            assert_eq!(
                flash.old_boundaries + flash.new_boundaries,
                flash.boundaries
            );
            assert!(verify_pattern(&flash, new_config, 29).unwrap());
            println!("audited {expected_boundaries} byte boundaries for {new_length}-byte update");
        }
    }

    #[test]
    fn corrupt_latest_and_stale_revisions_are_rejected() {
        let mut flash = MemoryNor::new();
        let old = write_config(
            &mut flash,
            41,
            MIN_CONFIG_BYTES,
            &mut PatternReader::new(3, MIN_CONFIG_BYTES),
        )
        .unwrap();
        let latest = write_config(
            &mut flash,
            42,
            MAX_CONFIG_BYTES,
            &mut PatternReader::new(5, MAX_CONFIG_BYTES),
        )
        .unwrap();
        corrupt_payload_byte(&mut flash, latest, 17).unwrap();

        let booted = scan(&flash).unwrap().active.unwrap();
        assert_eq!(booted, old);
        assert!(verify_pattern(&flash, booted, 3).unwrap());

        let error = write_config(
            &mut flash,
            40,
            MIN_CONFIG_BYTES,
            &mut PatternReader::new(8, MIN_CONFIG_BYTES),
        )
        .unwrap_err();
        assert!(matches!(error, JournalError::StaleRevision { .. }));
    }

    #[test]
    fn ten_thousand_updates_keep_erase_counts_within_ten_percent() {
        let mut flash = MemoryNor::new();
        for revision in 1..=10_000 {
            write_config(
                &mut flash,
                revision,
                MIN_CONFIG_BYTES,
                &mut PatternReader::new(revision as u8, MIN_CONFIG_BYTES),
            )
            .unwrap();
        }

        let minimum = *flash.erase_counts().iter().min().unwrap();
        let maximum = *flash.erase_counts().iter().max().unwrap();
        let average = flash
            .erase_counts()
            .iter()
            .map(|count| *count as f64)
            .sum::<f64>()
            / ERASE_BLOCKS as f64;
        let imbalance_percent = f64::from(maximum - minimum) / average * 100.0;

        assert!(imbalance_percent <= 10.0);
        assert_eq!((minimum, maximum), (312, 313));
        println!(
            "10,000 updates: erase count {minimum}..={maximum}, imbalance {imbalance_percent:.2}%"
        );
    }

    #[test]
    fn resource_bounds_and_boot_scan_are_bounded() {
        let mut flash = MemoryNor::new();
        let config = write_config(
            &mut flash,
            1,
            MAX_CONFIG_BYTES,
            &mut PatternReader::new(91, MAX_CONFIG_BYTES),
        )
        .unwrap();
        let started = Instant::now();
        let boot = scan(&flash).unwrap();
        let elapsed = started.elapsed();

        assert_eq!(flash.bytes.len(), FLASH_BYTES);
        assert_eq!(boot.scanned_blocks, SCAN_BLOCK_LIMIT);
        assert_eq!(boot.active, Some(config));
        assert!(elapsed < Duration::from_millis(50));
        println!(
            "maximum record boot scan: {} blocks in {elapsed:?}",
            boot.scanned_blocks
        );
    }

    fn baseline_write(
        flash: &mut MemoryNor,
        revision: u64,
        payload_len: usize,
        seed: u8,
    ) -> Result<(), JournalError> {
        let span_blocks = blocks_for_payload(payload_len);
        for block in 0..span_blocks {
            flash.erase_block(block)?;
        }

        let mut source = PatternReader::new(seed, payload_len);
        let mut buffer = [0u8; IO_CHUNK_BYTES];
        let mut crc = Crc32::new();
        let mut written = 0;
        while written < payload_len {
            let count = (payload_len - written).min(buffer.len());
            read_exact_source(&mut source, &mut buffer[..count])?;
            crc.update(&buffer[..count]);
            flash.program(PAYLOAD_OFFSET + written, &buffer[..count])?;
            written += count;
        }
        let header = Header {
            revision,
            payload_len,
            payload_crc32: crc.finish(),
            span_blocks,
        }
        .encode();
        flash.program(0, &header)?;
        flash.program(COMMIT_OFFSET, &[COMMITTED])
    }
}
