use crate::{pipeline::*, stream::*, tests::fresh_db};
use anny::metric::{Cosine, L2};

type Sink = terminal::search::Flat<u32, f32, L2, 4>;
type HnswSink = terminal::search::Hnsw<u32, f32, L2, 4>;

fn ids(hits: &[Scored<f32, u32>]) -> Vec<u32> {
    hits.iter().map(|h| h.val).collect()
}

#[test]
fn flat_nearest_upsert_retract() {
    let path = fresh_db("flat.db");
    let mut st = Stream::new(&path, Sink::new("vecs", L2));

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

    // retraction by key, with a value the caller no longer reproduces
    // exactly: must still delete (embeddings may be recomputed)
    st.wtx(|tx| tx.remove(&Keyed::new(2, [1.5, 0.0, 0.0, 0.0])));
    st.rtx(|idx| {
        assert_eq!(idx.len(), 2);
        assert_eq!(ids(&idx.search(&[0.1, 0.0, 0.0, 0.0])), vec![3, 1]);
    });

    // insert + retract within one tx nets out: no row appears
    st.wtx(|tx| {
        tx.insert(&Keyed::new(9, [5.0, 5.0, 5.0, 5.0]));
        tx.remove(&Keyed::new(9, [5.0, 5.0, 5.0, 5.0]));
    });
    st.rtx(|idx| assert_eq!(idx.len(), 2));

    // replacement inside one tx (retract old, insert new) reaches the store
    st.wtx(|tx| {
        tx.remove(&Keyed::new(1, [20.0, 20.0, 20.0, 20.0]));
        tx.insert(&Keyed::new(1, [0.0, 0.0, 0.0, 1.0]));
    });
    st.rtx(|idx| {
        assert_eq!(idx.len(), 2);
        assert_eq!(ids(&idx.search(&[0.0, 0.0, 0.0, 1.0]))[0], 1);
    });
}

#[test]
fn flat_top_k_is_bounded_and_tie_broken_by_key() {
    type Small = terminal::search::Flat<u32, f32, L2, 2, 3>;
    let path = fresh_db("flat-topk.db");
    let mut st = Stream::new(&path, Small::new("vecs", L2));
    st.wtx(|tx| {
        // five identical points: every distance ties
        for k in [40u32, 10, 30, 20, 50] {
            tx.insert(&Keyed::new(k, [1.0, 1.0]));
        }
    });
    st.rtx(|idx| {
        let hits = idx.search(&[1.0, 1.0]);
        assert_eq!(hits.len(), 3, "TOP_K bounds the result");
        // postcard(u32) is a varint, so byte order is numeric order here
        assert_eq!(ids(&hits), vec![10, 20, 30], "ties resolve by encoded key");
    });
}

#[test]
fn flat_reads_rows_hnsw_wrote() {
    // the swap story: a pipeline retires Hnsw and adopts its keyspace as-is
    let path = fresh_db("flat-compat.db");
    {
        let mut st = Stream::new(&path, HnswSink::new("vecs", L2, 42));
        st.wtx(|tx| {
            tx.insert(&Keyed::new(1, [0.0, 0.0, 0.0, 0.0]));
            tx.insert(&Keyed::new(2, [1.0, 0.0, 0.0, 0.0]));
            tx.insert(&Keyed::new(3, [10.0, 10.0, 10.0, 10.0]));
        });
    }
    let mut st = Stream::new(&path, Sink::new("vecs", L2));
    st.rtx(|idx| {
        assert_eq!(idx.len(), 3);
        assert_eq!(ids(&idx.search(&[0.1, 0.0, 0.0, 0.0])), vec![1, 2, 3]);
    });
    // and the adopted rows keep behaving: delete one Hnsw wrote
    st.wtx(|tx| tx.remove(&Keyed::new(1, [0.0, 0.0, 0.0, 0.0])));
    st.rtx(|idx| assert_eq!(ids(&idx.search(&[0.1, 0.0, 0.0, 0.0])), vec![2, 3]));
}

#[test]
fn flat_is_deterministic_across_opens() {
    let path = fresh_db("flat-det.db");
    let points: Vec<[f32; 4]> = (0..64)
        .map(|i| {
            let f = i as f32;
            [f.sin(), (f * 0.7).cos(), (f * 1.3).sin(), 0.5]
        })
        .collect();
    let first = {
        let mut st = Stream::new(
            &path,
            terminal::search::Flat::<u32, f32, Cosine, 4>::new("vecs", Cosine),
        );
        st.wtx(|tx| {
            for (i, p) in points.iter().enumerate() {
                tx.insert(&Keyed::new(i as u32, *p));
            }
        });
        st.rtx(|idx| ids(&idx.search(&[0.3, 0.2, 0.9, 0.5])))
    };
    let second = {
        let st = Stream::new(
            &path,
            terminal::search::Flat::<u32, f32, Cosine, 4>::new("vecs", Cosine),
        );
        st.rtx(|idx| ids(&idx.search(&[0.3, 0.2, 0.9, 0.5])))
    };
    assert_eq!(first, second);
    assert_eq!(first.len(), 10);
}
