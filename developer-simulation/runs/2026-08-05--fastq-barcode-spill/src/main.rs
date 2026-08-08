use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

const BARCODE_LEN: usize = 10;
const BUFFER_LIMIT: usize = 64 * 1024;
const AMBIGUOUS_FILE: &str = "ambiguous.fastq";
const UNMATCHED_FILE: &str = "unmatched.fastq";
const MANIFEST_FILE: &str = "manifest.json";

fn main() {
    match Config::parse().and_then(run) {
        Ok(summary) => {
            println!(
                "processed {} read pairs: exact {}, corrected {}, ambiguous {}, unmatched {}; max open output writers {}",
                summary.total_pairs,
                summary.exact_pairs,
                summary.corrected_pairs,
                summary.ambiguous_pairs,
                summary.unmatched_pairs,
                summary.max_open_writers
            );
        }
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    }
}

#[derive(Debug)]
struct Config {
    barcode_path: PathBuf,
    output_dir: PathBuf,
    max_open: usize,
}

impl Config {
    fn parse() -> Result<Self, String> {
        let mut barcode_path = None;
        let mut output_dir = None;
        let mut max_open = 24_usize;
        let mut args = env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--barcodes" => {
                    barcode_path = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--barcodes requires a path".to_string())?,
                    ));
                }
                "--out" => {
                    output_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--out requires a directory".to_string())?,
                    ));
                }
                "--max-open" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--max-open requires a number".to_string())?;
                    max_open = value
                        .parse::<usize>()
                        .map_err(|_| "--max-open must be a positive integer".to_string())?;
                    if max_open == 0 || max_open > 24 {
                        return Err("--max-open must be between 1 and 24".to_string());
                    }
                }
                "-h" | "--help" => {
                    println!(
                        "usage: fastq-barcode-spill --barcodes MAP.tsv --out DIRECTORY [--max-open 24]\n\
                         reads interleaved paired-end FASTQ from standard input"
                    );
                    std::process::exit(0);
                }
                _ => return Err("unknown argument; use --help for usage".to_string()),
            }
        }

        Ok(Self {
            barcode_path: barcode_path
                .ok_or_else(|| "missing required --barcodes path".to_string())?,
            output_dir: output_dir.ok_or_else(|| "missing required --out directory".to_string())?,
            max_open,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Barcode([u8; BARCODE_LEN]);

#[derive(Debug)]
struct BarcodeMap {
    exact: HashMap<Barcode, usize>,
    corrections: HashMap<Barcode, Correction>,
    samples: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Correction {
    Unique(usize),
    Ambiguous,
}

impl BarcodeMap {
    fn load(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|_| "could not open barcode map".to_string())?;
        let reader = BufReader::new(file);
        let mut barcodes = Vec::new();
        let mut exact = HashMap::new();
        let mut samples = Vec::new();
        let mut sample_indexes = HashMap::new();
        let mut folded_sample_names = HashMap::new();

        for (offset, line_result) in reader.lines().enumerate() {
            let line_number = offset + 1;
            let line = line_result
                .map_err(|_| format!("barcode map line {line_number}: could not read line"))?;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split('\t');
            let barcode_text = fields.next().unwrap_or_default();
            let sample = fields.next().ok_or_else(|| {
                format!("barcode map line {line_number}: expected barcode and sample")
            })?;
            if fields.next().is_some() || barcode_text.is_empty() || sample.is_empty() {
                return Err(format!(
                    "barcode map line {line_number}: expected exactly two non-empty fields"
                ));
            }

            let barcode = parse_barcode(barcode_text.as_bytes())
                .map_err(|message| format!("barcode map line {line_number}: {message}"))?;
            validate_sample_name(sample)
                .map_err(|message| format!("barcode map line {line_number}: {message}"))?;
            if exact.contains_key(&barcode) {
                return Err(format!("barcode map line {line_number}: duplicate barcode"));
            }

            let folded_sample = sample.to_ascii_lowercase();
            if let Some(existing) = folded_sample_names.get(&folded_sample) {
                if existing != sample {
                    return Err(format!(
                        "barcode map line {line_number}: sample output name collides case-insensitively"
                    ));
                }
            } else {
                folded_sample_names.insert(folded_sample, sample.to_string());
            }

            let sample_index = match sample_indexes.get(sample) {
                Some(index) => *index,
                None => {
                    let index = samples.len();
                    samples.push(sample.to_string());
                    sample_indexes.insert(sample.to_string(), index);
                    index
                }
            };
            exact.insert(barcode, sample_index);
            barcodes.push((barcode, sample_index));
        }

        if barcodes.is_empty() {
            return Err("barcode map contains no barcodes".to_string());
        }

        let corrections = build_correction_index(&barcodes);
        Ok(Self {
            exact,
            corrections,
            samples,
        })
    }

    fn classify(&self, observed: &[u8]) -> Classification {
        let Ok(barcode) = parse_observed_barcode(observed) else {
            return Classification::Unmatched;
        };
        if let Some(sample_index) = self.exact.get(&barcode) {
            return Classification::Exact(*sample_index);
        }

        match self.corrections.get(&barcode) {
            Some(Correction::Unique(sample_index)) => Classification::Corrected(*sample_index),
            Some(Correction::Ambiguous) => Classification::Ambiguous,
            None => Classification::Unmatched,
        }
    }
}

fn build_correction_index(barcodes: &[(Barcode, usize)]) -> HashMap<Barcode, Correction> {
    let mut corrections = HashMap::with_capacity(barcodes.len() * BARCODE_LEN * 4);
    for (barcode, sample_index) in barcodes {
        for position in 0..BARCODE_LEN {
            for replacement in b"ACGTN" {
                if *replacement == barcode.0[position] {
                    continue;
                }
                let mut neighbor = barcode.0;
                neighbor[position] = *replacement;
                corrections
                    .entry(Barcode(neighbor))
                    .and_modify(|entry| *entry = Correction::Ambiguous)
                    .or_insert(Correction::Unique(*sample_index));
            }
        }
    }
    corrections
}

fn parse_barcode(bytes: &[u8]) -> Result<Barcode, String> {
    if bytes.len() != BARCODE_LEN {
        return Err(format!("barcode must contain exactly {BARCODE_LEN} bases"));
    }
    let mut barcode = [0; BARCODE_LEN];
    for (index, base) in bytes.iter().copied().enumerate() {
        let normalized = base.to_ascii_uppercase();
        if !matches!(normalized, b'A' | b'C' | b'G' | b'T') {
            return Err("barcode contains a non-ACGT base".to_string());
        }
        barcode[index] = normalized;
    }
    Ok(Barcode(barcode))
}

fn parse_observed_barcode(bytes: &[u8]) -> Result<Barcode, ()> {
    if bytes.len() < BARCODE_LEN {
        return Err(());
    }
    let mut barcode = [0; BARCODE_LEN];
    for (index, base) in bytes[..BARCODE_LEN].iter().copied().enumerate() {
        let normalized = base.to_ascii_uppercase();
        if !matches!(normalized, b'A' | b'C' | b'G' | b'T' | b'N') {
            return Err(());
        }
        barcode[index] = normalized;
    }
    Ok(Barcode(barcode))
}

fn validate_sample_name(sample: &str) -> Result<(), String> {
    if sample.len() > 120 {
        return Err("sample name is too long".to_string());
    }
    let folded = sample.to_ascii_lowercase();
    if sample == "."
        || sample == ".."
        || folded == "ambiguous"
        || folded == "unmatched"
        || !sample
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("sample name is not a safe output name".to_string());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Classification {
    Exact(usize),
    Corrected(usize),
    Ambiguous,
    Unmatched,
}

#[derive(Debug)]
struct FastqPair {
    lines: [Vec<u8>; 8],
}

impl FastqPair {
    fn write_to(&self, destination: &mut Vec<u8>) {
        for line in &self.lines {
            destination.extend_from_slice(line);
        }
    }

    fn read_one_barcode(&self) -> &[u8] {
        line_content(&self.lines[1])
    }
}

struct PairReader<R: BufRead> {
    input: R,
    line_number: usize,
}

impl<R: BufRead> PairReader<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            line_number: 0,
        }
    }

    fn next_pair(&mut self) -> Result<Option<FastqPair>, String> {
        let mut lines: [Vec<u8>; 8] = std::array::from_fn(|_| Vec::new());
        let first_line = self.line_number + 1;
        if !self.read_line(&mut lines[0])? {
            return Ok(None);
        }
        for line in &mut lines[1..] {
            if !self.read_line(line)? {
                return Err(format!(
                    "line {}: truncated interleaved FASTQ pair",
                    self.line_number + 1
                ));
            }
        }
        validate_record(&lines[0..4], first_line)?;
        validate_record(&lines[4..8], first_line + 4)?;
        if line_content(&lines[1]).len() < BARCODE_LEN {
            return Err(format!(
                "line {}: read 1 is shorter than the barcode length",
                first_line + 1
            ));
        }
        validate_pair_ids(&lines[0], &lines[4], first_line, first_line + 4)?;
        Ok(Some(FastqPair { lines }))
    }

    fn read_line(&mut self, destination: &mut Vec<u8>) -> Result<bool, String> {
        self.line_number += 1;
        match self.input.read_until(b'\n', destination) {
            Ok(0) => {
                self.line_number -= 1;
                Ok(false)
            }
            Ok(_) => Ok(true),
            Err(_) => Err(format!("line {}: could not read input", self.line_number)),
        }
    }
}

fn line_content(line: &[u8]) -> &[u8] {
    let without_lf = line.strip_suffix(b"\n").unwrap_or(line);
    without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
}

fn validate_record(lines: &[Vec<u8>], first_line: usize) -> Result<(), String> {
    let header = line_content(&lines[0]);
    let sequence = line_content(&lines[1]);
    let separator = line_content(&lines[2]);
    let quality = line_content(&lines[3]);

    if header.first() != Some(&b'@') || header.len() == 1 {
        return Err(format!(
            "line {first_line}: FASTQ header must start with @ and contain an identifier"
        ));
    }
    if sequence.is_empty() || sequence.iter().any(|byte| !byte.is_ascii_alphabetic()) {
        return Err(format!(
            "line {}: FASTQ sequence must contain only base letters",
            first_line + 1
        ));
    }
    if separator.first() != Some(&b'+') {
        return Err(format!(
            "line {}: FASTQ separator must start with +",
            first_line + 2
        ));
    }
    if !separator[1..].is_empty() {
        let repeated = first_token(&separator[1..]);
        if repeated != first_token(&header[1..]) {
            return Err(format!(
                "line {}: FASTQ separator identifier does not match header",
                first_line + 2
            ));
        }
    }
    if sequence.len() != quality.len() {
        return Err(format!(
            "line {}: sequence and quality lengths differ",
            first_line + 3
        ));
    }
    if quality.iter().any(|byte| !(33..=126).contains(byte)) {
        return Err(format!(
            "line {}: FASTQ quality contains a non-printable value",
            first_line + 3
        ));
    }
    Ok(())
}

fn first_token(value: &[u8]) -> &[u8] {
    let end = value
        .iter()
        .position(|byte| byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    &value[..end]
}

fn read_id(header_line: &[u8]) -> Result<(&[u8], Option<u8>), ()> {
    let header = line_content(header_line);
    let without_at = &header[1..];
    let token = first_token(without_at);
    if token.is_empty() || token.iter().any(|byte| !(33..=126).contains(byte)) {
        return Err(());
    }
    let (core, slash_role) = if token.ends_with(b"/1") {
        (&token[..token.len() - 2], Some(1))
    } else if token.ends_with(b"/2") {
        (&token[..token.len() - 2], Some(2))
    } else {
        (token, None)
    };
    if core.is_empty() {
        return Err(());
    }

    let rest = &without_at[token.len()..];
    let second = first_token(rest.trim_ascii_start());
    let role = match second.first() {
        Some(b'1') if second.get(1) == Some(&b':') => Some(1),
        Some(b'2') if second.get(1) == Some(&b':') => Some(2),
        _ => None,
    };
    if slash_role.is_some() && role.is_some() && slash_role != role {
        return Err(());
    }
    Ok((core, slash_role.or(role)))
}

fn validate_pair_ids(
    read_one_header: &[u8],
    read_two_header: &[u8],
    read_one_line: usize,
    read_two_line: usize,
) -> Result<(), String> {
    let (read_one_id, read_one_role) = read_id(read_one_header)
        .map_err(|_| format!("line {read_one_line}: unsupported FASTQ identifier"))?;
    let (read_two_id, read_two_role) = read_id(read_two_header)
        .map_err(|_| format!("line {read_two_line}: unsupported FASTQ identifier"))?;
    if read_one_role.is_some_and(|role| role != 1) {
        return Err(format!("line {read_one_line}: first record is not read 1"));
    }
    if read_two_role.is_some_and(|role| role != 2) {
        return Err(format!("line {read_two_line}: second record is not read 2"));
    }
    if read_one_id != read_two_id {
        return Err(format!(
            "line {read_two_line}: paired read identifiers do not match"
        ));
    }
    Ok(())
}

struct OpenWriter {
    writer: BufWriter<File>,
    last_used: u64,
}

struct WriterPool {
    paths: Vec<PathBuf>,
    open: BTreeMap<usize, OpenWriter>,
    capacity: usize,
    clock: u64,
    max_observed: usize,
    open_events: u64,
}

impl WriterPool {
    fn new(paths: Vec<PathBuf>, capacity: usize) -> Self {
        Self {
            paths,
            open: BTreeMap::new(),
            capacity,
            clock: 0,
            max_observed: 0,
            open_events: 0,
        }
    }

    fn write(&mut self, destination: usize, bytes: &[u8]) -> Result<(), String> {
        self.clock += 1;
        if !self.open.contains_key(&destination) {
            self.open_writer(destination)?;
        }
        let open_writer = self
            .open
            .get_mut(&destination)
            .ok_or_else(|| "internal writer-pool error".to_string())?;
        open_writer.last_used = self.clock;
        open_writer
            .writer
            .write_all(bytes)
            .map_err(|_| "could not write an output file".to_string())
    }

    fn open_writer(&mut self, destination: usize) -> Result<(), String> {
        if self.open.len() == self.capacity {
            let evict = self
                .open
                .iter()
                .min_by_key(|(_, writer)| writer.last_used)
                .map(|(index, _)| *index)
                .ok_or_else(|| "internal writer-pool error".to_string())?;
            let mut writer = self
                .open
                .remove(&evict)
                .ok_or_else(|| "internal writer-pool error".to_string())?;
            writer
                .writer
                .flush()
                .map_err(|_| "could not flush an output file".to_string())?;
        }

        let file = OpenOptions::new()
            .append(true)
            .open(&self.paths[destination])
            .map_err(|_| "could not open an output file".to_string())?;
        self.open.insert(
            destination,
            OpenWriter {
                writer: BufWriter::new(file),
                last_used: self.clock,
            },
        );
        self.open_events += 1;
        self.max_observed = self.max_observed.max(self.open.len());
        Ok(())
    }

    fn finish(mut self) -> Result<(usize, u64), String> {
        for writer in self.open.values_mut() {
            writer
                .writer
                .flush()
                .map_err(|_| "could not flush an output file".to_string())?;
        }
        Ok((self.max_observed, self.open_events))
    }
}

#[derive(Debug)]
struct Summary {
    total_pairs: u64,
    exact_pairs: u64,
    corrected_pairs: u64,
    ambiguous_pairs: u64,
    unmatched_pairs: u64,
    sample_pairs: Vec<u64>,
    max_open_writers: usize,
    open_events: u64,
}

fn run(config: Config) -> Result<Summary, String> {
    let barcode_map = BarcodeMap::load(&config.barcode_path)?;
    let paths = prepare_output_directory(&config.output_dir, &barcode_map.samples)?;
    let ambiguous_index = barcode_map.samples.len();
    let unmatched_index = ambiguous_index + 1;
    let mut buffers: Vec<Vec<u8>> = (0..paths.len()).map(|_| Vec::new()).collect();
    let mut writers = WriterPool::new(paths, config.max_open);
    let stdin = io::stdin();
    let mut pairs = PairReader::new(stdin.lock());
    let mut summary = Summary {
        total_pairs: 0,
        exact_pairs: 0,
        corrected_pairs: 0,
        ambiguous_pairs: 0,
        unmatched_pairs: 0,
        sample_pairs: vec![0; barcode_map.samples.len()],
        max_open_writers: 0,
        open_events: 0,
    };

    while let Some(pair) = pairs.next_pair()? {
        let destination = match barcode_map.classify(pair.read_one_barcode()) {
            Classification::Exact(sample_index) => {
                summary.exact_pairs += 1;
                summary.sample_pairs[sample_index] += 1;
                sample_index
            }
            Classification::Corrected(sample_index) => {
                summary.corrected_pairs += 1;
                summary.sample_pairs[sample_index] += 1;
                sample_index
            }
            Classification::Ambiguous => {
                summary.ambiguous_pairs += 1;
                ambiguous_index
            }
            Classification::Unmatched => {
                summary.unmatched_pairs += 1;
                unmatched_index
            }
        };
        summary.total_pairs += 1;
        pair.write_to(&mut buffers[destination]);
        if buffers[destination].len() >= BUFFER_LIMIT {
            writers.write(destination, &buffers[destination])?;
            buffers[destination].clear();
        }
    }

    for (destination, buffer) in buffers.iter().enumerate() {
        if !buffer.is_empty() {
            writers.write(destination, buffer)?;
        }
    }
    let (max_observed, open_events) = writers.finish()?;
    summary.max_open_writers = max_observed;
    summary.open_events = open_events;

    let classified = summary.exact_pairs
        + summary.corrected_pairs
        + summary.ambiguous_pairs
        + summary.unmatched_pairs;
    if classified != summary.total_pairs {
        return Err("internal count validation failed".to_string());
    }
    if summary.sample_pairs.iter().sum::<u64>() != summary.exact_pairs + summary.corrected_pairs {
        return Err("internal sample-count validation failed".to_string());
    }

    write_manifest(&config.output_dir, &barcode_map.samples, &summary)?;
    Ok(summary)
}

fn prepare_output_directory(output_dir: &Path, samples: &[String]) -> Result<Vec<PathBuf>, String> {
    if output_dir.exists() {
        if !output_dir.is_dir() {
            return Err("output path is not a directory".to_string());
        }
        if output_dir
            .read_dir()
            .map_err(|_| "could not inspect output directory".to_string())?
            .next()
            .is_some()
        {
            return Err("output directory must be empty".to_string());
        }
    } else {
        fs::create_dir_all(output_dir)
            .map_err(|_| "could not create output directory".to_string())?;
    }

    let mut paths: Vec<PathBuf> = samples
        .iter()
        .map(|sample| output_dir.join(format!("{sample}.fastq")))
        .collect();
    paths.push(output_dir.join(AMBIGUOUS_FILE));
    paths.push(output_dir.join(UNMATCHED_FILE));
    for path in &paths {
        File::create(path).map_err(|_| "could not create an output file".to_string())?;
    }
    Ok(paths)
}

fn write_manifest(output_dir: &Path, samples: &[String], summary: &Summary) -> Result<(), String> {
    let temporary = output_dir.join(".manifest.json.tmp");
    let final_path = output_dir.join(MANIFEST_FILE);
    let file = File::create(&temporary).map_err(|_| "could not create manifest".to_string())?;
    let mut writer = BufWriter::new(file);
    writeln!(writer, "{{").map_err(|_| "could not write manifest".to_string())?;
    writeln!(writer, "  \"complete\": true,")
        .map_err(|_| "could not write manifest".to_string())?;
    writeln!(writer, "  \"total_pairs\": {},", summary.total_pairs)
        .map_err(|_| "could not write manifest".to_string())?;
    writeln!(writer, "  \"exact_pairs\": {},", summary.exact_pairs)
        .map_err(|_| "could not write manifest".to_string())?;
    writeln!(
        writer,
        "  \"corrected_pairs\": {},",
        summary.corrected_pairs
    )
    .map_err(|_| "could not write manifest".to_string())?;
    writeln!(
        writer,
        "  \"ambiguous_pairs\": {},",
        summary.ambiguous_pairs
    )
    .map_err(|_| "could not write manifest".to_string())?;
    writeln!(
        writer,
        "  \"unmatched_pairs\": {},",
        summary.unmatched_pairs
    )
    .map_err(|_| "could not write manifest".to_string())?;
    writeln!(
        writer,
        "  \"max_open_writers\": {},",
        summary.max_open_writers
    )
    .map_err(|_| "could not write manifest".to_string())?;
    writeln!(writer, "  \"open_events\": {},", summary.open_events)
        .map_err(|_| "could not write manifest".to_string())?;
    writeln!(writer, "  \"samples\": [").map_err(|_| "could not write manifest".to_string())?;
    for (index, (sample, count)) in samples.iter().zip(summary.sample_pairs.iter()).enumerate() {
        let comma = if index + 1 == samples.len() { "" } else { "," };
        writeln!(
            writer,
            "    {{\"sample\": \"{sample}\", \"file\": \"{sample}.fastq\", \"pairs\": {count}}}{comma}"
        )
        .map_err(|_| "could not write manifest".to_string())?;
    }
    writeln!(writer, "  ]").map_err(|_| "could not write manifest".to_string())?;
    writeln!(writer, "}}").map_err(|_| "could not write manifest".to_string())?;
    writer
        .flush()
        .map_err(|_| "could not flush manifest".to_string())?;
    let file = writer
        .into_inner()
        .map_err(|_| "could not finish manifest".to_string())?;
    file.sync_all()
        .map_err(|_| "could not sync manifest".to_string())?;
    fs::rename(&temporary, &final_path)
        .map_err(|_| "could not publish completion manifest".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn map() -> BarcodeMap {
        let barcodes = [("AAAAAAAAAA", 0), ("CAAAAAAAAA", 1), ("CCCCCCCCCC", 2)];
        let parsed = barcodes
            .iter()
            .map(|(barcode, sample_index)| {
                (parse_barcode(barcode.as_bytes()).unwrap(), *sample_index)
            })
            .collect::<Vec<_>>();
        let exact = parsed.iter().copied().collect();
        BarcodeMap {
            exact,
            corrections: build_correction_index(&parsed),
            samples: vec!["a".into(), "b".into(), "c".into()],
        }
    }

    #[test]
    fn exact_unique_one_error_and_tie_are_distinct() {
        let map = map();
        assert_eq!(map.classify(b"AAAAAAAAAATTT"), Classification::Exact(0));
        assert_eq!(map.classify(b"TCCCCCCCCCTTT"), Classification::Corrected(2));
        assert_eq!(map.classify(b"GAAAAAAAAATTT"), Classification::Ambiguous);
        assert_eq!(map.classify(b"TTTTTTTTTTTTT"), Classification::Unmatched);
    }

    #[test]
    fn same_sample_barcode_tie_remains_ambiguous() {
        let parsed = [
            (parse_barcode(b"AAAAAAAAAA").unwrap(), 0),
            (parse_barcode(b"CAAAAAAAAA").unwrap(), 0),
        ];
        let map = BarcodeMap {
            exact: parsed.iter().copied().collect(),
            corrections: build_correction_index(&parsed),
            samples: vec!["sample".into()],
        };
        assert_eq!(map.classify(b"GAAAAAAAAA"), Classification::Ambiguous);
    }

    #[test]
    fn pair_reader_preserves_original_bytes() {
        let input = b"@x/1\r\nAAAAAAAAAATG\r\n+\r\nIIIIIIIIIIII\r\n@x/2\r\nACGT\r\n+\r\nIIII\r\n";
        let mut reader = PairReader::new(Cursor::new(input));
        let pair = reader.next_pair().unwrap().unwrap();
        let mut output = Vec::new();
        pair.write_to(&mut output);
        assert_eq!(output, input);
        assert!(reader.next_pair().unwrap().is_none());
    }

    #[test]
    fn identifier_validation_rejects_empty_control_and_conflicting_roles() {
        let cases: [&[u8]; 3] = [
            b"@ /1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@ /2\nACGT\n+\nIIII\n",
            b"@bad\0/1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@bad\0/2\nACGT\n+\nIIII\n",
            b"@x/1 2:N:0:1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@x/2 2:N:0:1\nACGT\n+\nIIII\n",
        ];
        for input in cases {
            assert!(PairReader::new(Cursor::new(input)).next_pair().is_err());
        }

        let casava = b"@machine:1:flow:2:3:4:5 1:N:0:ACGT\nAAAAAAAAAA\n+\nIIIIIIIIII\n@machine:1:flow:2:3:4:5 2:N:0:ACGT\nACGT\n+\nIIII\n";
        assert!(
            PairReader::new(Cursor::new(casava))
                .next_pair()
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn malformed_inputs_report_expected_line() {
        let truncated = b"@x/1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@x/2\nACGT\n+\n";
        let mut reader = PairReader::new(Cursor::new(truncated));
        assert_eq!(
            reader.next_pair().unwrap_err(),
            "line 8: truncated interleaved FASTQ pair"
        );

        let mismatch = b"@x/1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@y/2\nACGT\n+\nIIII\n";
        let mut reader = PairReader::new(Cursor::new(mismatch));
        assert_eq!(
            reader.next_pair().unwrap_err(),
            "line 5: paired read identifiers do not match"
        );

        let first = b"@ok/1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@ok/2\nACGT\n+\nIIII\n";
        let second = b"@left/1\nAAAAAAAAAA\n+\nIIIIIIIIII\n@right/2\nACGT\n+\nIIII\n";
        let joined = [first.as_slice(), second.as_slice()].concat();
        let mut reader = PairReader::new(Cursor::new(joined));
        assert!(reader.next_pair().unwrap().is_some());
        assert_eq!(
            reader.next_pair().unwrap_err(),
            "line 13: paired read identifiers do not match"
        );
    }
}
