#![recursion_limit = "256"]

mod common;

use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;

use figmog::flatten::flatten_file;
use figmog::model::{Id, Rec};
use figmog::store::{Churn, sync};
use fold::pipeline::{Keyed, Map};

/// Open a store whose pipeline is fronted by a delta probe: every push
/// into the graph bumps the counter. Zero churn must mean zero pushes.
macro_rules! open_probed {
    ($path:expr, $counter:expr) => {{
        let c = $counter.clone();
        ::fold::stream::KeyedStream::<Id, Rec, _>::new(
            $path,
            Map::new(
                move |d: &Keyed<Id, Rec>| {
                    c.set(c.get() + 1);
                    d.clone()
                },
                figmog::figmog_pipeline!(),
            ),
        )
    }};
}

fn pull(
    st: &mut fold::stream::KeyedStream<Id, Rec, impl fold::pipeline::Push<Keyed<Id, Rec>>>,
    fixture: &serde_json::Value,
) -> Churn {
    // NOTE: `impl Trait` in argument position works here because we only
    // use write-path (upsert/remove) APIs; readers stay at the call site.
    let flattened = flatten_file(fixture).unwrap();
    let prior = BTreeSet::new(); // overridden by tests that need the sweep
    sync(st, &prior, &flattened, 1_000)
}

#[test]
fn initial_pull_populates_every_sink() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Rc::new(Cell::new(0usize));
    let mut st = open_probed!(dir.path().join("db"), counter);

    let churn = pull(&mut st, &common::fixture_v1());
    assert_eq!(churn, Churn { added: 18, changed: 0, removed: 0, unchanged: 0 });
    // 18 records + 1 meta row, all fresh inserts -> 19 pushes
    assert_eq!(counter.get(), 19);

    st.rtx(|((nodes, children, text, instances_of, styled_by, bound_to, by_type),
             components, component_sets, styles, _vars, _colls, meta)| {
        assert_eq!(nodes.iter().count(), 12);
        assert_eq!(nodes.get(&"1:2".to_string()).unwrap().name, "Title");

        let mut kids = children.get(&"1:1".to_string());
        kids.sort();
        assert_eq!(kids, vec![(0, "1:2".to_string()), (1, "1:3".to_string())]);

        let hits = text.search("garden", 5);
        assert!(hits.iter().any(|h| h.val == "1:2"), "bm25 finds the title text");

        assert_eq!(instances_of.search(&"2:2".to_string()), vec!["1:3".to_string()]);
        assert_eq!(styled_by.search(&"S:2".to_string()), vec!["1:2".to_string()]);
        assert_eq!(bound_to.search(&"VariableID:100".to_string()), vec!["1:1".to_string()]);

        let mut texts = by_type.search(&"TEXT".to_string());
        texts.sort();
        assert_eq!(texts, vec!["1:2".to_string()]);

        assert_eq!(components.iter().count(), 3);
        assert_eq!(component_sets.iter().count(), 1);
        assert_eq!(styles.iter().count(), 2);
        let m = meta.get(&0).unwrap();
        assert_eq!(m.version, "100");
        assert_eq!(m.synced_at_unix_ms, 1_000);
    });
}

#[test]
fn identical_repull_causes_zero_churn() {
    let dir = tempfile::tempdir().unwrap();
    let counter = Rc::new(Cell::new(0usize));
    let mut st = open_probed!(dir.path().join("db"), counter);

    pull(&mut st, &common::fixture_v1());
    counter.set(0);
    let churn = pull(&mut st, &common::fixture_v1()); // same synced_at too
    assert_eq!(churn, Churn { added: 0, changed: 0, removed: 0, unchanged: 18 });
    assert_eq!(counter.get(), 0, "no delta may enter the graph on an identical re-pull");
}

#[test]
fn reopen_resumes_persisted_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    {
        let mut st = figmog::open_store!(&db);
        let flattened = flatten_file(&common::fixture_v1()).unwrap();
        sync(&mut st, &BTreeSet::new(), &flattened, 1_000);
    }
    let st = figmog::open_store!(&db);
    st.rtx(|((nodes, ..), _, _, _, _, _, _)| {
        assert_eq!(nodes.iter().count(), 12);
    });
}
