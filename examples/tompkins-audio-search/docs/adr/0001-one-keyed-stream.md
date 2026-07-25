# ADR 0001 — One keyed stream carries every record type

**Status:** accepted
**Context:** milestone 0/3

## Context

The store holds five record kinds with different natural keys: streams,
segments, CLAP windows, speech utterances, and bird detections. Each feeds a
different subset of indexes — an acoustic HNSW, transcript BM25, a transcript
embedding graph, bird labels, a bird embedding graph, a per-stream timeline, a
species vocabulary.

The property the project actually needs from Bog is not vector search; `anny`
would provide that alone. It is that **one write fans out atomically, and one
retraction cleans up everywhere**. The handoff's failure table names the
specific bug this prevents: "model upgrade leaves stale terms/vectors", and
"a user must never receive a vector hit whose metadata or playback span was
removed in another index."

## Decision

Use a single `fold::stream::KeyedStream<RecordKey, Record, P>` for the entire
store, where `RecordKey` is an enum over the five identities and `Record` is
the matching tagged union. The pipeline fans out with `FilterMap` branches, one
per index, each selecting the variants it cares about.

Two consequences follow deliberately:

**Record keys are stable across reprocessing.** `RecordKey::Acoustic(stream_id,
start_ms)` names a place in the archive, not a model run. `model_version` lives
in `ModelStamp` inside the record body. Re-running CLAP with a new checkpoint
therefore *upserts the same key*, and `KeyedTx::upsert` retracts the previous
record from every index before inserting the new one. Had the version been part
of the key, the old vectors and postings would survive as a second, stale copy
of the same moment.

**Every index shares one key type.** A hit from any ranker is a `RecordKey`,
which is a point read away from its full record in the `Table` sink — so
resolving evidence, metadata, and `PlaybackSpan` needs no join and cannot go
out of sync with the index that produced the hit.

## Alternatives considered

*One `KeyedStream` per record type.* Simpler types, but writes across stores are
no longer one transaction. A crash between commits leaves a bird detection whose
stream record does not exist, or a vector whose playback span was retracted.
This gives up the single property that motivated using Bog.

*Composite key including model version.* Makes comparative model runs trivial,
but breaks replacement: the active index accumulates every version ever run.
Comparative runs are better served by a separately-named dataset, which keeps
the active corpus honest.

## Consequences

- The tuple fan-out is bounded at 16 branches by `fold`; the current design uses
  roughly 11, leaving room.
- `Hnsw`'s `TOP_K` is a compile-time constant, so the candidate pool per ranker
  is fixed at build time (set well above the display limit, for fusion).
- CLAP (512), `ese` transcript embeddings (512), and BirdNET (1024) need
  separate typed indexes; sharing a dimension does not let them share a sink.
- `FilterMap` closures must stay pure. A branch that consulted wall-clock time
  or a mutable table would break retraction, because `fold` re-maps the original
  datum when cancelling it.
