//! The chronicle: an append-only temporal view over ALL of history, built
//! once, scrubbed with zero writes.
//!
//! Every file version becomes a `Keyed<path, Scored<birth_commit, oid>>`
//! record in a [`KeyedRanked`] sink, whose order-preserving score encoding
//! keeps each path's versions in one contiguous score-ordered run — so
//! "this file as of commit T" is a single bounded seek
//! (`range(&path, ..=T).next_back()`), at any repo size. A deletion is a
//! version whose oid is `None`. Contents are not stored at all: the value
//! is a git blob oid, and git is already the content-addressed store.
//!
//! Per-commit stats (file count, line count, per-extension counts) are
//! precomputed into a `Table<commit_idx, StatsRow>`: as-of-T stats are one
//! point read. This is Bog's thesis applied to the time axis — all the
//! work at ingest, reads for free.
//!
//! The pipeline uses plain `fn` items instead of closures, which makes the
//! whole pipeline type nameable — so [`Chronicle`] is an ordinary struct
//! other modules can hold and query.

use std::collections::HashMap;
use std::path::Path;

use fold::pipeline::{FilterMap, Keyed, Scored, terminal};
use fold::stream::Stream;
use serde::{Deserialize, Serialize};

use crate::git;

/// One record of history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Rec {
    /// `path` acquired a new content (`oid = Some`) or was deleted /
    /// stopped being indexable (`oid = None`) at commit `at`.
    Version {
        path: String,
        at: u32,
        oid: Option<String>,
    },
    /// The full stats header as of commit `at`.
    Stats { at: u32, row: StatsRow },
    /// Build completed with `head` as the newest ingested commit.
    Done { head: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StatsRow {
    pub files: i64,
    pub lines: i64,
    pub by_ext: Vec<(String, i64)>,
}

type VersionRec = Keyed<String, Scored<u32, Option<String>>>;
type StatsRec = Keyed<u32, StatsRow>;
type MetaRec = Keyed<u8, String>;

fn version_rec(r: &Rec) -> Option<VersionRec> {
    match r {
        Rec::Version { path, at, oid } => {
            Some(Keyed::new(path.clone(), Scored::new(*at, oid.clone())))
        }
        _ => None,
    }
}
fn stats_rec(r: &Rec) -> Option<StatsRec> {
    match r {
        Rec::Stats { at, row } => Some(Keyed::new(*at, row.clone())),
        _ => None,
    }
}
fn meta_rec(r: &Rec) -> Option<MetaRec> {
    match r {
        Rec::Done { head } => Some(Keyed::new(0u8, head.clone())),
        _ => None,
    }
}

type Pipe = (
    FilterMap<
        fn(&Rec) -> Option<VersionRec>,
        terminal::KeyedRanked<String, u32, Option<String>>,
        Rec,
        VersionRec,
    >,
    FilterMap<fn(&Rec) -> Option<StatsRec>, terminal::Table<u32, StatsRow>, Rec, StatsRec>,
    FilterMap<fn(&Rec) -> Option<MetaRec>, terminal::Table<u8, String>, Rec, MetaRec>,
);

fn pipe() -> Pipe {
    (
        FilterMap::new(
            version_rec as fn(&Rec) -> Option<VersionRec>,
            terminal::KeyedRanked::new("versions"),
        ),
        FilterMap::new(
            stats_rec as fn(&Rec) -> Option<StatsRec>,
            terminal::Table::new("stats_at"),
        ),
        FilterMap::new(
            meta_rec as fn(&Rec) -> Option<MetaRec>,
            terminal::Table::new("meta"),
        ),
    )
}

pub struct Chronicle {
    st: Stream<Rec, Pipe>,
}

impl Chronicle {
    pub fn open(path: &Path) -> Chronicle {
        Chronicle {
            st: Stream::new(path, pipe()),
        }
    }

    /// The head oid this chronicle was completely built for, if any.
    pub fn built_for(&self) -> Option<String> {
        self.st.rtx(|(_, _, meta)| meta.get(&0u8))
    }

    /// The stats header as of commit `t`: one point read.
    pub fn stats_at(&self, t: u32) -> Option<StatsRow> {
        self.st.rtx(|(_, stats, _)| stats.get(&t))
    }

    /// The blob oid of `path` as of commit `t`: one bounded seek.
    /// `None` = never existed by `t`; `Some(None)` = deleted at `t`.
    pub fn oid_at(&self, path: &str, t: u32) -> Option<Option<String>> {
        self.st.rtx(|(versions, _, _)| {
            versions
                .range(&path.to_string(), ..=t)
                .next_back()
                .map(|(s, _)| s.val)
        })
    }
}

/// Build (or resume) the chronicle for `commits` at `db_path`. Returns
/// early if a completed build for the current head already exists —
/// reopening is milliseconds, so restarts are free.
pub fn build(
    repo: &Path,
    commits: &[git::Commit],
    db_path: &Path,
    empty_tree: &str,
) -> Chronicle {
    let head_oid = &commits[commits.len() - 1].oid;
    {
        let existing = Chronicle::open(db_path);
        if existing.built_for().as_deref() == Some(head_oid) {
            eprintln!("seance: chronicle already built for {}", &head_oid[..10]);
            return existing;
        }
        // partial or outdated: rebuild from scratch (drop closes the store)
    }
    let _ = std::fs::remove_dir_all(db_path);
    let mut ch = Chronicle::open(db_path);
    let mut blobs = git::Blobs::new(repo);
    let started = std::time::Instant::now();

    // one `git log` spawn per chunk fetches every commit's changed paths
    let mut changes_by_oid: HashMap<String, Vec<String>> = HashMap::new();
    const CHUNK: usize = 5000;
    let mut lo = 0;
    while lo + 1 < commits.len() {
        let hi = (lo + CHUNK).min(commits.len() - 1);
        for (oid, paths) in git::first_parent_changes(repo, &commits[lo].oid, &commits[hi].oid) {
            changes_by_oid.insert(oid, paths);
        }
        lo = hi;
    }

    // running snapshot state: path -> line count (of the capped text)
    let mut lines_of: HashMap<String, i64> = HashMap::new();
    let mut by_ext: HashMap<String, i64> = HashMap::new();
    let mut total_lines: i64 = 0;

    let root_paths: Vec<String> = git::diff(repo, empty_tree, &commits[0].oid)
        .into_iter()
        .map(|(_, p)| p)
        .collect();

    const TX_RECORDS: usize = 4000;
    let mut pending: Vec<Rec> = Vec::with_capacity(TX_RECORDS + 64);
    let mut versions_total = 0usize;

    for (i, commit) in commits.iter().enumerate() {
        let paths = if i == 0 {
            &root_paths
        } else {
            match changes_by_oid.get(&commit.oid) {
                Some(p) => p,
                None => continue, // no changes recorded (empty commit)
            }
        };

        for path in paths {
            let meta = blobs.blob_meta(&format!("{}:{}", commit.oid, path));
            let indexed = match meta {
                Some((oid, size)) if size <= crate::MAX_BLOB_BYTES => {
                    // read for the line count; skip binaries like the live store
                    match blobs.read(&oid) {
                        Some(bytes) if !bytes.contains(&0) => {
                            let text = String::from_utf8_lossy(&bytes);
                            let lines = crate::cap(&text, crate::INDEX_CAP_BYTES).lines().count() as i64;
                            Some((oid, lines))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };

            let ext = crate::ext_of(path);
            if let Some(old_lines) = lines_of.remove(path) {
                total_lines -= old_lines;
                *by_ext.entry(ext.clone()).or_insert(0) -= 1;
            }
            let oid = match indexed {
                Some((oid, lines)) => {
                    total_lines += lines;
                    *by_ext.entry(ext).or_insert(0) += 1;
                    lines_of.insert(path.clone(), lines);
                    Some(oid)
                }
                None => None,
            };
            versions_total += 1;
            pending.push(Rec::Version {
                path: path.clone(),
                at: i as u32,
                oid,
            });
        }

        let mut exts: Vec<(String, i64)> = by_ext
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(e, n)| (e.clone(), *n))
            .collect();
        exts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        pending.push(Rec::Stats {
            at: i as u32,
            row: StatsRow {
                files: lines_of.len() as i64,
                lines: total_lines,
                by_ext: exts,
            },
        });

        if pending.len() >= TX_RECORDS {
            let batch = std::mem::take(&mut pending);
            ch.st.wtx(|tx| {
                for rec in &batch {
                    tx.insert(rec);
                }
            });
        }
        if i % 2000 == 0 && i > 0 {
            let ms = started.elapsed().as_millis();
            eprintln!(
                "seance: chronicle {i}/{} commits, {versions_total} versions, {ms} ms ({:.0} commits/s)",
                commits.len(),
                i as f64 / (ms as f64 / 1000.0)
            );
        }
    }

    pending.push(Rec::Done {
        head: head_oid.clone(),
    });
    let batch = pending;
    ch.st.wtx(|tx| {
        for rec in &batch {
            tx.insert(rec);
        }
    });
    ch.st.checkpoint();
    eprintln!(
        "seance: chronicle built — {} commits, {versions_total} versions, {} ms",
        commits.len(),
        started.elapsed().as_millis()
    );
    ch
}
