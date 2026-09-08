//! The SQLite contender, built the way a careful SQLite user would build
//! it in 2026: FTS5 for keyword search, the native sqlite-vec extension
//! (vec0, cosine) for vector search, WAL with synchronous=NORMAL, prepared
//! statements throughout, one transaction per batch.
//!
//! ese embeds documents at insert time and queries at query time, exactly
//! as on the bog side, so embedding cost is charged symmetrically.

use std::path::{Path, PathBuf};
use std::sync::Once;

use rusqlite::Connection;

use crate::bog::DIM;
use crate::rrf;

static REGISTER_VEC: Once = Once::new();

pub struct Sqlite {
    conn: Connection,
    path: PathBuf,
}

impl Sqlite {
    pub fn open(path: &Path) -> Self {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        REGISTER_VEC.call_once(|| unsafe {
            type InitFn = unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut std::ffi::c_char,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> std::ffi::c_int;
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), InitFn>(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        });
        let conn = Connection::open(path).expect("open sqlite db");
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "synchronous", "NORMAL").unwrap();
        // same cache budget as the bog side; negative = KiB units
        conn.pragma_update(
            None,
            "cache_size",
            -((crate::CACHE_BYTES / 1024) as i64),
        )
        .unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE docs(id INTEGER PRIMARY KEY, text TEXT NOT NULL, nchars INTEGER NOT NULL);
             CREATE VIRTUAL TABLE fts USING fts5(text);
             CREATE VIRTUAL TABLE vec USING vec0(embedding float[{DIM}] distance_metric=cosine);"
        ))
        .expect("create schema");
        Sqlite {
            conn,
            path: path.to_path_buf(),
        }
    }

    /// One transaction: upserts (delete-then-insert across all three
    /// tables, mirroring bog's retract-then-insert) and removes.
    pub fn apply(&mut self, upserts: &[(u64, String)], removes: &[u64]) {
        let tx = self.conn.transaction().unwrap();
        {
            let mut del_docs = tx.prepare_cached("DELETE FROM docs WHERE id = ?1").unwrap();
            let mut del_fts = tx.prepare_cached("DELETE FROM fts WHERE rowid = ?1").unwrap();
            let mut del_vec = tx.prepare_cached("DELETE FROM vec WHERE rowid = ?1").unwrap();
            let mut ins_docs = tx
                .prepare_cached("INSERT INTO docs(id, text, nchars) VALUES (?1, ?2, ?3)")
                .unwrap();
            let mut ins_fts = tx
                .prepare_cached("INSERT INTO fts(rowid, text) VALUES (?1, ?2)")
                .unwrap();
            let mut ins_vec = tx
                .prepare_cached("INSERT INTO vec(rowid, embedding) VALUES (?1, ?2)")
                .unwrap();
            for (id, text) in upserts {
                let id = *id as i64;
                del_docs.execute((id,)).unwrap();
                del_fts.execute((id,)).unwrap();
                del_vec.execute((id,)).unwrap();
                ins_docs.execute((id, text, text.len() as i64)).unwrap();
                ins_fts.execute((id, text)).unwrap();
                ins_vec
                    .execute((id, blob(&ese::encode_single(text))))
                    .unwrap();
            }
            for id in removes {
                let id = *id as i64;
                del_docs.execute((id,)).unwrap();
                del_fts.execute((id,)).unwrap();
                del_vec.execute((id,)).unwrap();
            }
        }
        tx.commit().unwrap();
    }

    pub fn keyword(&self, query: &str, k: usize) -> Vec<u64> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT rowid FROM fts WHERE fts MATCH ?1 ORDER BY rank LIMIT ?2")
            .unwrap();
        stmt.query_map((fts_query(query), k as i64), |r| r.get::<_, i64>(0))
            .unwrap()
            .map(|r| r.unwrap() as u64)
            .collect()
    }

    /// Timed end to end by the harness: includes embedding the query.
    pub fn semantic(&self, query: &str, k: usize) -> Vec<u64> {
        let q = blob(&ese::encode_single(query));
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT rowid FROM vec WHERE embedding MATCH ?1 AND k = ?2 ORDER BY distance",
            )
            .unwrap();
        stmt.query_map((q, k as i64), |r| r.get::<_, i64>(0))
            .unwrap()
            .map(|r| r.unwrap() as u64)
            .collect()
    }

    /// Same product operation as bog's: embed, FTS5 top-k, vec top-k, fuse.
    pub fn hybrid(&self, query: &str, k: usize) -> Vec<u64> {
        let kw = self.keyword(query, k);
        let sem = self.semantic(query, k);
        rrf(&kw, &sem, k)
    }

    /// (live docs, mean length) — computed by SQL aggregate at read time.
    pub fn stats(&self) -> (i64, f64) {
        self.conn
            .prepare_cached("SELECT COUNT(*), COALESCE(AVG(nchars), 0.0) FROM docs")
            .unwrap()
            .query_row((), |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
    }

    pub fn disk_bytes(&self) -> u64 {
        ["", "-wal", "-shm"]
            .iter()
            .filter_map(|s| std::fs::metadata(format!("{}{s}", self.path.display())).ok())
            .map(|md| md.len())
            .sum()
    }
}

/// OR-of-terms, each quoted — the same query shape fold's Bm25 runs.
fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn blob(emb: &[f32; DIM]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DIM * 4);
    for f in emb {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}
