use crate::{pipeline::*, stream::*};

use super::fresh_db;

#[test]
fn meter_counts_deltas_and_times_downstream() {
    let reg = MeterRegistry::new();
    let mut st = Stream::new(
        fresh_db("fold_meter.db"),
        Meter::new(
            &reg,
            "in",
            (
                terminal::Count::new("total"),
                Map::new(
                    |s: &String| s.len(),
                    Meter::new(&reg, "lens", terminal::Bag::new("lengths")),
                ),
            ),
        ),
    );

    st.wtx(|tx| {
        for i in 0..10 {
            tx.insert(&format!("item-{i}"));
        }
    });
    let after_insert = reg.get("in").unwrap();
    assert_eq!(after_insert.pushes, 10);
    assert_eq!(after_insert.inserts, 10);
    assert_eq!(after_insert.retracts, 0);
    assert_eq!(after_insert.commits, 1);

    st.wtx(|tx| {
        for i in 0..3 {
            tx.remove(&format!("item-{i}"));
        }
    });

    let snaps = reg.snapshot();
    assert_eq!(snaps.len(), 2);
    // pipelines are built inside-out, so the inner meter registers first
    assert_eq!(snaps[0].name, "lens");
    assert_eq!(snaps[1].name, "in");
    let inn = &reg.get("in").unwrap();
    let lens = &reg.get("lens").unwrap();

    assert_eq!(inn.pushes, 13);
    assert_eq!(inn.inserts, 10);
    assert_eq!(inn.retracts, 3);
    assert_eq!(inn.weight, 13);
    assert_eq!(inn.commits, 2);
    assert_eq!(inn.aborts, 0);

    assert_eq!(lens.pushes, 13);
    assert_eq!(lens.inserts, 10);
    assert_eq!(lens.retracts, 3);
    assert_eq!(lens.commits, 2);

    // the outer meter times the whole subtree, which contains the inner one
    assert!(inn.ns_downstream >= lens.ns_downstream);
    assert!(inn.ns_commit >= lens.ns_commit);
    // sinks write at commit: two commits happened, and the outer meter timed
    // the whole subtree's commit work (Count + Bag flush lives below it)
    assert!(inn.ns_commit > 0, "commit time was not metered");
    assert_eq!(inn.ns_total(), inn.ns_downstream + inn.ns_commit);

    // diffing snapshots isolates the second transaction
    let second = inn.diff(&after_insert);
    assert_eq!(second.name, "in");
    assert_eq!(second.pushes, 3);
    assert_eq!(second.inserts, 0);
    assert_eq!(second.retracts, 3);
    assert_eq!(second.weight, 3);
    assert_eq!(second.commits, 1);
    assert!(second.ns_downstream <= inn.ns_downstream);

    // and the sinks agree the deltas got through unchanged
    st.rtx(|(count, lengths)| {
        assert_eq!(count.get(), 7);
        assert_eq!(lengths.iter().map(|(_, n)| n as u64).sum::<u64>(), 7);
    });
}

#[test]
#[should_panic(expected = "duplicate meter name")]
fn meter_names_are_unique() {
    let reg = MeterRegistry::new();
    let _a = reg.register("dup");
    let _b = reg.register("dup");
}
