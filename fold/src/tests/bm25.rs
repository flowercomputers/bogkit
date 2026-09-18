use crate::{pipeline::*, stream::*, tests::fresh_db};

fn ids(hits: &[Scored<f64, u32>]) -> Vec<u32> {
    hits.iter().map(|h| h.val).collect()
}

#[test]
fn bm25_rank_and_retract() {
    let docs: &[(u32, &str)] = &[
        (1, "the quick brown fox jumps over the lazy dog"),
        (2, "The Quick Brown Fox!"),
        (3, "rust is a systems programming language rust rust"),
    ];

    let mut st = Stream::new(fresh_db("bm25.db"), terminal::search::Bm25::new("bm25_idx"));

    st.wtx(|tx| {
        for (id, text) in docs {
            tx.insert(&Keyed::new(*id, text.to_string()));
        }
    });

    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 3);

        // both fox docs match; the shorter doc 2 outranks doc 1
        let hits = idx.search("fox", 10);
        assert_eq!(ids(&hits), vec![2, 1]);
        assert!(hits[0].score > hits[1].score);

        // query tokenization matches ingest tokenization
        assert_eq!(ids(&idx.search("FOX!!", 10)), vec![2, 1]);

        assert_eq!(ids(&idx.search("rust", 10)), vec![3]);
        assert!(idx.search("zzzzzz", 10).is_empty());
        assert!(idx.search("", 10).is_empty());

        // rarer term with higher tf dominates: doc 3 tops a mixed query
        let hits = idx.search("rust fox", 10);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].val, 3);

        assert_eq!(idx.search("the quick", 1).len(), 1);
    });

    // insert + remove within one tx cancels before hitting the store
    st.wtx(|tx| {
        let d = Keyed::new(4u32, "ephemeral fox".to_string());
        tx.insert(&d);
        tx.remove(&d);
    });
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 3);
        assert_eq!(ids(&idx.search("fox", 10)), vec![2, 1]);
    });

    // retracting a doc removes it from results and corpus stats
    st.wtx(|tx| tx.remove(&Keyed::new(2u32, docs[1].1.to_string())));
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 2);
        assert_eq!(ids(&idx.search("fox", 10)), vec![1]);
    });

    st.wtx(|tx| {
        tx.remove(&Keyed::new(1u32, docs[0].1.to_string()));
        tx.remove(&Keyed::new(3u32, docs[2].1.to_string()));
    });
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 0);
        assert!(idx.search("fox", 10).is_empty());
        assert!(idx.search("rust", 10).is_empty());
    });
}

#[test]
fn bm25_keyed_replacement_matches_fresh_corpus() {
    let path = fresh_db("bm25_keyed.db");
    let mut st = KeyedStream::new(&path, terminal::search::Bm25::new("idx"));
    st.wtx(|tx| {
        tx.upsert(&1u32, &"fox fox fox old".to_string());
        tx.upsert(&2, &"fox anchor".to_string());
    });
    st.wtx(|tx| {
        tx.upsert(&1, &"fox new".to_string());
    });
    let expected = {
        let mut fresh = KeyedStream::new(
            fresh_db("bm25_reference.db"),
            terminal::search::Bm25::new("idx"),
        );
        fresh.wtx(|tx| {
            tx.upsert(&1u32, &"fox new".to_string());
            tx.upsert(&2, &"fox anchor".to_string());
        });
        fresh.rtx(|idx| {
            idx.search("fox new", 10)
                .into_iter()
                .map(|h| (h.val, h.score))
                .collect::<Vec<_>>()
        })
    };
    let check = |hits: Vec<Scored<f64, u32>>| {
        let actual: Vec<_> = hits.into_iter().map(|h| (h.val, h.score)).collect();
        assert_eq!(actual, expected);
    };
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 2);
        assert!(idx.search("old", 10).is_empty());
        check(idx.search("fox new", 10));
    });
    st.wtx(|tx| {
        tx.upsert(&1, &"temporary temporary".to_string());
        tx.upsert(&1, &"fox new".to_string());
        tx.upsert(&3, &"ephemeral".to_string());
        tx.remove(&3);
    });
    st.rtx(|idx| check(idx.search("fox new", 10)));
    let aborted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        st.wtx(|tx| {
            tx.upsert(&1, &"aborted".to_string());
            tx.rtx(|idx| assert_eq!(ids(&idx.search("aborted", 10)), vec![1]));
            panic!("abort replacement");
        });
    }));
    assert!(aborted.is_err());
    st.rtx(|idx| check(idx.search("fox new", 10)));
    drop(st);
    let mut st = KeyedStream::<u32, String, _>::new(&path, terminal::search::Bm25::new("idx"));
    st.rtx(|idx| check(idx.search("fox new", 10)));
    st.wtx(|tx| {
        tx.upsert(&1, &"".to_string());
    });
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 2);
        assert_eq!(ids(&idx.search("fox", 10)), vec![2]);
    });
    st.wtx(|tx| {
        tx.remove(&1);
        tx.remove(&2);
    });
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 0);
        assert!(idx.search("fox", 10).is_empty());
    });
}
