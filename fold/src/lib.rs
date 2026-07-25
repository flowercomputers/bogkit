//! Incrementally-maintained, persistent dataflow.
//!
//! A [`Stream`](stream::Stream) accepts *deltas*—a datum paired with a
//! signed multiplicity—and pushes them through a statically composed
//! [`pipeline`] of operators into persistent sinks (counts, bags, keyed
//! views, ordered indexes, histograms, full-text search). Every sink is
//! maintained incrementally: removing a
//! previously inserted record retracts its effect everywhere, and nothing is
//! recomputed from scratch.
//!
//! State lives in an embedded [fjall](https://docs.rs/fjall) LSM store.
//! Ingestion is transactional and crash-safe; reads observe one consistent
//! snapshot across all sinks.
//!
//! # Crate layout
//! - [`pipeline`] — the [`Push`](pipeline::Push) trait, operators, and
//!   [`terminal`](pipeline::terminal) sinks.
//! - [`stream`] — the [`Stream`](stream::Stream) driver and transaction
//!   plumbing.
//!
//! # Example
//!
//! ```no_run
//! use fold::pipeline::{Filter, Map, terminal};
//! use fold::stream::Stream;
//!
//! // Pipelines are built inside-out: each operator wraps its downstream.
//! // Tuples fan a stream out to multiple branches.
//! let mut st = Stream::new(
//!     "example.db",
//!     Filter::new(
//!         |s: &String| !s.is_empty(),
//!         (
//!             terminal::Count::new("total"),
//!             Map::new(|s: &String| s.len(), terminal::Bag::new("lengths")),
//!         ),
//!     ),
//! );
//!
//! // Writes are atomic: all sinks observe the whole batch or none of it.
//! st.wtx(|tx| {
//!     tx.insert(&"hello".to_string());
//!     tx.insert(&"world".to_string());
//! });
//!
//! // Reads span one snapshot; readers mirror the pipeline's sink structure.
//! st.rtx(|(count, lengths)| {
//!     assert_eq!(count.get(), 2);
//!     assert!(lengths.contains(&5));
//! });
//!
//! // Removal retracts: sinks roll back as if the record was never inserted.
//! st.wtx(|tx| tx.remove(&"hello".to_string()));
//! st.rtx(|(count, _)| assert_eq!(count.get(), 1));
//! ```

pub mod pipeline;

pub mod stream;

#[cfg(test)]
mod tests;
