use crate::{pipeline::*, stream::*, tests::fresh_db};

#[test]
fn rtx_in_wtx() {
    let mut st = Stream::new(
        fresh_db("rtx.db"),
        Distinct::new(
            "distinct",
            (terminal::Count::new("count"), terminal::Bag::new("bag")),
        ),
    );

    st.wtx(|tx| {
        tx.insert(&1u32);
        tx.insert(&1u32); // duplicate, buffered in Distinct
        tx.insert(&2u32);

        // mid-tx read observes everything pushed so far
        tx.rtx(|(count, bag)| {
            assert_eq!(count.get(), 2);
            assert!(bag.contains(&1));
            assert!(bag.contains(&2));
            assert!(!bag.contains(&3));
        });

        // pushes resume after the read
        tx.insert(&3u32);
        tx.remove(&1u32); // one of two copies: still distinct-present

        tx.rtx(|(count, bag)| {
            assert_eq!(count.get(), 3);
            assert!(bag.contains(&1));
            assert!(bag.contains(&3));
        });
    });

    // committed state matches the last mid-tx view
    st.rtx(|(count, bag)| {
        assert_eq!(count.get(), 3);
        assert!(bag.contains(&1));
        assert!(bag.contains(&2));
        assert!(bag.contains(&3));
    });

    // a panicking tx rolls back everything a mid-tx read observed
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        st.wtx(|tx| {
            tx.insert(&4u32);
            tx.rtx(|(count, _)| assert_eq!(count.get(), 4));
            panic!("abort");
        });
    }));
    assert!(r.is_err());
    st.rtx(|(count, bag)| {
        assert_eq!(count.get(), 3);
        assert!(!bag.contains(&4));
    });
}

#[test]
fn final_flush_panic_discards_pending_pipeline_state() {
    const PANIC_PAYLOAD: &str = "final flush panic";

    let mut st = Stream::new(
        fresh_db("final-flush-panic.db"),
        Distinct::new(
            "distinct",
            (
                terminal::Count::new("count"),
                Filter::new(
                    |_: &u32| -> bool { std::panic::panic_any(PANIC_PAYLOAD) },
                    terminal::Bag::new("filtered"),
                ),
            ),
        ),
    );

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        st.wtx(|tx| tx.insert(&1));
    }))
    .expect_err("final flush should panic");
    assert_eq!(
        panic.downcast_ref::<&str>().copied(),
        Some(PANIC_PAYLOAD),
        "wtx should resume the original panic",
    );

    st.rtx(|(count, filtered)| {
        assert_eq!(count.get(), 0);
        assert!(!filtered.contains(&1));
    });

    st.wtx(|_| {});

    st.rtx(|(count, filtered)| {
        assert_eq!(count.get(), 0);
        assert!(!filtered.contains(&1));
    });
}
