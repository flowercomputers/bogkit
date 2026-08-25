use super::fresh_db;
use crate::pipeline::terminal;
use crate::stream::{KeyedStream, Stream};

#[test]
fn err_rolls_back_completely() {
    let mut st = Stream::new(
        fresh_db("try_wtx.db"),
        (
            terminal::Count::new("total"),
            terminal::Bag::<String>::new("bag"),
        ),
    );

    // deltas pushed before the Err must not survive
    let out: Result<(), &str> = st.try_wtx(|tx| {
        tx.insert(&"doomed".to_string());
        tx.insert(&"also doomed".to_string());
        Err("changed my mind")
    });
    assert_eq!(out, Err("changed my mind"));
    st.rtx(|(count, bag)| {
        assert_eq!(count.get(), 0);
        assert_eq!(bag.iter().count(), 0);
    });

    // Ok commits, and the stream is healthy after a prior rollback
    let out: Result<u8, &str> = st.try_wtx(|tx| {
        tx.insert(&"kept".to_string());
        Ok(7)
    });
    assert_eq!(out, Ok(7));
    st.rtx(|(count, bag)| {
        assert_eq!(count.get(), 1);
        assert!(bag.contains(&"kept".to_string()));
    });
}

#[test]
fn keyed_check_and_set() {
    let mut st = KeyedStream::new(
        fresh_db("try_wtx_keyed.db"),
        terminal::Table::<u32, String>::new("rows"),
    );

    let mut claim = |key: u32, val: &str| {
        st.try_wtx(|tx| {
            if tx.contains(&key) {
                return Err("taken");
            }
            tx.upsert(&key, &val.to_string());
            Ok(())
        })
    };

    assert_eq!(claim(1, "first"), Ok(()));
    assert_eq!(claim(1, "second"), Err("taken"));
    assert_eq!(st.get(&1), Some("first".to_string()));
}
