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
fn bm25_zero_delta_push_is_a_no_op() {
    let mut st = Stream::new(fresh_db("bm25_zero.db"), terminal::search::Bm25::new("idx"));
    st.wtx(|tx| tx.insert(&Keyed::new(1u32, "a quick fox".to_string())));
    // delta == 0 must touch nothing — neither postings nor doc length
    st.wtx(|tx| tx.push(&Keyed::new(1u32, "a quick fox".to_string()), 0));
    st.rtx(|idx| {
        assert_eq!(idx.doc_count(), 1);
        assert_eq!(idx.search("fox", 10).len(), 1);
    });
}

// editing a document as retract-old + insert-new in ONE transaction — which
// is exactly what `KeyedStream::upsert` emits — must leave the index
// identical to one built directly with the final texts. Terms whose
// frequency CHANGES between old and new text (and every changed document
// length) are the trap: a net-delta store writes the difference as if it
// were the absolute value.
#[test]
fn bm25_same_txn_replace_matches_direct_build() {
    let finals: &[(u32, &str)] = &[
        (1, "the cat lay on the mat by the door"), // "the" tf 2->3, sat->lay, dl 6->9
        (2, "a quick brown fox"),                  // untouched
        (3, "rust systems"),                       // "rust" tf 3->1
    ];

    let mut edited = Stream::new(
        fresh_db("bm25_edit.db"),
        terminal::search::Bm25::new("idx"),
    );
    edited.wtx(|tx| {
        tx.insert(&Keyed::new(1u32, "the cat sat on the mat".to_string()));
        tx.insert(&Keyed::new(2u32, "a quick brown fox".to_string()));
        tx.insert(&Keyed::new(3u32, "rust rust rust systems".to_string()));
    });
    edited.wtx(|tx| {
        tx.remove(&Keyed::new(1u32, "the cat sat on the mat".to_string()));
        tx.insert(&Keyed::new(1u32, finals[0].1.to_string()));
        tx.remove(&Keyed::new(3u32, "rust rust rust systems".to_string()));
        tx.insert(&Keyed::new(3u32, finals[2].1.to_string()));
    });

    let mut direct = Stream::new(
        fresh_db("bm25_direct.db"),
        terminal::search::Bm25::new("idx"),
    );
    direct.wtx(|tx| {
        for (id, text) in finals {
            tx.insert(&Keyed::new(*id, text.to_string()));
        }
    });

    for q in ["the", "cat", "sat", "lay door", "rust", "fox", "the rust cat"] {
        let (a, b) = (
            edited.rtx(|idx| idx.search(q, 10)),
            direct.rtx(|idx| idx.search(q, 10)),
        );
        assert_eq!(
            a.iter().map(|h| (h.val, h.score)).collect::<Vec<_>>(),
            b.iter().map(|h| (h.val, h.score)).collect::<Vec<_>>(),
            "query {q:?} diverged after same-txn replace"
        );
    }
}
