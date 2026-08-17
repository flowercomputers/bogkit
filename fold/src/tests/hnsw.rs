use crate::{pipeline::*, stream::*, tests::fresh_db};
use anny::metric::L2;

type Sink = terminal::search::Hnsw<u32, f32, L2, 4>;

fn ids(hits: &[Scored<f32, u32>]) -> Vec<u32> {
    hits.iter().map(|h| h.val).collect()
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

/// A replacement inside one transaction must reach the vector index.
///
/// `KeyedStream::upsert` retracts the old record and inserts the new one in
/// the same transaction. With pending work keyed only by `K`, that -1/+1 pair
/// nets to zero and the old vector survives in both the graph and the store —
/// while the same-value insert+retract pair (above) must still net out.
/// Pending is therefore keyed by (key, value), the `Bm25` posting discipline.
#[test]
fn hnsw_replacement_within_one_transaction_updates_the_vector() {
    let path = fresh_db("hnsw-intra-tx-replace.db");
    let mut st = KeyedStream::new(
        &path,
        Map::new(
            |d: &Keyed<u32, [f32; 4]>| Keyed::new(d.key, d.val),
            Sink::new("vecs", L2, 42),
        ),
    );

    st.wtx(|tx| {
        tx.upsert(&1u32, &[0.0f32, 0.0, 0.0, 0.0]);
        tx.upsert(&2u32, &[10.0f32, 10.0, 10.0, 10.0]);
    });
    // the defect: this upsert emits remove(old) + insert(new) in ONE tx
    st.wtx(|tx| {
        tx.upsert(&1u32, &[20.0f32, 20.0, 20.0, 20.0]);
    });

    st.rtx(|idx| {
        assert_eq!(idx.len(), 2);
        // key 1 must be found at its new position, not its old one
        assert_eq!(ids(&idx.search(&[20.0, 20.0, 20.0, 20.0]))[0], 1);
        assert_eq!(ids(&idx.search(&[0.1, 0.0, 0.0, 0.0]))[0], 2);
    });

    // and the store agrees after a reopen (graph rebuilt from vectors)
    drop(st);
    let st = KeyedStream::new(
        &path,
        Map::new(
            |d: &Keyed<u32, [f32; 4]>| Keyed::new(d.key, d.val),
            Sink::new("vecs", L2, 42),
        ),
    );
    st.rtx(|idx| {
        assert_eq!(ids(&idx.search(&[20.0, 20.0, 20.0, 20.0]))[0], 1);
    });
}
