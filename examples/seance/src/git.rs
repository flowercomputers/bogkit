//! Just enough git plumbing, via the system `git` binary.
//!
//! Three primitives feed the séance:
//!   - [`list_commits`] — the repo's first-parent line, oldest first
//!   - [`diff`] — paths changed between two trees (any two commits, or the
//!     empty tree for "before the beginning")
//!   - [`Blobs`] — file contents at any commit, served by one persistent
//!     `git cat-file --batch` child instead of a process spawn per file

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Commit {
    pub oid: String,
    pub at: i64, // unix seconds
    pub author: String,
    pub subject: String,
}

fn run<I, S>(repo: &Path, label: &str, args: I) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .expect("failed to run git (is it installed?)");
    if !out.status.success() {
        eprintln!("seance: git {label} failed");
        std::process::exit(2);
    }
    out.stdout
}

/// The first-parent commit line, oldest first — a linear mainline even when
/// history has merges. `limit` keeps only the most recent n commits.
pub fn list_commits(repo: &Path, limit: Option<usize>) -> Vec<Commit> {
    let mut args = vec![
        "log".to_string(),
        "--first-parent".to_string(),
        "--reverse".to_string(),
        "--format=%H\u{1f}%at\u{1f}%an\u{1f}%s".to_string(),
    ];
    if let Some(n) = limit {
        args.push(format!("-{n}"));
    }
    let out = run(repo, "log", &args);
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| {
            let mut f = line.split('\u{1f}');
            Some(Commit {
                oid: f.next()?.to_string(),
                at: f.next()?.parse().ok()?,
                author: f.next()?.to_string(),
                subject: f.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// The empty tree's oid — asked of the repo rather than hardcoded, so both
/// sha1 and sha256 repositories work.
pub fn empty_tree(repo: &Path) -> String {
    let out = run(repo, "hash-object", ["hash-object", "-t", "tree", "--stdin"]);
    String::from_utf8_lossy(&out).trim().to_string()
}

pub enum Status {
    Upsert,
    Delete,
}

/// Paths changed between two trees. Renames are left split into delete +
/// add — exactly the deltas the pipeline wants.
pub fn diff(repo: &Path, from: &str, to: &str) -> Vec<(Status, String)> {
    let raw = run(
        repo,
        "diff-tree",
        ["diff-tree", "-r", "-z", "--no-renames", "--name-status", from, to],
    );
    // -z output: <status> NUL <path> NUL ...
    let mut fields = raw.split(|&b| b == 0);
    let mut changes = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        if status.is_empty() {
            break;
        }
        let Ok(path) = std::str::from_utf8(path) else {
            continue;
        };
        // `cat-file --batch` requests are line-based; such paths are unreachable
        if path.contains('\n') {
            continue;
        }
        let s = if status[0] == b'D' {
            Status::Delete
        } else {
            Status::Upsert // A, M, T
        };
        changes.push((s, path.to_string()));
    }
    changes
}

fn spawn_cat_file(repo: &Path, mode: &str) -> (Child, ChildStdin, BufReader<ChildStdout>) {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", mode])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn git cat-file");
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    (child, stdin, stdout)
}

// header: `<oid> SP <type> SP <size> LF` | `<spec> SP missing LF`. The spec
// itself may contain spaces, so parse from the right. Returns (type, size).
fn parse_header(header: &str) -> Option<(&str, usize)> {
    let mut fields = header.trim_end().rsplit(' ');
    let size = fields.next()?.parse::<usize>().ok()?;
    Some((fields.next()?, size))
}

/// Per-commit changed paths for every commit in `old..young` on the
/// first-parent line, one process spawn for the whole range — the batch
/// counterpart of [`diff`] for commit-stepped walks. Returns
/// `(commit_oid, paths changed vs its first parent)` per commit; with
/// `--first-parent`, merge commits diff against their first parent, which
/// is exactly the walk's step relation.
pub fn first_parent_changes(repo: &Path, old: &str, young: &str) -> Vec<(String, Vec<String>)> {
    let out = run(
        repo,
        "log",
        [
            "-c",
            "core.quotepath=false",
            "log",
            "--first-parent",
            "--format=%x01%H",
            "--name-only",
            &format!("{old}..{young}"),
        ],
    );
    let text = String::from_utf8_lossy(&out);
    let mut result = Vec::new();
    for record in text.split('\u{1}').skip(1) {
        let mut lines = record.lines();
        let Some(oid) = lines.next() else { continue };
        let paths = lines
            .filter(|l| !l.is_empty() && !l.starts_with('"'))
            .map(|l| l.to_string())
            .collect();
        result.push((oid.trim().to_string(), paths));
    }
    result
}

/// Two persistent `git cat-file` children: `--batch-check` answers "what is
/// this object and how big" without payload, `--batch` streams contents —
/// so oversized blobs can be skipped without ever transporting them.
pub struct Blobs {
    _check_child: Child,
    check_in: ChildStdin,
    check_out: BufReader<ChildStdout>,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Blobs {
    pub fn new(repo: &Path) -> Self {
        let (_check_child, check_in, check_out) = spawn_cat_file(repo, "--batch-check");
        let (child, stdin, stdout) = spawn_cat_file(repo, "--batch");
        Blobs {
            _check_child,
            check_in,
            check_out,
            child,
            stdin,
            stdout,
        }
    }

    /// The object's size in bytes, if `spec` resolves to a blob.
    pub fn blob_size(&mut self, spec: &str) -> Option<usize> {
        self.blob_meta(spec).map(|(_, size)| size)
    }

    /// The object's oid and size, if `spec` resolves to a blob. The oid is
    /// git's content address — a stable, deduplicating reference that can
    /// be read back later with [`Blobs::read`].
    pub fn blob_meta(&mut self, spec: &str) -> Option<(String, usize)> {
        writeln!(self.check_in, "{spec}").ok()?;
        self.check_in.flush().ok()?;
        let mut header = String::new();
        self.check_out.read_line(&mut header).ok()?;
        let mut fields = header.trim_end().rsplit(' ');
        let size = fields.next()?.parse::<usize>().ok()?;
        let typ = fields.next()?;
        let oid = fields.next()?;
        (typ == "blob").then(|| (oid.to_string(), size))
    }

    /// The contents of `spec` (e.g. `<oid>:<path>`), or `None` if the object
    /// is missing or not a blob (submodule gitlinks resolve to commits).
    pub fn read(&mut self, spec: &str) -> Option<Vec<u8>> {
        writeln!(self.stdin, "{spec}").ok()?;
        self.stdin.flush().ok()?;

        let mut header = String::new();
        self.stdout.read_line(&mut header).ok()?;
        let Some((typ, size)) = parse_header(&header) else {
            return None; // "missing"/"ambiguous": no payload follows
        };
        let is_blob = typ == "blob";

        // always consume payload + trailing LF to keep the stream in sync
        let mut buf = vec![0u8; size + 1];
        self.stdout.read_exact(&mut buf).ok()?;
        buf.pop();
        is_blob.then_some(buf)
    }
}

impl Drop for Blobs {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self._check_child.kill();
    }
}
