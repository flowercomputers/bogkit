use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

const MAX_EDIT_DISTANCE: usize = 256;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageInput {
    page_id: String,
    old_text: String,
    revised_text: String,
    glyphs: Vec<Glyph>,
    spans: Vec<ReviewedSpan>,
}

/// Compact JSON form: [start_byte, end_byte, line, x, y, width, height].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Glyph(usize, usize, u32, i32, i32, i32, i32);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ReviewedSpan {
    start: usize,
    end: usize,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PageOutput {
    page_id: String,
    status: String,
    rectangles: Vec<Rect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PageAudit {
    page_id: String,
    status: String,
    reviewed_spans: usize,
    exact_spans: usize,
    conservative_spans: usize,
    removed_spans: usize,
    rectangle_count: usize,
    reason_codes: Vec<String>,
    decisions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Checkpoint {
    completed_pages: usize,
    output_len: u64,
    audit_len: u64,
    input_prefix_hash: u64,
    output_prefix_hash: u64,
    audit_prefix_hash: u64,
    blocked_pages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompletionMarker {
    version: u32,
    input_path: String,
    output_path: String,
    audit_path: String,
    input_len: u64,
    output_len: u64,
    audit_len: u64,
    input_sha256: String,
    output_sha256: String,
    audit_sha256: String,
    completed_pages: usize,
    blocked_pages: usize,
}

#[derive(Debug)]
struct SafeError {
    code: &'static str,
    line: Option<usize>,
}

impl SafeError {
    const fn new(code: &'static str) -> Self {
        Self { code, line: None }
    }

    const fn at_line(code: &'static str, line: usize) -> Self {
        Self {
            code,
            line: Some(line),
        }
    }

    fn print(&self) {
        match self.line {
            Some(line) => eprintln!("error code={} line={line}", self.code),
            None => eprintln!("error code={}", self.code),
        }
    }
}

type SafeResult<T> = Result<T, SafeError>;

#[derive(Debug)]
struct RemapArgs {
    input: PathBuf,
    output: PathBuf,
    audit: PathBuf,
    resume: bool,
    stop_after: Option<usize>,
}

#[derive(Debug, Serialize)]
struct RunSummary {
    pages: usize,
    blocked_pages: usize,
    resumed_from: usize,
    interrupted: bool,
}

pub fn entry(args: Vec<String>) -> ExitCode {
    let result = dispatch(&args);
    match result {
        Ok(CommandResult::Complete(summary)) => {
            let _ = serde_json::to_writer(std::io::stdout().lock(), &summary);
            println!();
            if summary.blocked_pages == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
        }
        Ok(CommandResult::Interrupted(summary)) => {
            let _ = serde_json::to_writer(std::io::stdout().lock(), &summary);
            println!();
            ExitCode::from(75)
        }
        Ok(CommandResult::Generated(summary)) | Ok(CommandResult::Checked(summary)) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            error.print();
            ExitCode::from(2)
        }
    }
}

#[derive(Debug)]
enum CommandResult {
    Complete(RunSummary),
    Interrupted(RunSummary),
    Generated(String),
    Checked(String),
}

fn dispatch(args: &[String]) -> SafeResult<CommandResult> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(SafeError::new("usage"));
    };
    match command {
        "remap" => {
            let parsed = parse_remap_args(&args[1..])?;
            run_remap(&parsed)
        }
        "generate" => {
            let dir = required_path_arg(&args[1..], "--dir")?;
            let pages = optional_usize_arg(&args[1..], "--pages")?.unwrap_or(240);
            generate_suite(&dir, pages)?;
            Ok(CommandResult::Generated(format!(
                "generated fixture_pages={} hand_pages=4",
                pages
            )))
        }
        "generate-workload" => {
            let output = required_path_arg(&args[1..], "--output")?;
            let pages = optional_usize_arg(&args[1..], "--pages")?.unwrap_or(5_000);
            let scalars = optional_usize_arg(&args[1..], "--scalars-per-page")?.unwrap_or(4_000);
            let spans = optional_usize_arg(&args[1..], "--spans-per-page")?.unwrap_or(30);
            generate_workload(&output, pages, scalars, spans)?;
            Ok(CommandResult::Generated(format!(
                "generated workload_pages={pages} scalars={} spans={}",
                pages.saturating_mul(scalars),
                pages.saturating_mul(spans)
            )))
        }
        "check" => {
            let input = required_path_arg(&args[1..], "--input")?;
            let output = required_path_arg(&args[1..], "--output")?;
            let audit = required_path_arg(&args[1..], "--audit")?;
            let expected = required_path_arg(&args[1..], "--expected")?;
            let sentinels = required_path_arg(&args[1..], "--sentinels")?;
            let diagnostics = optional_path_arg(&args[1..], "--diagnostics");
            let summary = check_suite(
                &input,
                &output,
                &audit,
                &expected,
                &sentinels,
                diagnostics.as_deref(),
            )?;
            Ok(CommandResult::Checked(summary))
        }
        _ => Err(SafeError::new("usage")),
    }
}

fn parse_remap_args(args: &[String]) -> SafeResult<RemapArgs> {
    Ok(RemapArgs {
        input: required_path_arg(args, "--input")?,
        output: required_path_arg(args, "--output")?,
        audit: required_path_arg(args, "--audit")?,
        resume: args.iter().any(|arg| arg == "--resume"),
        stop_after: optional_usize_arg(args, "--stop-after")?,
    })
}

fn required_path_arg(args: &[String], name: &str) -> SafeResult<PathBuf> {
    optional_path_arg(args, name).ok_or_else(|| SafeError::new("usage"))
}

fn optional_path_arg(args: &[String], name: &str) -> Option<PathBuf> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| PathBuf::from(&pair[1]))
}

fn optional_usize_arg(args: &[String], name: &str) -> SafeResult<Option<usize>> {
    let Some(raw) = args
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
    else {
        return Ok(None);
    };
    raw.parse::<usize>()
        .map(Some)
        .map_err(|_| SafeError::new("usage"))
}

fn run_remap(args: &RemapArgs) -> SafeResult<CommandResult> {
    if args.output == args.audit || args.input == args.output || args.input == args.audit {
        return Err(SafeError::new("path_conflict"));
    }
    let output_partial = suffixed(&args.output, ".partial");
    let audit_partial = suffixed(&args.audit, ".partial");
    let checkpoint_path = suffixed(&args.output, ".checkpoint");
    let complete_path = suffixed(&args.output, ".complete");

    if args.resume && complete_path.exists() {
        let marker: CompletionMarker = read_json_file(&complete_path, "invalid_completion_marker")?;
        verify_completion_marker(args, &marker)?;
        return Ok(CommandResult::Complete(RunSummary {
            pages: marker.completed_pages,
            blocked_pages: marker.blocked_pages,
            resumed_from: marker.completed_pages,
            interrupted: false,
        }));
    }

    let (mut checkpoint, resumed_from) = if args.resume && checkpoint_path.exists() {
        let checkpoint: Checkpoint = read_json_file(&checkpoint_path, "invalid_checkpoint")?;
        (checkpoint.clone(), checkpoint.completed_pages)
    } else {
        remove_if_exists(&complete_path)?;
        remove_if_exists(&checkpoint_path)?;
        remove_if_exists(&output_partial)?;
        remove_if_exists(&audit_partial)?;
        Checkpoint {
            completed_pages: 0,
            output_len: 0,
            audit_len: 0,
            input_prefix_hash: FNV_OFFSET,
            output_prefix_hash: FNV_OFFSET,
            audit_prefix_hash: FNV_OFFSET,
            blocked_pages: 0,
        }
        .pipe(|checkpoint| (checkpoint, 0))
    };

    let input_file = File::open(&args.input).map_err(|_| SafeError::new("input_open"))?;
    let mut reader = BufReader::new(input_file);
    if resumed_from > 0 {
        restore_partial_for_resume(&output_partial, &args.output)?;
        restore_partial_for_resume(&audit_partial, &args.audit)?;
        validate_partial(
            &output_partial,
            checkpoint.output_len,
            checkpoint.output_prefix_hash,
        )?;
        validate_partial(
            &audit_partial,
            checkpoint.audit_len,
            checkpoint.audit_prefix_hash,
        )?;
    }
    let output_file = open_partial(&output_partial, resumed_from > 0, checkpoint.output_len)?;
    let audit_file = open_partial(&audit_partial, resumed_from > 0, checkpoint.audit_len)?;

    let mut raw = Vec::new();
    let mut prefix_hash = FNV_OFFSET;
    for line_index in 1..=checkpoint.completed_pages {
        raw.clear();
        if reader
            .read_until(b'\n', &mut raw)
            .map_err(|_| SafeError::at_line("input_read", line_index))?
            == 0
        {
            return Err(SafeError::new("resume_input_shorter"));
        }
        prefix_hash = fnv_update(prefix_hash, &raw);
    }
    if prefix_hash != checkpoint.input_prefix_hash {
        return Err(SafeError::new("resume_input_changed"));
    }

    let mut output_writer = BufWriter::new(output_file);
    let mut audit_writer = BufWriter::new(audit_file);
    let mut line_index = checkpoint.completed_pages;
    loop {
        raw.clear();
        let bytes = reader
            .read_until(b'\n', &mut raw)
            .map_err(|_| SafeError::at_line("input_read", line_index + 1))?;
        if bytes == 0 {
            break;
        }
        line_index += 1;
        let content = raw.strip_suffix(b"\n").unwrap_or(&raw);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if content.is_empty() {
            return Err(SafeError::at_line("blank_input_line", line_index));
        }
        std::str::from_utf8(content).map_err(|_| SafeError::at_line("invalid_utf8", line_index))?;
        let page: PageInput = serde_json::from_slice(content)
            .map_err(|_| SafeError::at_line("invalid_json", line_index))?;
        let (output, audit) = process_page(page, line_index);
        let mut output_record =
            serde_json::to_vec(&output).map_err(|_| SafeError::new("output_write"))?;
        output_record.push(b'\n');
        output_writer
            .write_all(&output_record)
            .map_err(|_| SafeError::new("output_write"))?;
        let mut audit_record =
            serde_json::to_vec(&audit).map_err(|_| SafeError::new("audit_write"))?;
        audit_record.push(b'\n');
        audit_writer
            .write_all(&audit_record)
            .map_err(|_| SafeError::new("audit_write"))?;
        output_writer
            .flush()
            .map_err(|_| SafeError::new("output_write"))?;
        audit_writer
            .flush()
            .map_err(|_| SafeError::new("audit_write"))?;

        checkpoint.completed_pages += 1;
        checkpoint.output_len = output_writer
            .stream_position()
            .map_err(|_| SafeError::new("output_write"))?;
        checkpoint.audit_len = audit_writer
            .stream_position()
            .map_err(|_| SafeError::new("audit_write"))?;
        checkpoint.input_prefix_hash = fnv_update(checkpoint.input_prefix_hash, &raw);
        checkpoint.output_prefix_hash = fnv_update(checkpoint.output_prefix_hash, &output_record);
        checkpoint.audit_prefix_hash = fnv_update(checkpoint.audit_prefix_hash, &audit_record);
        if output.status == "blocked" {
            checkpoint.blocked_pages += 1;
        }
        write_checkpoint(&checkpoint_path, &checkpoint)?;

        if args.stop_after == Some(checkpoint.completed_pages) {
            return Ok(CommandResult::Interrupted(RunSummary {
                pages: checkpoint.completed_pages,
                blocked_pages: checkpoint.blocked_pages,
                resumed_from,
                interrupted: true,
            }));
        }
    }

    output_writer
        .flush()
        .map_err(|_| SafeError::new("output_write"))?;
    audit_writer
        .flush()
        .map_err(|_| SafeError::new("audit_write"))?;
    output_writer
        .get_ref()
        .sync_all()
        .map_err(|_| SafeError::new("output_write"))?;
    audit_writer
        .get_ref()
        .sync_all()
        .map_err(|_| SafeError::new("audit_write"))?;
    drop(output_writer);
    drop(audit_writer);
    replace_file(&output_partial, &args.output)?;
    replace_file(&audit_partial, &args.audit)?;
    let marker = completion_marker(args, &checkpoint)?;
    write_checkpoint(&complete_path, &marker)?;
    remove_if_exists(&checkpoint_path)?;

    Ok(CommandResult::Complete(RunSummary {
        pages: checkpoint.completed_pages,
        blocked_pages: checkpoint.blocked_pages,
        resumed_from,
        interrupted: false,
    }))
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn open_partial(path: &Path, resume: bool, len: u64) -> SafeResult<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if resume {
        let mut file = options
            .open(path)
            .map_err(|_| SafeError::new("partial_open"))?;
        let actual_len = file
            .metadata()
            .map_err(|_| SafeError::new("partial_open"))?
            .len();
        if actual_len < len {
            return Err(SafeError::new("partial_short"));
        }
        file.set_len(len)
            .map_err(|_| SafeError::new("partial_write"))?;
        file.seek(SeekFrom::Start(len))
            .map_err(|_| SafeError::new("partial_write"))?;
        Ok(file)
    } else {
        options
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|_| SafeError::new("partial_open"))
    }
}

fn restore_partial_for_resume(partial: &Path, final_path: &Path) -> SafeResult<()> {
    if partial.exists() {
        return Ok(());
    }
    if final_path.exists() {
        fs::rename(final_path, partial).map_err(|_| SafeError::new("resume_partial_restore"))
    } else {
        Err(SafeError::new("resume_partial_missing"))
    }
}

fn fnv_update(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn validate_partial(path: &Path, expected_len: u64, expected_hash: u64) -> SafeResult<()> {
    let file = File::open(path).map_err(|_| SafeError::new("partial_open"))?;
    let actual_len = file
        .metadata()
        .map_err(|_| SafeError::new("partial_open"))?
        .len();
    if actual_len < expected_len {
        return Err(SafeError::new("partial_short"));
    }
    let mut reader = file.take(expected_len);
    let mut buffer = [0_u8; 8192];
    let mut hash = FNV_OFFSET;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|_| SafeError::new("partial_read"))?;
        if read == 0 {
            break;
        }
        hash = fnv_update(hash, &buffer[..read]);
    }
    if hash != expected_hash {
        return Err(SafeError::new("partial_changed"));
    }
    Ok(())
}

fn path_identity(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn file_len(path: &Path) -> SafeResult<u64> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|_| SafeError::new("completed_state_mismatch"))
}

fn sha256_file(path: &Path) -> SafeResult<String> {
    let mut file = File::open(path).map_err(|_| SafeError::new("completed_state_mismatch"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| SafeError::new("completed_state_mismatch"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn completion_marker(args: &RemapArgs, checkpoint: &Checkpoint) -> SafeResult<CompletionMarker> {
    Ok(CompletionMarker {
        version: 1,
        input_path: path_identity(&args.input),
        output_path: path_identity(&args.output),
        audit_path: path_identity(&args.audit),
        input_len: file_len(&args.input)?,
        output_len: file_len(&args.output)?,
        audit_len: file_len(&args.audit)?,
        input_sha256: sha256_file(&args.input)?,
        output_sha256: sha256_file(&args.output)?,
        audit_sha256: sha256_file(&args.audit)?,
        completed_pages: checkpoint.completed_pages,
        blocked_pages: checkpoint.blocked_pages,
    })
}

fn verify_completion_marker(args: &RemapArgs, marker: &CompletionMarker) -> SafeResult<()> {
    let paths_match = marker.version == 1
        && marker.input_path == path_identity(&args.input)
        && marker.output_path == path_identity(&args.output)
        && marker.audit_path == path_identity(&args.audit);
    let lengths_match = file_len(&args.input)? == marker.input_len
        && file_len(&args.output)? == marker.output_len
        && file_len(&args.audit)? == marker.audit_len;
    let hashes_match = sha256_file(&args.input)? == marker.input_sha256
        && sha256_file(&args.output)? == marker.output_sha256
        && sha256_file(&args.audit)? == marker.audit_sha256;
    if paths_match && lengths_match && hashes_match {
        Ok(())
    } else {
        Err(SafeError::new("completed_state_mismatch"))
    }
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path, code: &'static str) -> SafeResult<T> {
    let file = File::open(path).map_err(|_| SafeError::new(code))?;
    serde_json::from_reader(BufReader::new(file)).map_err(|_| SafeError::new(code))
}

fn write_checkpoint<T: Serialize>(path: &Path, checkpoint: &T) -> SafeResult<()> {
    let temp = suffixed(path, ".tmp");
    let file = File::create(&temp).map_err(|_| SafeError::new("checkpoint_write"))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, checkpoint)
        .map_err(|_| SafeError::new("checkpoint_write"))?;
    writer
        .write_all(b"\n")
        .map_err(|_| SafeError::new("checkpoint_write"))?;
    writer
        .flush()
        .map_err(|_| SafeError::new("checkpoint_write"))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|_| SafeError::new("checkpoint_write"))?;
    replace_file(&temp, path)
}

fn replace_file(from: &Path, to: &Path) -> SafeResult<()> {
    remove_if_exists(to)?;
    fs::rename(from, to).map_err(|_| SafeError::new("atomic_rename"))
}

fn remove_if_exists(path: &Path) -> SafeResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SafeError::new("file_remove")),
    }
}

#[derive(Debug, Clone)]
struct GraphemeInfo {
    start: usize,
    end: usize,
    is_line_break: bool,
}

#[derive(Debug, Clone)]
struct Token {
    ch: char,
    grapheme_start: usize,
    grapheme_end: usize,
    had_newline: bool,
}

#[derive(Debug, Clone, Copy)]
enum EditOp {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

#[derive(Debug)]
enum SpanDecision {
    Exact(BTreeSet<usize>),
    ConservativeToken(BTreeSet<usize>),
    ConservativeLine(BTreeSet<u32>),
    Removed,
    Blocked(&'static str),
}

fn process_page(page: PageInput, line_number: usize) -> (PageOutput, PageAudit) {
    let safe_id = if valid_identifier(&page.page_id) {
        page.page_id.clone()
    } else {
        format!("line-{line_number:08}")
    };
    if !valid_identifier(&page.page_id) {
        return blocked_page(safe_id, "invalid_page_id");
    }

    let spans: Vec<ReviewedSpan> = page
        .spans
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if spans.iter().any(|span| !valid_identifier(&span.reason)) {
        return blocked_page(safe_id, "invalid_reason_code");
    }
    if let Some(code) = validate_spans(&page.old_text, &spans) {
        return blocked_page(safe_id, code);
    }

    let revised_graphemes = graphemes(&page.revised_text);
    let glyphs = match validate_geometry(&page.revised_text, &revised_graphemes, &page.glyphs) {
        Ok(glyphs) => glyphs,
        Err(code) => return blocked_page(safe_id, code),
    };
    let old_graphemes = graphemes(&page.old_text);
    let old_tokens = canonical_tokens(&page.old_text, &old_graphemes);
    let revised_tokens = canonical_tokens(&page.revised_text, &revised_graphemes);
    let old_chars: Vec<char> = old_tokens.iter().map(|token| token.ch).collect();
    let revised_chars: Vec<char> = revised_tokens.iter().map(|token| token.ch).collect();
    let mut edit_script: Option<Result<Vec<EditOp>, &'static str>> = None;

    let mut rectangle_set = BTreeSet::new();
    let mut reason_codes = BTreeSet::new();
    let mut decisions = Vec::new();
    let mut exact_spans = 0;
    let mut conservative_spans = 0;
    let mut removed_spans = 0;
    let mut page_status = "exact";

    for span in &spans {
        reason_codes.insert(span.reason.clone());
        let Some((token_start, token_end)) =
            token_interval_for_bytes(&old_tokens, &old_graphemes, span.start, span.end)
        else {
            return blocked_page(safe_id, "empty_span_after_normalization");
        };
        let pattern = &old_chars[token_start..token_end];
        let occurrences = find_occurrences(&revised_chars, pattern);
        let decision = if !occurrences.is_empty() {
            let literal_decision = if occurrences.len() == 1 {
                SpanDecision::Exact(token_range_graphemes(
                    &revised_tokens,
                    occurrences[0],
                    occurrences[0] + pattern.len(),
                ))
            } else {
                let scored: Vec<(usize, usize)> = occurrences
                    .iter()
                    .map(|&candidate| {
                        (
                            candidate,
                            context_score(
                                &old_chars,
                                token_start,
                                token_end,
                                &revised_chars,
                                candidate,
                                candidate + pattern.len(),
                            ),
                        )
                    })
                    .collect();
                let best = scored.iter().map(|(_, score)| *score).max().unwrap_or(0);
                let winners: Vec<usize> = scored
                    .iter()
                    .filter(|(_, score)| *score == best)
                    .map(|(candidate, _)| *candidate)
                    .collect();
                if best > 0 && winners.len() == 1 {
                    SpanDecision::Exact(token_range_graphemes(
                        &revised_tokens,
                        winners[0],
                        winners[0] + pattern.len(),
                    ))
                } else {
                    let mut all = BTreeSet::new();
                    for candidate in occurrences {
                        all.extend(token_range_graphemes(
                            &revised_tokens,
                            candidate,
                            candidate + pattern.len(),
                        ));
                    }
                    SpanDecision::ConservativeToken(all)
                }
            };

            let script_result =
                edit_script.get_or_insert_with(|| myers_diff(&old_chars, &revised_chars));
            match (literal_decision, script_result) {
                (SpanDecision::Exact(literal), Ok(script)) => {
                    let mapped =
                        map_span_through_edits(script, token_start, token_end, &revised_tokens);
                    if mapped == literal {
                        SpanDecision::Exact(literal)
                    } else {
                        let mut conservative = literal;
                        conservative.extend(mapped);
                        if conservative.is_empty() {
                            SpanDecision::Blocked("ambiguous_mapping")
                        } else {
                            SpanDecision::ConservativeToken(conservative)
                        }
                    }
                }
                (SpanDecision::ConservativeToken(mut literal), Ok(script)) => {
                    literal.extend(map_span_through_edits(
                        script,
                        token_start,
                        token_end,
                        &revised_tokens,
                    ));
                    if literal.is_empty() {
                        SpanDecision::Blocked("ambiguous_mapping")
                    } else {
                        SpanDecision::ConservativeToken(literal)
                    }
                }
                (_, Err(code)) => SpanDecision::Blocked(code),
                _ => unreachable!("literal matching returns exact or conservative token"),
            }
        } else {
            let script_result =
                edit_script.get_or_insert_with(|| myers_diff(&old_chars, &revised_chars));
            match script_result {
                Ok(script) => {
                    let mapped =
                        map_span_through_edits(script, token_start, token_end, &revised_tokens);
                    if unique_flanks(&old_chars, token_start, token_end, &revised_chars, &mapped) {
                        if mapped.is_empty() {
                            SpanDecision::Removed
                        } else {
                            SpanDecision::Exact(mapped)
                        }
                    } else {
                        let lines = lines_for_graphemes(&mapped, &revised_graphemes, &glyphs);
                        if lines.is_empty() {
                            SpanDecision::Blocked("ambiguous_mapping")
                        } else {
                            SpanDecision::ConservativeLine(lines)
                        }
                    }
                }
                Err(code) => SpanDecision::Blocked(code),
            }
        };

        match decision {
            SpanDecision::Exact(indices) => {
                exact_spans += 1;
                decisions.push("exact".to_string());
                add_glyph_rectangles(&mut rectangle_set, &indices, &glyphs);
            }
            SpanDecision::ConservativeToken(indices) => {
                conservative_spans += 1;
                page_status = "conservative";
                decisions.push("fallback_token".to_string());
                add_glyph_rectangles(&mut rectangle_set, &indices, &glyphs);
            }
            SpanDecision::ConservativeLine(lines) => {
                conservative_spans += 1;
                page_status = "conservative";
                decisions.push("fallback_line".to_string());
                for glyph in glyphs.values().filter(|glyph| lines.contains(&glyph.2)) {
                    rectangle_set.insert(glyph_rect(*glyph));
                }
            }
            SpanDecision::Removed => {
                removed_spans += 1;
                decisions.push("removed".to_string());
            }
            SpanDecision::Blocked(code) => return blocked_page(safe_id, code),
        }
    }

    let mut rectangles: Vec<Rect> = rectangle_set.into_iter().collect();
    rectangles.sort_by_key(|rect| (rect.y, rect.x, rect.w, rect.h));
    let output = PageOutput {
        page_id: safe_id.clone(),
        status: page_status.to_string(),
        rectangles,
        error_code: None,
    };
    let audit = PageAudit {
        page_id: safe_id,
        status: page_status.to_string(),
        reviewed_spans: spans.len(),
        exact_spans,
        conservative_spans,
        removed_spans,
        rectangle_count: output.rectangles.len(),
        reason_codes: reason_codes.into_iter().collect(),
        decisions,
        error_code: None,
    };
    (output, audit)
}

fn blocked_page(page_id: String, code: &'static str) -> (PageOutput, PageAudit) {
    (
        PageOutput {
            page_id: page_id.clone(),
            status: "blocked".to_string(),
            rectangles: Vec::new(),
            error_code: Some(code.to_string()),
        },
        PageAudit {
            page_id,
            status: "blocked".to_string(),
            reviewed_spans: 0,
            exact_spans: 0,
            conservative_spans: 0,
            removed_spans: 0,
            rectangle_count: 0,
            reason_codes: Vec::new(),
            decisions: Vec::new(),
            error_code: Some(code.to_string()),
        },
    )
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn validate_spans(text: &str, spans: &[ReviewedSpan]) -> Option<&'static str> {
    for span in spans {
        if span.start >= span.end || span.end > text.len() {
            return Some("corrupt_offset");
        }
        if !text.is_char_boundary(span.start) || !text.is_char_boundary(span.end) {
            return Some("invalid_utf8_boundary");
        }
    }
    None
}

fn graphemes(text: &str) -> Vec<GraphemeInfo> {
    let mut result = Vec::new();
    for (start, grapheme) in text.grapheme_indices(true) {
        result.push(GraphemeInfo {
            start,
            end: start + grapheme.len(),
            is_line_break: grapheme.contains(['\n', '\r']),
        });
    }
    result
}

fn canonical_tokens(text: &str, graphemes: &[GraphemeInfo]) -> Vec<Token> {
    let mut raw = Vec::new();
    for (index, grapheme) in graphemes.iter().enumerate() {
        let value = &text[grapheme.start..grapheme.end];
        for ch in value.nfkc() {
            if ch == '\u{00ad}' {
                continue;
            }
            let ch = if ch.is_whitespace() { ' ' } else { ch };
            if ch == ' ' && raw.last().is_some_and(|token: &Token| token.ch == ' ') {
                let previous = raw.last_mut().expect("last token exists");
                previous.grapheme_end = index + 1;
                previous.had_newline |= grapheme.is_line_break;
            } else {
                raw.push(Token {
                    ch,
                    grapheme_start: index,
                    grapheme_end: index + 1,
                    had_newline: grapheme.is_line_break,
                });
            }
        }
    }

    let mut result = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        let dehyphenated = index + 2 < raw.len()
            && raw[index].ch == '-'
            && raw[index + 1].ch == ' '
            && raw[index + 1].had_newline
            && result
                .last()
                .is_some_and(|token: &Token| token.ch.is_alphanumeric())
            && raw[index + 2].ch.is_alphanumeric();
        if dehyphenated {
            index += 2;
        } else {
            result.push(raw[index].clone());
            index += 1;
        }
    }
    result
}

fn validate_geometry(
    _text: &str,
    graphemes: &[GraphemeInfo],
    supplied: &[Glyph],
) -> Result<BTreeMap<usize, Glyph>, &'static str> {
    let mut by_range = BTreeMap::new();
    for glyph in supplied {
        if glyph.0 >= glyph.1 || glyph.5 <= 0 || glyph.6 <= 0 {
            return Err("contradictory_geometry");
        }
        if by_range.insert((glyph.0, glyph.1), *glyph).is_some() {
            return Err("contradictory_geometry");
        }
    }
    let mut result = BTreeMap::new();
    for (index, grapheme) in graphemes.iter().enumerate() {
        if grapheme.is_line_break {
            continue;
        }
        let Some(glyph) = by_range.remove(&(grapheme.start, grapheme.end)) else {
            return Err("missing_geometry");
        };
        result.insert(index, glyph);
    }
    if by_range.is_empty() {
        Ok(result)
    } else {
        Err("contradictory_geometry")
    }
}

fn token_interval_for_bytes(
    tokens: &[Token],
    graphemes: &[GraphemeInfo],
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let mut first = None;
    let mut last = None;
    for (index, token) in tokens.iter().enumerate() {
        let token_start = graphemes[token.grapheme_start].start;
        let token_end = graphemes[token.grapheme_end - 1].end;
        if token_start < end && token_end > start {
            first.get_or_insert(index);
            last = Some(index + 1);
        }
    }
    first.zip(last)
}

fn find_occurrences(haystack: &[char], needle: &[char]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let first = needle[0];
    haystack
        .windows(needle.len())
        .enumerate()
        .filter(|(_, window)| window[0] == first && *window == needle)
        .map(|(index, _)| index)
        .collect()
}

fn context_score(
    old: &[char],
    old_start: usize,
    old_end: usize,
    revised: &[char],
    revised_start: usize,
    revised_end: usize,
) -> usize {
    let mut score = 0;
    for distance in 1..=32 {
        if old_start < distance || revised_start < distance {
            break;
        }
        if old[old_start - distance] != revised[revised_start - distance] {
            break;
        }
        score += 1;
    }
    for distance in 0..32 {
        if old_end + distance >= old.len() || revised_end + distance >= revised.len() {
            break;
        }
        if old[old_end + distance] != revised[revised_end + distance] {
            break;
        }
        score += 1;
    }
    score
}

fn token_range_graphemes(tokens: &[Token], start: usize, end: usize) -> BTreeSet<usize> {
    let mut result = BTreeSet::new();
    for token in &tokens[start..end] {
        result.extend(token.grapheme_start..token.grapheme_end);
    }
    result
}

fn myers_diff(old: &[char], revised: &[char]) -> Result<Vec<EditOp>, &'static str> {
    let max_distance = old
        .len()
        .saturating_add(revised.len())
        .min(MAX_EDIT_DISTANCE);
    let mut rows: Vec<Vec<i32>> = Vec::with_capacity(max_distance + 1);
    for distance in 0..=max_distance {
        let mut row = vec![-1_i32; distance.saturating_mul(2) + 1];
        for diagonal in (-(distance as isize)..=distance as isize).step_by(2) {
            let mut x = if distance == 0 {
                0
            } else if diagonal == -(distance as isize) {
                row_get(&rows[distance - 1], distance - 1, diagonal + 1)
            } else if diagonal == distance as isize {
                row_get(&rows[distance - 1], distance - 1, diagonal - 1) + 1
            } else {
                let deletion = row_get(&rows[distance - 1], distance - 1, diagonal - 1) + 1;
                let insertion = row_get(&rows[distance - 1], distance - 1, diagonal + 1);
                if deletion >= insertion {
                    deletion
                } else {
                    insertion
                }
            };
            let mut y = x - i32::try_from(diagonal).expect("edit diagonal fits i32");
            while x >= 0
                && y >= 0
                && (x as usize) < old.len()
                && (y as usize) < revised.len()
                && old[x as usize] == revised[y as usize]
            {
                x += 1;
                y += 1;
            }
            row_set(&mut row, distance, diagonal, x);
            if x as usize >= old.len() && y as usize >= revised.len() {
                rows.push(row);
                return Ok(backtrack_myers(&rows, old.len(), revised.len()));
            }
        }
        rows.push(row);
    }
    Err("edit_distance_limit")
}

fn row_get(row: &[i32], distance: usize, diagonal: isize) -> i32 {
    let index = diagonal + distance as isize;
    if index < 0 || index as usize >= row.len() {
        -1
    } else {
        row[index as usize]
    }
}

fn row_set(row: &mut [i32], distance: usize, diagonal: isize, value: i32) {
    let index = (diagonal + distance as isize) as usize;
    row[index] = value;
}

fn backtrack_myers(rows: &[Vec<i32>], old_len: usize, revised_len: usize) -> Vec<EditOp> {
    let mut x = old_len as isize;
    let mut y = revised_len as isize;
    let mut reverse = Vec::with_capacity(old_len.saturating_add(revised_len));
    for distance in (1..rows.len()).rev() {
        let diagonal = x - y;
        let previous = &rows[distance - 1];
        let previous_diagonal = if diagonal == -(distance as isize)
            || (diagonal != distance as isize
                && row_get(previous, distance - 1, diagonal - 1)
                    < row_get(previous, distance - 1, diagonal + 1))
        {
            diagonal + 1
        } else {
            diagonal - 1
        };
        let previous_x = isize::try_from(row_get(previous, distance - 1, previous_diagonal))
            .expect("stored edit coordinate fits isize");
        let previous_y = previous_x - previous_diagonal;
        while x > previous_x && y > previous_y {
            reverse.push(EditOp::Equal((x - 1) as usize, (y - 1) as usize));
            x -= 1;
            y -= 1;
        }
        if x == previous_x {
            reverse.push(EditOp::Insert((y - 1) as usize));
            y -= 1;
        } else {
            reverse.push(EditOp::Delete((x - 1) as usize));
            x -= 1;
        }
    }
    while x > 0 && y > 0 {
        reverse.push(EditOp::Equal((x - 1) as usize, (y - 1) as usize));
        x -= 1;
        y -= 1;
    }
    while x > 0 {
        reverse.push(EditOp::Delete((x - 1) as usize));
        x -= 1;
    }
    while y > 0 {
        reverse.push(EditOp::Insert((y - 1) as usize));
        y -= 1;
    }
    reverse.reverse();
    reverse
}

fn map_span_through_edits(
    script: &[EditOp],
    span_start: usize,
    span_end: usize,
    revised_tokens: &[Token],
) -> BTreeSet<usize> {
    let mut revised_indices = BTreeSet::new();
    let mut index = 0;
    let mut old_cursor = 0;
    while index < script.len() {
        match script[index] {
            EditOp::Equal(old_index, revised_index) => {
                if (span_start..span_end).contains(&old_index) {
                    revised_indices.insert(revised_index);
                }
                old_cursor = old_index + 1;
                index += 1;
            }
            EditOp::Delete(_) | EditOp::Insert(_) => {
                let insertion_point = old_cursor;
                let mut deleted = Vec::new();
                let mut inserted = Vec::new();
                while index < script.len() {
                    match script[index] {
                        EditOp::Delete(old_index) => {
                            deleted.push(old_index);
                            old_cursor = old_index + 1;
                        }
                        EditOp::Insert(revised_index) => inserted.push(revised_index),
                        EditOp::Equal(_, _) => break,
                    }
                    index += 1;
                }
                let touches_deleted = deleted
                    .iter()
                    .any(|old_index| (span_start..span_end).contains(old_index));
                let inserted_inside = insertion_point > span_start && insertion_point < span_end;
                if touches_deleted || inserted_inside {
                    revised_indices.extend(inserted);
                }
            }
        }
    }
    let mut grapheme_indices = BTreeSet::new();
    for revised_index in revised_indices {
        if let Some(token) = revised_tokens.get(revised_index) {
            grapheme_indices.extend(token.grapheme_start..token.grapheme_end);
        }
    }
    grapheme_indices
}

fn unique_flanks(
    old: &[char],
    span_start: usize,
    span_end: usize,
    revised: &[char],
    mapped_graphemes: &BTreeSet<usize>,
) -> bool {
    let left = find_unique_left_flank(old, revised, span_start);
    let right = find_unique_right_flank(old, revised, span_end);
    let left_ok = span_start == 0 || left.is_some();
    let right_ok = span_end == old.len() || right.is_some();
    if !left_ok || !right_ok {
        return false;
    }
    match (left, right) {
        // A deleted token can cause the deterministic diff to assign one
        // shared boundary whitespace to either flank.
        (Some(left_end), Some(right_start)) => left_end <= right_start.saturating_add(1),
        _ => !mapped_graphemes.is_empty() || old.is_empty() || revised.is_empty(),
    }
}

fn find_unique_left_flank(old: &[char], revised: &[char], span_start: usize) -> Option<usize> {
    for distance in 0..=64 {
        let end = span_start.checked_sub(distance)?;
        for width in (4..=8).rev() {
            let Some(start) = end.checked_sub(width) else {
                continue;
            };
            let needle = &old[start..end];
            if find_occurrences(old, needle).len() == 1 {
                let matches = find_occurrences(revised, needle);
                if matches.len() == 1 {
                    return Some(matches[0] + width);
                }
            }
        }
    }
    None
}

fn find_unique_right_flank(old: &[char], revised: &[char], span_end: usize) -> Option<usize> {
    for distance in 0..=64 {
        let start = span_end.saturating_add(distance);
        if start >= old.len() {
            break;
        }
        for width in (4..=8).rev() {
            let end = start.saturating_add(width);
            if end > old.len() {
                continue;
            }
            let needle = &old[start..end];
            if find_occurrences(old, needle).len() == 1 {
                let matches = find_occurrences(revised, needle);
                if matches.len() == 1 {
                    return Some(matches[0]);
                }
            }
        }
    }
    None
}

fn lines_for_graphemes(
    indices: &BTreeSet<usize>,
    _graphemes: &[GraphemeInfo],
    glyphs: &BTreeMap<usize, Glyph>,
) -> BTreeSet<u32> {
    indices
        .iter()
        .filter_map(|index| glyphs.get(index).map(|glyph| glyph.2))
        .collect()
}

fn add_glyph_rectangles(
    rectangles: &mut BTreeSet<Rect>,
    indices: &BTreeSet<usize>,
    glyphs: &BTreeMap<usize, Glyph>,
) {
    for index in indices {
        if let Some(glyph) = glyphs.get(index) {
            rectangles.insert(glyph_rect(*glyph));
        }
    }
}

const fn glyph_rect(glyph: Glyph) -> Rect {
    Rect {
        x: glyph.3,
        y: glyph.4,
        w: glyph.5,
        h: glyph.6,
    }
}

fn generate_suite(dir: &Path, generated_pages: usize) -> SafeResult<()> {
    if generated_pages < 200 {
        return Err(SafeError::new("fixture_count_below_acceptance"));
    }
    fs::create_dir_all(dir).map_err(|_| SafeError::new("generator_directory"))?;
    let malformed_dir = dir.join("malformed");
    fs::create_dir_all(&malformed_dir).map_err(|_| SafeError::new("generator_directory"))?;
    let pages_path = dir.join("pages.jsonl");
    let variant_path = dir.join("pages_shuffled_duplicated.jsonl");
    let expected_path = dir.join("expected.jsonl");
    let sentinel_path = dir.join("sentinels.json");
    let mut pages_writer =
        BufWriter::new(File::create(&pages_path).map_err(|_| SafeError::new("generator_write"))?);
    let mut variant_writer =
        BufWriter::new(File::create(&variant_path).map_err(|_| SafeError::new("generator_write"))?);
    let mut expected_writer = BufWriter::new(
        File::create(&expected_path).map_err(|_| SafeError::new("generator_write"))?,
    );
    let mut sentinels = Vec::new();

    for index in 0..generated_pages {
        let (page, expected, sentinel) = generated_case(index);
        write_json_line(&mut pages_writer, &page)?;
        let mut variant = page.clone();
        variant.spans.reverse();
        if let Some(first) = variant.spans.first().cloned() {
            variant.spans.push(first);
        }
        write_json_line(&mut variant_writer, &variant)?;
        write_json_line(&mut expected_writer, &expected)?;
        sentinels.push(sentinel);
    }
    for hand in hand_cases() {
        write_json_line(&mut pages_writer, &hand.page)?;
        let mut variant = hand.page.clone();
        variant.spans.reverse();
        variant.spans.extend(variant.spans.clone());
        write_json_line(&mut variant_writer, &variant)?;
        write_json_line(&mut expected_writer, &hand.expected)?;
        sentinels.extend(hand.sentinels);
    }
    pages_writer
        .flush()
        .map_err(|_| SafeError::new("generator_write"))?;
    variant_writer
        .flush()
        .map_err(|_| SafeError::new("generator_write"))?;
    expected_writer
        .flush()
        .map_err(|_| SafeError::new("generator_write"))?;
    let sentinel_file =
        File::create(sentinel_path).map_err(|_| SafeError::new("generator_write"))?;
    serde_json::to_writer(BufWriter::new(sentinel_file), &sentinels)
        .map_err(|_| SafeError::new("generator_write"))?;

    write_malformed_cases(&malformed_dir)?;
    Ok(())
}

#[derive(Debug)]
struct HandCase {
    page: PageInput,
    expected: PageOutput,
    sentinels: Vec<String>,
}

fn generated_case(index: usize) -> (PageInput, PageOutput, String) {
    let serial = format!("{index:04}");
    let (old_text, revised_text, old_needle, revised_needle, occurrence) = match index % 10 {
        0 => {
            let secret = format!("SECRET_ALPHA_{serial}");
            (
                format!("alpha {secret} omega"),
                format!("alpha {secret} omega"),
                secret.clone(),
                secret,
                0,
            )
        }
        1 => {
            let secret = format!("SECRET_SHIFT_{serial}");
            (
                format!("alpha removable words {secret} omega"),
                format!("alpha {secret} omega"),
                secret.clone(),
                secret,
                0,
            )
        }
        2 => {
            let old_secret = format!("SECRET_CAFE_{serial}_e\u{301}");
            let revised_secret = format!("SECRET_CAFE_{serial}_é");
            (
                format!("alpha {old_secret} omega"),
                format!("alpha {revised_secret} omega"),
                old_secret,
                revised_secret,
                0,
            )
        }
        3 => {
            let old_secret = format!("SECRET_SOFT\u{ad}HYPHEN_{serial}");
            let revised_secret = format!("SECRET_SOFTHYPHEN_{serial}");
            (
                format!("alpha {old_secret} omega"),
                format!("alpha {revised_secret} omega"),
                old_secret,
                revised_secret,
                0,
            )
        }
        4 => {
            let old_secret = format!("SECRET_INTER-\nNAL_{serial}");
            let revised_secret = format!("SECRET_INTERNAL_{serial}");
            (
                format!("alpha {old_secret} omega"),
                format!("alpha {revised_secret} omega"),
                old_secret,
                revised_secret,
                0,
            )
        }
        5 => {
            let old_secret = format!("SECRET   WHITE   {serial}");
            let revised_secret = format!("SECRET WHITE {serial}");
            (
                format!("alpha {old_secret} omega"),
                format!("alpha {revised_secret} omega"),
                old_secret,
                revised_secret,
                0,
            )
        }
        6 => {
            let old_secret = format!("SECRET_ﬁLE_{serial}");
            let revised_secret = format!("SECRET_fiLE_{serial}");
            (
                format!("alpha {old_secret} omega"),
                format!("alpha {revised_secret} omega"),
                old_secret,
                revised_secret,
                0,
            )
        }
        7 => {
            let old_secret = format!("SECR3T_OCR_{serial}");
            let revised_secret = format!("SECRET_OCR_{serial}");
            (
                format!("unique-left {old_secret} unique-right"),
                format!("unique-left {revised_secret} unique-right"),
                old_secret,
                revised_secret,
                0,
            )
        }
        8 => {
            let secret = format!("秘密_é_{serial}");
            (
                format!("alpha {secret} omega"),
                format!("alpha {secret} omega"),
                secret.clone(),
                secret,
                0,
            )
        }
        _ => {
            let secret = format!("SECRET_REPEAT_{serial}");
            (
                format!("alpha {secret} omega then beta {secret} gamma"),
                format!("alpha {secret} omega then beta {secret} gamma"),
                secret.clone(),
                secret,
                0,
            )
        }
    };
    let old_start = old_text
        .find(&old_needle)
        .expect("generated old needle exists");
    let page_id = format!("fixture-{index:04}");
    let glyphs = geometry(&revised_text);
    let target_start = nth_find(&revised_text, &revised_needle, occurrence);
    let rectangles = rectangles_for_ranges(
        &glyphs,
        &[(target_start, target_start + revised_needle.len())],
    );
    let page = PageInput {
        page_id: page_id.clone(),
        old_text,
        revised_text,
        glyphs,
        spans: vec![ReviewedSpan {
            start: old_start,
            end: old_start + old_needle.len(),
            reason: "PUBLIC_RECORDS_RULE".to_string(),
        }],
    };
    let expected = PageOutput {
        page_id,
        status: "exact".to_string(),
        rectangles,
        error_code: None,
    };
    (page, expected, revised_needle)
}

fn hand_cases() -> Vec<HandCase> {
    let mut cases = Vec::new();

    let secret = "AMBIGUOUS_SECRET_X".to_string();
    let old_text = format!("left {secret} right");
    let revised_text = format!("{secret} gap {secret}");
    let glyphs = geometry(&revised_text);
    let first = nth_find(&revised_text, &secret, 0);
    let second = nth_find(&revised_text, &secret, 1);
    cases.push(HandCase {
        page: PageInput {
            page_id: "hand-ambiguous-token".to_string(),
            old_text: old_text.clone(),
            revised_text: revised_text.clone(),
            glyphs: glyphs.clone(),
            spans: vec![ReviewedSpan {
                start: old_text.find(&secret).expect("needle exists"),
                end: old_text.find(&secret).expect("needle exists") + secret.len(),
                reason: "PERSON_ID".to_string(),
            }],
        },
        expected: PageOutput {
            page_id: "hand-ambiguous-token".to_string(),
            status: "conservative".to_string(),
            rectangles: rectangles_for_ranges(
                &glyphs,
                &[
                    (first, first + secret.len()),
                    (second, second + secret.len()),
                ],
            ),
            error_code: None,
        },
        sentinels: vec![secret],
    });

    let secret = "OVERLAPPING_SECRET_Y".to_string();
    let text = format!("head {secret} tail");
    let secret_start = text.find(&secret).expect("needle exists");
    let glyphs = geometry(&text);
    cases.push(HandCase {
        page: PageInput {
            page_id: "hand-overlap-duplicate".to_string(),
            old_text: text.clone(),
            revised_text: text.clone(),
            glyphs: glyphs.clone(),
            spans: vec![
                ReviewedSpan {
                    start: secret_start,
                    end: secret_start + secret.len(),
                    reason: "ACCOUNT_NUMBER".to_string(),
                },
                ReviewedSpan {
                    start: secret_start + 4,
                    end: secret_start + secret.len(),
                    reason: "ACCOUNT_NUMBER".to_string(),
                },
            ],
        },
        expected: PageOutput {
            page_id: "hand-overlap-duplicate".to_string(),
            status: "exact".to_string(),
            rectangles: rectangles_for_ranges(
                &glyphs,
                &[(secret_start, secret_start + secret.len())],
            ),
            error_code: None,
        },
        sentinels: vec![secret],
    });

    let secret = "DELETED_SECRET_Z".to_string();
    let old_text = format!("unique-left {secret} unique-right");
    let revised_text = "unique-left unique-right".to_string();
    cases.push(HandCase {
        page: PageInput {
            page_id: "hand-deleted".to_string(),
            old_text: old_text.clone(),
            revised_text: revised_text.clone(),
            glyphs: geometry(&revised_text),
            spans: vec![ReviewedSpan {
                start: old_text.find(&secret).expect("needle exists"),
                end: old_text.find(&secret).expect("needle exists") + secret.len(),
                reason: "PERSON_ID".to_string(),
            }],
        },
        expected: PageOutput {
            page_id: "hand-deleted".to_string(),
            status: "exact".to_string(),
            rectangles: Vec::new(),
            error_code: None,
        },
        sentinels: vec![secret],
    });

    let secret = "BOUNDARY_SECRET_Q".to_string();
    let glyphs = geometry(&secret);
    cases.push(HandCase {
        page: PageInput {
            page_id: "hand-whole-page".to_string(),
            old_text: secret.clone(),
            revised_text: secret.clone(),
            glyphs: glyphs.clone(),
            spans: vec![ReviewedSpan {
                start: 0,
                end: secret.len(),
                reason: "SEALED_RECORD".to_string(),
            }],
        },
        expected: PageOutput {
            page_id: "hand-whole-page".to_string(),
            status: "exact".to_string(),
            rectangles: rectangles_for_ranges(&glyphs, &[(0, secret.len())]),
            error_code: None,
        },
        sentinels: vec![secret],
    });
    cases
}

fn nth_find(haystack: &str, needle: &str, occurrence: usize) -> usize {
    haystack
        .match_indices(needle)
        .nth(occurrence)
        .map(|(index, _)| index)
        .expect("generated revised needle exists")
}

fn geometry(text: &str) -> Vec<Glyph> {
    let mut result = Vec::new();
    let mut line = 0_u32;
    let mut column = 0_i32;
    for (start, value) in text.grapheme_indices(true) {
        if value.contains(['\n', '\r']) {
            line += 1;
            column = 0;
            continue;
        }
        result.push(Glyph(
            start,
            start + value.len(),
            line,
            column * 10,
            i32::try_from(line).expect("fixture line fits i32") * 20,
            9,
            12,
        ));
        column += 1;
    }
    result
}

fn rectangles_for_ranges(glyphs: &[Glyph], ranges: &[(usize, usize)]) -> Vec<Rect> {
    let mut result = BTreeSet::new();
    for glyph in glyphs {
        if ranges
            .iter()
            .any(|(start, end)| glyph.0 < *end && glyph.1 > *start)
        {
            result.insert(glyph_rect(*glyph));
        }
    }
    let mut result: Vec<_> = result.into_iter().collect();
    result.sort_by_key(|rect| (rect.y, rect.x, rect.w, rect.h));
    result
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> SafeResult<()> {
    serde_json::to_writer(&mut *writer, value).map_err(|_| SafeError::new("generator_write"))?;
    writer
        .write_all(b"\n")
        .map_err(|_| SafeError::new("generator_write"))
}

fn write_malformed_cases(dir: &Path) -> SafeResult<()> {
    let text = "prefix 秘密 suffix".to_string();
    let start = text.find("秘密").expect("needle exists");
    let base = PageInput {
        page_id: "malformed".to_string(),
        old_text: text.clone(),
        revised_text: text.clone(),
        glyphs: geometry(&text),
        spans: vec![ReviewedSpan {
            start,
            end: start + "秘密".len(),
            reason: "PERSON_ID".to_string(),
        }],
    };
    write_single_page(&dir.join("valid.jsonl"), &base)?;

    let mut corrupt = base.clone();
    corrupt.page_id = "corrupt-offset".to_string();
    corrupt.spans[0].end = corrupt.old_text.len() + 1;
    write_single_page(&dir.join("corrupt_offset.jsonl"), &corrupt)?;

    let mut boundary = base.clone();
    boundary.page_id = "invalid-boundary".to_string();
    boundary.spans[0].start = start + 1;
    write_single_page(&dir.join("invalid_utf8_boundary.jsonl"), &boundary)?;

    let mut missing = base.clone();
    missing.page_id = "missing-geometry".to_string();
    missing.glyphs.pop();
    write_single_page(&dir.join("missing_geometry.jsonl"), &missing)?;

    let mut contradictory = base;
    contradictory.page_id = "contradictory-geometry".to_string();
    contradictory.glyphs.push(contradictory.glyphs[0]);
    write_single_page(&dir.join("contradictory_geometry.jsonl"), &contradictory)?;

    let invalid_path = dir.join("invalid_utf8.jsonl");
    let mut invalid = File::create(invalid_path).map_err(|_| SafeError::new("generator_write"))?;
    invalid
        .write_all(b"{\"page_id\":\"invalid-utf8\",\"old_text\":\"")
        .map_err(|_| SafeError::new("generator_write"))?;
    invalid
        .write_all(&[0xff])
        .map_err(|_| SafeError::new("generator_write"))?;
    invalid
        .write_all(b"\"}\n")
        .map_err(|_| SafeError::new("generator_write"))?;
    Ok(())
}

fn write_single_page(path: &Path, page: &PageInput) -> SafeResult<()> {
    let file = File::create(path).map_err(|_| SafeError::new("generator_write"))?;
    let mut writer = BufWriter::new(file);
    write_json_line(&mut writer, page)?;
    writer
        .flush()
        .map_err(|_| SafeError::new("generator_write"))
}

fn generate_workload(path: &Path, pages: usize, scalars: usize, spans: usize) -> SafeResult<()> {
    if scalars < spans.saturating_mul(8) || spans > 100 {
        return Err(SafeError::new("invalid_workload_shape"));
    }
    let file = File::create(path).map_err(|_| SafeError::new("generator_write"))?;
    let mut writer = BufWriter::new(file);
    for page_index in 0..pages {
        let mut bytes = vec![b'a'; scalars];
        let mut reviewed = Vec::with_capacity(spans);
        let stride = scalars / spans;
        for span_index in 0..spans {
            let token = format!("Q{span_index:02}Z");
            let start = span_index * stride + 2;
            bytes[start..start + token.len()].copy_from_slice(token.as_bytes());
            reviewed.push(ReviewedSpan {
                start,
                end: start + token.len(),
                reason: "PUBLIC_RECORDS_RULE".to_string(),
            });
        }
        let text = String::from_utf8(bytes).expect("ASCII fixture is UTF-8");
        let glyphs = (0..scalars)
            .map(|index| {
                Glyph(
                    index,
                    index + 1,
                    0,
                    i32::try_from(index % 200).expect("workload column fits i32"),
                    i32::try_from(index / 200).expect("workload row fits i32"),
                    1,
                    1,
                )
            })
            .collect();
        let page = PageInput {
            page_id: format!("workload-{page_index:05}"),
            old_text: text.clone(),
            revised_text: text,
            glyphs,
            spans: reviewed,
        };
        write_json_line(&mut writer, &page)?;
    }
    writer
        .flush()
        .map_err(|_| SafeError::new("generator_write"))
}

fn check_suite(
    input_path: &Path,
    output_path: &Path,
    audit_path: &Path,
    expected_path: &Path,
    sentinel_path: &Path,
    diagnostics_path: Option<&Path>,
) -> SafeResult<String> {
    let expected: Vec<PageOutput> = read_json_lines(expected_path, "checker_expected")?;
    let actual: Vec<PageOutput> = read_json_lines(output_path, "checker_output")?;
    let audit: Vec<PageAudit> = read_json_lines(audit_path, "checker_audit")?;
    let input: Vec<PageInput> = read_json_lines(input_path, "checker_input")?;
    if actual != expected {
        return Err(SafeError::new("rectangle_mismatch"));
    }
    if actual.len() != audit.len() || actual.len() != input.len() || actual.len() < 200 {
        return Err(SafeError::new("coverage_count"));
    }
    for (page, audit_page) in actual.iter().zip(&audit) {
        if page.page_id != audit_page.page_id
            || page.status != audit_page.status
            || page.rectangles.len() != audit_page.rectangle_count
        {
            return Err(SafeError::new("audit_mismatch"));
        }
    }
    let sentinels: Vec<String> = read_json_file(sentinel_path, "checker_sentinels")?;
    if sentinels.len() != input.len() {
        return Err(SafeError::new("sentinel_coverage_count"));
    }
    let mut public_bytes = Vec::new();
    File::open(output_path)
        .and_then(|mut file| file.read_to_end(&mut public_bytes))
        .map_err(|_| SafeError::new("checker_output"))?;
    File::open(audit_path)
        .and_then(|mut file| file.read_to_end(&mut public_bytes))
        .map_err(|_| SafeError::new("checker_audit"))?;
    if let Some(path) = diagnostics_path {
        File::open(path)
            .and_then(|mut file| file.read_to_end(&mut public_bytes))
            .map_err(|_| SafeError::new("checker_diagnostics"))?;
    }
    for sentinel in &sentinels {
        if contains_bytes(&public_bytes, sentinel.as_bytes()) {
            return Err(SafeError::new("sentinel_leak"));
        }
    }

    let conservative = actual
        .iter()
        .filter(|page| page.status == "conservative")
        .count();
    let exact = actual.iter().filter(|page| page.status == "exact").count();
    Ok(format!(
        "checked pages={} exact={} conservative={} sentinel_leaks=0 rectangle_mismatches=0",
        actual.len(),
        exact,
        conservative
    ))
}

fn read_json_lines<T: for<'de> Deserialize<'de>>(
    path: &Path,
    code: &'static str,
) -> SafeResult<Vec<T>> {
    let file = File::open(path).map_err(|_| SafeError::new(code))?;
    let mut result = Vec::new();
    for line in BufReader::new(file).split(b'\n') {
        let line = line.map_err(|_| SafeError::new(code))?;
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_slice(&line).map_err(|_| SafeError::new(code))?;
        result.push(value);
    }
    Ok(result)
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn myers_script_reconstructs_revised_sequence() {
        let cases = [
            ("", ""),
            ("abc", "abc"),
            ("abc", "axc"),
            ("kitten", "sitting"),
            ("repeat repeat", "repeat gap repeat"),
            ("秘密", "秘X密"),
        ];
        for (old, revised) in cases {
            let old: Vec<char> = old.chars().collect();
            let revised: Vec<char> = revised.chars().collect();
            let script = myers_diff(&old, &revised).expect("small edit should align");
            let reconstructed: String = script
                .iter()
                .filter_map(|operation| match operation {
                    EditOp::Equal(_, revised_index) | EditOp::Insert(revised_index) => {
                        Some(revised[*revised_index])
                    }
                    EditOp::Delete(_) => None,
                })
                .collect();
            assert_eq!(reconstructed, revised.iter().collect::<String>());
        }
    }

    #[test]
    fn normalization_handles_required_cleanup_shapes() {
        let pairs = [
            ("e\u{301}", "é"),
            ("ﬁle", "file"),
            ("soft\u{ad}hyphen", "softhyphen"),
            ("inter-\nnal", "internal"),
            ("a   b", "a b"),
        ];
        for (old, revised) in pairs {
            let old_tokens = canonical_tokens(old, &graphemes(old));
            let revised_tokens = canonical_tokens(revised, &graphemes(revised));
            assert_eq!(
                old_tokens.iter().map(|token| token.ch).collect::<String>(),
                revised_tokens
                    .iter()
                    .map(|token| token.ch)
                    .collect::<String>()
            );
        }
    }

    #[test]
    fn duplicate_and_ordered_spans_have_identical_records() {
        let (page, _, _) = generated_case(7);
        let (output, audit) = process_page(page.clone(), 1);
        let mut variant = page;
        variant.spans.extend(variant.spans.clone());
        variant.spans.reverse();
        let (variant_output, variant_audit) = process_page(variant, 1);
        assert_eq!(output, variant_output);
        assert_eq!(audit, variant_audit);
    }

    #[test]
    fn malformed_geometry_blocks_publication() {
        let (mut page, _, _) = generated_case(0);
        page.glyphs.pop();
        let (output, audit) = process_page(page, 1);
        assert_eq!(output.status, "blocked");
        assert_eq!(output.error_code.as_deref(), Some("missing_geometry"));
        assert!(output.rectangles.is_empty());
        assert_eq!(audit.status, "blocked");
    }

    #[test]
    fn changed_first_repeated_occurrence_is_never_mapped_only_to_the_second() {
        let secret = "AMBIGUOUS_SECRET_X";
        let old = format!("{secret} gap {secret}");
        let revised = format!("AMBIGUOUS_SECR3T_X gap {secret}");
        let page = page_with_span("repeat-shift", &old, &revised, 0, secret.len());
        let (output, audit) = process_page(page, 1);

        assert_ne!(output.status, "blocked");
        assert_eq!(audit.status, "conservative");
        assert!(
            output
                .rectangles
                .iter()
                .any(|rectangle| rectangle.x < i32::try_from(secret.len() * 10).unwrap()),
            "changed first occurrence must be covered: {:?}",
            output.rectangles
        );
    }

    #[test]
    fn completed_resume_binds_input_output_and_audit() {
        let dir = temp_test_dir("completed");
        let input = dir.join("pages.jsonl");
        let output = dir.join("output.jsonl");
        let audit = dir.join("audit.jsonl");
        let page = generated_case(0).0;
        write_test_pages(&input, std::slice::from_ref(&page));
        run_remap(&remap_args(&input, &output, &audit, false, None)).expect("complete run");

        write_test_pages(&input, &[page.clone(), page.clone()]);
        let changed_input = run_remap(&remap_args(&input, &output, &audit, true, None))
            .expect_err("changed completed input must fail");
        assert_eq!(changed_input.code, "completed_state_mismatch");

        write_test_pages(&input, std::slice::from_ref(&page));
        fs::write(&output, b"").expect("truncate completed output");
        let truncated_output = run_remap(&remap_args(&input, &output, &audit, true, None))
            .expect_err("truncated completed output must fail");
        assert_eq!(truncated_output.code, "completed_state_mismatch");

        run_remap(&remap_args(&input, &output, &audit, false, None))
            .expect("restore completed run");
        let other_audit = dir.join("other-audit.jsonl");
        fs::copy(&audit, &other_audit).expect("copy audit");
        let changed_audit = run_remap(&remap_args(&input, &output, &other_audit, true, None))
            .expect_err("changed audit path must fail");
        assert_eq!(changed_audit.code, "completed_state_mismatch");
    }

    #[test]
    fn stale_or_short_partial_state_never_completes_corrupted_output() {
        let dir = temp_test_dir("stale");
        let input = dir.join("pages.jsonl");
        let invalid = dir.join("invalid.jsonl");
        let output = dir.join("output.jsonl");
        let audit = dir.join("audit.jsonl");
        let pages = [generated_case(0).0, generated_case(1).0];
        write_test_pages(&input, &pages);
        fs::write(&invalid, b"{\"page_id\":\"").expect("write invalid UTF-8 prefix");
        let mut invalid_bytes = fs::read(&invalid).expect("read invalid prefix");
        invalid_bytes.push(0xff);
        fs::write(&invalid, invalid_bytes).expect("write invalid UTF-8");

        let interrupted = run_remap(&remap_args(&input, &output, &audit, false, Some(1)))
            .expect("controlled interruption");
        assert!(matches!(interrupted, CommandResult::Interrupted(_)));
        let invalid_run = run_remap(&remap_args(&invalid, &output, &audit, false, None))
            .expect_err("invalid fresh run must fail");
        assert_eq!(invalid_run.code, "invalid_utf8");
        assert!(!suffixed(&output, ".checkpoint").exists());

        let restarted = run_remap(&remap_args(&input, &output, &audit, true, None))
            .expect("resume without a checkpoint safely restarts");
        assert!(matches!(restarted, CommandResult::Complete(_)));
        let bytes = fs::read(&output).expect("read restarted output");
        assert!(!bytes.starts_with(&[0]));
        assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 2);
        assert!(suffixed(&output, ".complete").exists());

        run_remap(&remap_args(&input, &output, &audit, false, Some(1)))
            .expect("second controlled interruption");
        fs::write(suffixed(&output, ".partial"), b"").expect("truncate partial output");
        let short = run_remap(&remap_args(&input, &output, &audit, true, None))
            .expect_err("short partial must fail");
        assert_eq!(short.code, "partial_short");
        assert!(!suffixed(&output, ".complete").exists());
    }

    fn page_with_span(
        page_id: &str,
        old_text: &str,
        revised_text: &str,
        start: usize,
        end: usize,
    ) -> PageInput {
        let glyphs = graphemes(revised_text)
            .into_iter()
            .enumerate()
            .map(|(index, grapheme)| {
                Glyph(
                    grapheme.start,
                    grapheme.end,
                    0,
                    i32::try_from(index * 10).unwrap(),
                    0,
                    10,
                    10,
                )
            })
            .collect();
        PageInput {
            page_id: page_id.to_string(),
            old_text: old_text.to_string(),
            revised_text: revised_text.to_string(),
            glyphs,
            spans: vec![ReviewedSpan {
                start,
                end,
                reason: "PERSON_ID".to_string(),
            }],
        }
    }

    fn temp_test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ocr-redaction-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create test directory");
        dir
    }

    fn write_test_pages(path: &Path, pages: &[PageInput]) {
        let mut bytes = Vec::new();
        for page in pages {
            serde_json::to_writer(&mut bytes, page).expect("serialize test page");
            bytes.push(b'\n');
        }
        fs::write(path, bytes).expect("write test pages");
    }

    fn remap_args(
        input: &Path,
        output: &Path,
        audit: &Path,
        resume: bool,
        stop_after: Option<usize>,
    ) -> RemapArgs {
        RemapArgs {
            input: input.to_path_buf(),
            output: output.to_path_buf(),
            audit: audit.to_path_buf(),
            resume,
            stop_after,
        }
    }
}
