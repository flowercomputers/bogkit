//! Standalone check for the checkpoint-clone design: does a copied fjall db
//! directory open as a valid, independent database with identical
//! materialized state? Also measures the open cost that a teleport-to-
//! checkpoint would pay.
//!
//! Usage: cargo run -p seance --bin clone_check -- <db-dir>
//! Machine-readable JSON on stdout; compare two runs (original vs clone).

use fjall::Readable;

fn main() {
    let path = std::env::args().nth(1).expect("usage: clone_check <db-dir>");

    let t = std::time::Instant::now();
    let db = fjall::SingleWriterTxDatabase::builder(&path).open().unwrap();
    let open_ms = t.elapsed().as_millis();

    let count_ks = db
        .keyspace("sink_file_count", fjall::KeyspaceCreateOptions::default)
        .unwrap();
    let vec_ks = db
        .keyspace("sink_vecs", fjall::KeyspaceCreateOptions::default)
        .unwrap();

    let tx = db.read_tx();
    let files = tx
        .get(&count_ks, b"\0")
        .unwrap()
        .map(|v| i64::from_be_bytes(v.as_ref().try_into().unwrap()))
        .unwrap_or(0);

    // scan every persisted embedding: this is exactly the input the HNSW
    // sink's rebuild-on-open reads, so scan time is the I/O floor of an
    // open today (graph construction cost comes on top)
    let t = std::time::Instant::now();
    let (mut vectors, mut vector_bytes, mut key_hash) = (0usize, 0usize, 0u64);
    for kv in tx.iter(&vec_ks) {
        let (k, v) = kv.into_inner().unwrap();
        vectors += 1;
        vector_bytes += v.len();
        key_hash = key_hash.wrapping_mul(31).wrapping_add(fxhash_bytes(&k));
    }
    let scan_ms = t.elapsed().as_millis();

    println!(
        "{{\"open_ms\":{open_ms},\"files\":{files},\"vectors\":{vectors},\"vector_bytes\":{vector_bytes},\"key_hash\":{key_hash},\"vector_scan_ms\":{scan_ms}}}"
    );
}

fn fxhash_bytes(b: &[u8]) -> u64 {
    b.iter()
        .fold(0u64, |h, &x| h.wrapping_mul(0x100000001b3).wrapping_add(x as u64))
}
