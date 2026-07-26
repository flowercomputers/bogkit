use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

struct TestEnv {
    root: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
}

impl TestEnv {
    fn new(name: &str) -> Self {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("bog-bench-cli-{name}-{}-{id}", std::process::id()));
        let home = root.join("home");
        let tmp = root.join("tmp");
        fs::create_dir_all(home.join(".claude/projects/project")).unwrap();
        fs::create_dir_all(&tmp).unwrap();
        Self { root, home, tmp }
    }

    fn transcript(&self, relative: &str) -> PathBuf {
        let path = self.home.join(".claude/projects/project").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_bog-bench"))
            .args(args)
            .env("HOME", &self.home)
            .env("TMPDIR", &self.tmp)
            .output()
            .unwrap()
    }

    fn run_ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "bog-bench {args:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn completed_call(id: &str, tool: &str, started: &str, ended: &str, is_error: bool) -> String {
    [
        serde_json::json!({
            "type": "assistant",
            "timestamp": started,
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": id,
                    "name": tool
                }]
            }
        })
        .to_string(),
        serde_json::json!({
            "type": "user",
            "timestamp": ended,
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": format!("result-{id}"),
                    "is_error": is_error
                }]
            }
        })
        .to_string(),
    ]
    .join("\n")
}

fn pending_call(id: &str, tool: &str, started: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "timestamp": started,
        "message": {
            "content": [{
                "type": "tool_use",
                "id": id,
                "name": tool
            }]
        }
    })
    .to_string()
}

fn write_lines(path: &Path, lines: &[String]) {
    fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

fn append_lines(path: &Path, lines: &[String]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{}", lines.join("\n")).unwrap();
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn no_arguments_prints_help_without_running_demo() {
    let env = TestEnv::new("no-args");
    let output = env.run_ok(&[]);
    let text = stdout(&output);

    assert!(text.contains("usage: bog-bench <command>"));
    assert!(
        !text.contains("== ingested session-a =="),
        "no-argument invocation must not run the stateful demo"
    );
}

#[test]
fn demo_is_isolated_from_persistent_cli_state_across_processes() {
    let env = TestEnv::new("demo-isolation");
    let path = env.transcript("persistent.jsonl");
    write_lines(
        &path,
        &[completed_call(
            "persisted",
            "Read",
            "2026-07-25T00:00:00.000Z",
            "2026-07-25T00:00:01.000Z",
            false,
        )],
    );

    env.run_ok(&["reset"]);
    env.run_ok(&["ingest", path.to_str().unwrap()]);
    env.run_ok(&["demo"]);
    let shown = stdout(&env.run_ok(&["show"]));

    assert!(
        shown.contains("1 tool call"),
        "demo replaced persistent state:\n{shown}"
    );
}

#[test]
fn recent_reconciles_appends_and_retract_uses_the_ingested_snapshot() {
    let env = TestEnv::new("growing");
    let path = env.transcript("growing.jsonl");
    write_lines(
        &path,
        &[completed_call(
            "first",
            "Read",
            "2026-07-25T00:00:00.000Z",
            "2026-07-25T00:00:01.000Z",
            false,
        )],
    );

    env.run_ok(&["recent", "1"]);
    append_lines(
        &path,
        &[completed_call(
            "second",
            "Bash",
            "2026-07-25T00:01:00.000Z",
            "2026-07-25T00:01:01.000Z",
            true,
        )],
    );
    env.run_ok(&["recent", "1"]);
    let grown = stdout(&env.run_ok(&["show"]));
    assert!(
        grown.contains("2 tool calls"),
        "the appended call was not reconciled:\n{grown}"
    );

    append_lines(
        &path,
        &[completed_call(
            "never-ingested",
            "Edit",
            "2026-07-25T00:02:00.000Z",
            "2026-07-25T00:02:01.000Z",
            false,
        )],
    );
    env.run_ok(&["retract", path.to_str().unwrap()]);
    let retracted = stdout(&env.run_ok(&["show"]));
    assert!(
        retracted.contains("0 tool calls"),
        "retract did not remove exactly the stored snapshot:\n{retracted}"
    );
}

#[test]
fn canonical_paths_keep_same_stem_transcripts_distinct() {
    let env = TestEnv::new("same-stem");
    let first = env.transcript("one/shared.jsonl");
    let second = env.transcript("two/shared.jsonl");
    write_lines(
        &first,
        &[completed_call(
            "one",
            "Read",
            "2026-07-25T00:00:00.000Z",
            "2026-07-25T00:00:01.000Z",
            false,
        )],
    );
    write_lines(
        &second,
        &[completed_call(
            "two",
            "Bash",
            "2026-07-25T00:01:00.000Z",
            "2026-07-25T00:01:01.000Z",
            false,
        )],
    );

    env.run_ok(&["ingest", first.to_str().unwrap()]);
    env.run_ok(&["ingest", second.to_str().unwrap()]);
    let shown = stdout(&env.run_ok(&["show"]));

    assert!(
        shown.contains("2 tool calls"),
        "same-stem paths collided:\n{shown}"
    );
}

#[test]
fn parser_reports_malformed_lines_and_unmatched_outcomes_as_unknown() {
    let env = TestEnv::new("diagnostics");
    let path = env.transcript("diagnostics.jsonl");
    write_lines(
        &path,
        &[
            "{ definitely not json".to_string(),
            pending_call("pending", "Read", "2026-07-25T00:00:00.000Z"),
        ],
    );

    let output = env.run_ok(&["ingest", path.to_str().unwrap()]);
    let out = stdout(&output);
    let err = stderr(&output);

    assert!(
        err.contains("1 malformed JSON line"),
        "missing parse diagnostic:\n{err}"
    );
    assert!(
        out.contains("1 unknown outcome"),
        "unmatched call was presented as successful:\n{out}"
    );
    assert!(
        out.contains("0 known outcomes"),
        "unknown call leaked into the error-rate denominator:\n{out}"
    );
}

#[test]
fn one_hour_window_uses_exact_event_timestamps_at_the_cutoff() {
    let env = TestEnv::new("window-cutoff");
    let path = env.transcript("window.jsonl");
    write_lines(
        &path,
        &[
            completed_call(
                "before-cutoff",
                "Read",
                "2026-07-25T00:29:59.999Z",
                "2026-07-25T00:30:00.000Z",
                false,
            ),
            completed_call(
                "at-cutoff",
                "Bash",
                "2026-07-25T00:30:00.000Z",
                "2026-07-25T00:30:01.000Z",
                false,
            ),
            completed_call(
                "latest",
                "Edit",
                "2026-07-25T01:30:00.000Z",
                "2026-07-25T01:30:01.000Z",
                false,
            ),
        ],
    );

    let output = stdout(&env.run_ok(&["window", "1", "1"]));
    assert!(
        output.contains("2 calls still inside the window"),
        "the event one millisecond before the cutoff survived:\n{output}"
    );
}

#[test]
fn benchmark_repeats_trials_and_compares_every_materialized_view() {
    let env = TestEnv::new("benchmark");
    let first = env.transcript("first.jsonl");
    let second = env.transcript("second.jsonl");
    write_lines(
        &first,
        &[completed_call(
            "first",
            "Read",
            "2026-07-25T00:00:00.000Z",
            "2026-07-25T00:00:01.000Z",
            false,
        )],
    );
    write_lines(
        &second,
        &[completed_call(
            "second",
            "Bash",
            "2026-07-25T00:01:00.000Z",
            "2026-07-25T00:01:01.000Z",
            true,
        )],
    );

    let output = stdout(&env.run_ok(&["bench", "1", "3"]));
    assert!(output.contains("trials: 3"), "{output}");
    assert!(
        output.contains("all materialized views agree"),
        "benchmark did not report its full correctness gate:\n{output}"
    );
    assert!(
        output.contains("median") && output.contains("min") && output.contains("max"),
        "benchmark omitted repeated-trial spread:\n{output}"
    );
    assert!(
        output.contains(std::env::consts::OS) && output.contains(std::env::consts::ARCH),
        "benchmark omitted reproducibility environment:\n{output}"
    );
}
