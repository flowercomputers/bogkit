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
fn bm25_keyed_replacement_updates_postings_lengths_and_stats() {
    fn score(tf: f64, dl: f64, docs: f64, total_len: f64, df: f64) -> f64 {
        let k1 = 1.2;
        let b = 0.75;
        let idf = ((docs - df + 0.5) / (df + 0.5) + 1.0).ln();
        let norm = k1 * (1.0 - b + b * dl / (total_len / docs));
        idf * tf * (k1 + 1.0) / (tf + norm)
    }

    let mut st = KeyedStream::new(
        fresh_db("bm25_keyed_replacement.db"),
        terminal::search::Bm25::new("bm25_keyed_replacement"),
    );
    st.wtx(|tx| {
        tx.upsert(&1u32, &"kept kept removed filler filler filler".to_string());
        tx.upsert(&2, &"anchor filler filler filler".to_string());
    });

    st.wtx(|tx| {
        tx.upsert(&1, &"kept added filler filler filler".to_string());
    });
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 2);
        let expected = score(1.0, 5.0, 2.0, 9.0, 1.0);
        for term in ["kept", "added"] {
            let hits = idx.search(term, 10);
            assert_eq!(ids(&hits), vec![1]);
            assert!((hits[0].score - expected).abs() < 1e-12);
        }
        assert!(idx.search("removed", 10).is_empty());
    });

    st.wtx(|tx| {
        assert_eq!(
            tx.remove(&1),
            Some("kept added filler filler filler".to_string())
        );
    });
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 1);
        assert!(idx.search("kept", 10).is_empty());
        assert!(idx.search("added", 10).is_empty());
        assert_eq!(ids(&idx.search("anchor", 10)), vec![2]);
    });
}
