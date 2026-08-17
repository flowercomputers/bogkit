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

/// A same-transaction replacement whose old and new text SHARE terms must
/// leave the shared terms' postings correct — accumulating net deltas
/// wrote the difference as the stored frequency (or deleted the posting
/// outright), so the document vanished from queries for its own words.
#[test]
fn bm25_replacement_with_shared_terms_keeps_postings() {
    let path = fresh_db("bm25_shared_term_replacement");
    let mut st = KeyedStream::new(&path, terminal::search::Bm25::<u32, String>::new("idx"));
    st.wtx(|tx| {
        tx.upsert(&1u32, &"fold fold quick runs the".to_string());
    });
    // replacement in ONE tx: 'fold' tf drops 2 -> 1 (net -1 deleted the
    // posting under delta accumulation); 'the' tf stays 1 -> 1 (net 0)
    st.wtx(|tx| {
        tx.upsert(&1u32, &"fold slow sleeps the".to_string());
    });
    st.rtx(|idx| {
        let fold_hits: Vec<u32> = idx.search("fold", 10).into_iter().map(|h| h.val).collect();
        assert_eq!(fold_hits, vec![1], "shared term lost after replacement");
        let the_hits: Vec<u32> = idx.search("the", 10).into_iter().map(|h| h.val).collect();
        assert_eq!(the_hits, vec![1]);
        assert!(idx.search("quick", 10).is_empty(), "old-only term must be gone");
        assert!(!idx.search("slow", 10).is_empty(), "new-only term must be indexed");
    });
}
