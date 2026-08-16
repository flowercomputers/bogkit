//! Demo submission for testing the bog-auto-judge workflow.
//!
//! A tiny fold database that tallies swamp creature sightings: one
//! materialized count and one bag of species, updated incrementally.

use fold::pipeline::terminal;
use fold::stream::Stream;

fn main() {
    let db_path = std::env::temp_dir().join("judge-test-demo.db");
    let _ = std::fs::remove_dir_all(&db_path);

    let mut st = Stream::new(
        &db_path,
        (
            terminal::Count::new("sightings"),
            terminal::Bag::<String>::new("species"),
        ),
    );

    st.wtx(|tx| {
        tx.insert(&"heron".to_string());
        tx.insert(&"newt".to_string());
        tx.insert(&"newt".to_string());
        tx.insert(&"bog turtle".to_string());
    });

    st.rtx(|(count, species)| {
        println!("sightings so far: {}", count.get());
        for (name, seen) in species.iter() {
            let name: String = name;
            println!("  {name} x{seen}");
        }
    });

    // the heron was a misidentified plastic bag — retract it everywhere
    st.wtx(|tx| tx.remove(&"heron".to_string()));

    st.rtx(|(count, species)| {
        println!("after correction: {} sightings", count.get());
        assert!(!species.contains(&"heron".to_string()));
    });
}
