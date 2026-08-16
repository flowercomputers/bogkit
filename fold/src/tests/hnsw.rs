use crate::{pipeline::*, stream::*, tests::fresh_db};
use anny::metric::L2;
use std::time::Instant;

type Sink = terminal::search::Hnsw<u32, f32, L2, 4>;

fn ids(hits: &[Scored<f32, u32>]) -> Vec<u32> {
    hits.iter().map(|h| h.val).collect()
}

// distinct first coordinate => querying dv(i) has a unique zero-distance hit
fn dv(i: u32) -> [f32; 4] {
    [i as f32, (i % 7) as f32, ((i / 3) % 5) as f32, 0.0]
}

#[test]
fn hnsw_nearest_upsert_retract_recover() {
    let path = fresh_db("hnsw.db");
    let mut st = Stream::new(&path, Sink::new("vecs", L2, 42));

    st.wtx(|tx| {
        tx.insert(&Keyed::new(1, [0.0, 0.0, 0.0, 0.0]));
        tx.insert(&Keyed::new(2, [1.0, 0.0, 0.0, 0.0]));
        tx.insert(&Keyed::new(3, [10.0, 10.0, 10.0, 10.0]));
    });

    st.rtx(|idx| {
        assert_eq!(idx.len(), 3);
        let hits = idx.search(&[0.1, 0.0, 0.0, 0.0]);
        assert_eq!(ids(&hits), vec![1, 2, 3]);
        assert!(hits.windows(2).all(|w| w[0].score <= w[1].score));
        assert_eq!(ids(&idx.search(&[9.0, 9.0, 9.0, 9.0]))[0], 3);
    });

    // upsert moves key 1 across the space; it must leave its old spot
    st.wtx(|tx| tx.insert(&Keyed::new(1, [20.0, 20.0, 20.0, 20.0])));
    st.rtx(|idx| {
        assert_eq!(idx.len(), 3);
        assert_eq!(ids(&idx.search(&[0.1, 0.0, 0.0, 0.0]))[0], 2);
        assert_eq!(ids(&idx.search(&[20.0, 20.0, 20.0, 20.0]))[0], 1);
    });

    // retraction removes the node; the neighborhood heals
    st.wtx(|tx| tx.remove(&Keyed::new(2, [1.0, 0.0, 0.0, 0.0])));
    st.rtx(|idx| {
        assert_eq!(idx.len(), 2);
        assert_eq!(ids(&idx.search(&[0.1, 0.0, 0.0, 0.0])), vec![3, 1]);
    });

    // insert + retract within one tx nets out before touching the graph
    st.wtx(|tx| {
        let d = Keyed::new(4, [5.0, 5.0, 5.0, 5.0]);
        tx.insert(&d);
        tx.remove(&d);
    });
    st.rtx(|idx| assert_eq!(idx.len(), 2));

    // a panicking tx after a mid-tx flush marks the graph stale; the next
    // read rebuilds it from committed state
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        st.wtx(|tx| {
            tx.insert(&Keyed::new(9, [5.0, 5.0, 5.0, 5.0]));
            tx.rtx(|idx| assert_eq!(ids(&idx.search(&[5.0, 5.0, 5.0, 5.0]))[0], 9));
            panic!("abort");
        });
    }));
    assert!(r.is_err());
    st.rtx(|idx| {
        assert_eq!(idx.len(), 2);
        assert!(
            ids(&idx.search(&[5.0, 5.0, 5.0, 5.0]))
                .iter()
                .all(|&k| k != 9)
        );
    });

    // reopening rebuilds the graph from the persisted vectors
    drop(st);
    let st = Stream::new(&path, Sink::new("vecs", L2, 42));
    st.rtx(|idx| {
        assert_eq!(idx.len(), 2);
        assert_eq!(ids(&idx.search(&[20.0, 20.0, 20.0, 20.0]))[0], 1);
        assert_eq!(ids(&idx.search(&[9.0, 9.0, 9.0, 9.0]))[0], 3);
    });
}

#[test]
fn hnsw_graph_snapshot_fast_reopen() {
    let path = fresh_db("hnsw_snap.db");
    // a foreign non-.jnl file at the db-dir root is ignored by fjall recovery
    let graph = path.join("hnsw.graph");
    let n: u32 = 400;

    let mut st = Stream::new(&path, Sink::new("vecs", L2, 42).with_graph_snapshot(&graph));
    st.wtx(|tx| {
        for i in 0..n {
            tx.insert(&Keyed::new(i, dv(i)));
        }
    });

    let queries: Vec<[f32; 4]> = (0..25).map(|i| dv(i * 17 % n)).collect();
    let baseline: Vec<Vec<u32>> = st.rtx(|idx| {
        idx.save_graph().unwrap();
        queries.iter().map(|q| ids(&idx.search(q))).collect()
    });
    drop(st);

    // the restored graph is the saved graph: results match exactly
    let t = Instant::now();
    let st = Stream::new(&path, Sink::new("vecs", L2, 42).with_graph_snapshot(&graph));
    eprintln!("snapshot reopen: {:?}", t.elapsed());
    st.rtx(|idx| {
        assert_eq!(idx.len(), n as usize);
        for (q, want) in queries.iter().zip(&baseline) {
            assert_eq!(&ids(&idx.search(q)), want);
        }
    });
    drop(st);

    let t = Instant::now();
    let st = Stream::new(&path, Sink::new("vecs", L2, 42));
    eprintln!("rebuild reopen:  {:?}", t.elapsed());
    st.rtx(|idx| assert_eq!(idx.len(), n as usize));
}

#[test]
fn hnsw_graph_snapshot_corrupt_falls_back() {
    let path = fresh_db("hnsw_snap_corrupt.db");
    let graph = path.join("hnsw.graph");
    let n: u32 = 120;

    let mut st = Stream::new(&path, Sink::new("vecs", L2, 7).with_graph_snapshot(&graph));
    st.wtx(|tx| {
        for i in 0..n {
            tx.insert(&Keyed::new(i, dv(i)));
        }
    });
    st.rtx(|idx| idx.save_graph().unwrap());
    drop(st);

    // truncated blob: load fails, init rebuilds from the committed rows
    let len = std::fs::metadata(&graph).unwrap().len();
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&graph)
        .unwrap();
    f.set_len(len / 2).unwrap();
    drop(f);
    let st = Stream::new(&path, Sink::new("vecs", L2, 7).with_graph_snapshot(&graph));
    st.rtx(|idx| {
        assert_eq!(idx.len(), n as usize);
        for i in [0u32, 17, 63, 119] {
            assert_eq!(ids(&idx.search(&dv(i)))[0], i);
        }
    });
    st.rtx(|idx| idx.save_graph().unwrap());
    drop(st);

    // corrupted header: same fallback
    let mut blob = std::fs::read(&graph).unwrap();
    blob[3] ^= 0xFF;
    std::fs::write(&graph, &blob).unwrap();
    let st = Stream::new(&path, Sink::new("vecs", L2, 7).with_graph_snapshot(&graph));
    st.rtx(|idx| {
        assert_eq!(idx.len(), n as usize);
        assert_eq!(ids(&idx.search(&dv(63)))[0], 63);
    });
}

#[test]
fn hnsw_graph_snapshot_stale_rejected() {
    let path = fresh_db("hnsw_snap_stale.db");
    let graph = path.join("hnsw.graph");

    let mut st = Stream::new(&path, Sink::new("vecs", L2, 3).with_graph_snapshot(&graph));
    st.wtx(|tx| {
        for i in 0..50u32 {
            tx.insert(&Keyed::new(i, dv(i)));
        }
    });
    st.rtx(|idx| idx.save_graph().unwrap());
    // more committed rows leave the blob behind; drop without re-saving
    st.wtx(|tx| {
        for i in 50..80u32 {
            tx.insert(&Keyed::new(i, dv(i)));
        }
    });
    drop(st);

    // validation sees 80 rows vs 50 blob entries and rebuilds instead
    let st = Stream::new(&path, Sink::new("vecs", L2, 3).with_graph_snapshot(&graph));
    st.rtx(|idx| {
        assert_eq!(idx.len(), 80);
        assert_eq!(ids(&idx.search(&dv(70)))[0], 70);
        assert_eq!(ids(&idx.search(&dv(10)))[0], 10);
    });
}
