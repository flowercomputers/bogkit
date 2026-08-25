//! Open-time behavior: the per-process store lock and full-store reset.

use super::fresh_db;
use crate::{pipeline::*, stream::*};

#[test]
fn second_open_returns_locked() {
    let path = fresh_db("open_locked.db");
    let _held = Stream::new(&path, terminal::Bag::<String>::new("bag"));
    match Stream::try_new(&path, terminal::Bag::<String>::new("bag")) {
        Err(crate::fjall::Error::Locked) => {}
        Ok(_) => panic!("second open of a locked store unexpectedly succeeded"),
        Err(e) => panic!("expected Locked, got {e:?}"),
    }
}

#[test]
fn reset_wipes_and_reinitializes() {
    let path = fresh_db("reset.db");
    let mut st = Stream::new(
        &path,
        (terminal::Count::new("n"), terminal::Bag::new("bag")),
    );
    st.wtx(|tx| {
        tx.insert(&"a".to_string());
        tx.insert(&"b".to_string());
    });
    st.rtx(|(n, bag)| {
        assert_eq!(n.get(), 2);
        assert_eq!(bag.iter().count(), 2);
    });

    st.reset();
    st.rtx(|(n, bag)| {
        assert_eq!(n.get(), 0);
        assert_eq!(bag.iter().count(), 0);
    });

    // the stream stays fully usable after a reset
    st.wtx(|tx| tx.insert(&"c".to_string()));
    st.rtx(|(n, bag)| {
        assert_eq!(n.get(), 1);
        assert!(bag.contains(&"c".to_string()));
    });
}

#[test]
fn keyed_reset_wipes_table_and_sinks() {
    let path = fresh_db("reset_keyed.db");
    let mut st = KeyedStream::new(&path, terminal::Table::new("rows"));
    st.wtx(|tx| {
        tx.upsert(&1u32, &"alice".to_string());
    });
    assert!(st.contains(&1));

    st.reset();
    assert_eq!(st.get(&1), None);

    st.wtx(|tx| {
        tx.upsert(&2u32, &"bob".to_string());
    });
    assert_eq!(st.get(&2), Some("bob".to_string()));
}
