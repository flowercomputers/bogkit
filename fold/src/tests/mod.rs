use crate::{pipeline::*, stream::*};

use std::time::Instant;

#[cfg(test)]
mod bm25;

#[cfg(test)]
mod hnsw;

#[cfg(test)]
mod keyed_stream;

#[cfg(test)]
mod retain;

#[cfg(test)]
mod rtx;

#[cfg(test)]
mod scored;

#[cfg(test)]
mod terminals;

/// A path in the system temp dir, cleared of any previous test run's state.
pub(crate) fn fresh_db(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&path);
    path
}

#[test]
fn test_name() {
    let mut st = Stream::new(
        "zero.db",
        Filter::new(
            |v: &String| v.len() > 1,
            (
                Map::new(|v: &String| v.len(), terminal::Count::new("len_count")),
                terminal::Bag::new("raw"),
                Map::new(|v: &String| v.to_uppercase(), terminal::Bag::new("uppers")),
            ),
        ),
    );

    let iters = 10;

    let start = Instant::now();
    st.wtx(|tx| {
        for i in 0..iters {
            tx.push(&format!("{i}"), 1)
        }
    });
    let idur = start.elapsed();

    // one snapshot, all sinks consistent with each other
    st.rtx(|(count, raw, upper)| {
        println!("count: {}", count.get());
        println!("raw size: {}", raw.iter().count());
        assert!(upper.iter().all(|(s, _)| s == s.to_uppercase()));
    });

    let start = Instant::now();
    st.wtx(|tx| {
        for i in 0..iters {
            tx.push(&format!("{i}"), -1)
        }
    });
    let rdur = start.elapsed();

    st.rtx(|(count, raw, _)| {
        assert_eq!(count.get(), 0);
        assert_eq!(raw.iter().count(), 0);
    });

    println!(
        "in: {iters} in {idur:?} (avg: {:?})",
        idur.div_f64(iters as f64)
    );
    println!(
        "re: {iters} in {rdur:?} (avg: {:?})",
        rdur.div_f64(iters as f64)
    );
}
