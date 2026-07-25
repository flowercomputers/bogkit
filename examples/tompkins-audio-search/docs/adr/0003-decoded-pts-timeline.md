# ADR 0003 — Decoded PTS is the timeline; time is accumulated in 90 kHz ticks

**Status:** accepted
**Context:** milestone 2

## Context

The handoff says playlist `EXTINF` values are "suitable for the initial relative
timeline" and that decoded timestamps "should be authoritative wherever
available". Measurement showed the first half of that is not true for this
archive.

What the source actually provides:

- The playlist tail advertises two `EXTINF` values: **1.984 s** and
  **2.005333 s**.
- Decoded PTS over 900 real segments of stream 9561 shows durations of **91 or
  94 AAC frames** — **1.941333 s** and **2.005333 s**. The 93-frame value the
  playlist claims does not occur.
- 225 of those 900 segments are short.

So `EXTINF` is not a rounded version of the truth; it disagrees with it. And
because a media playlist only ever exposes ~12 values, there is no `EXTINF` at
all for 99.9% of a stream.

Assuming a single nominal duration cost **−14.4 s over 30 minutes**. Scaled:
~5.8 minutes over the 12-hour pilot stream, ~2.9 hours over the 358-hour stream
9422. That is not a precision refinement; it is the difference between a working
product and one where late seeks are useless.

## Decision

**Decoded PTS is the only authoritative duration source.** `DurationSource`
records provenance per segment — `Pts`, `PlaylistExtinf`, `NominalAssumed`, or
`GapEstimate` — and `Timeline::drift_report` reports how far an assumed timeline
sat from the decoded one, so the cost of any remaining assumption is visible
rather than hidden.

A stream whose timeline is still `NominalAssumed` must not be used for seeking.
Analysis records may not be committed against it.

**Time accumulates in 90 kHz MPEG-TS ticks**, not milliseconds. One 1024-sample
AAC frame at 48 kHz is exactly 1920 ticks, so every real segment duration is an
integer number of ticks and the timeline is exactly representable. Accumulating
milliseconds would lose a third of a millisecond per segment — about 7 seconds
over a 12-hour stream — reintroducing by rounding the very error the PTS probe
exists to remove.

**PTS wraparound is unwrapped explicitly.** MPEG-TS presentation timestamps are
33 bits at 90 kHz and roll over every 2^33/90000 ≈ 26.5 hours. Stream 9422 is
358 hours, crossing that boundary about thirteen times. `unwrap_pts` promotes a
raw value that sits more than half a wrap period below the stream's current
position into the next epoch. A test walks 358 simulated hours and asserts
monotonicity across all thirteen wraps; without this, a seek late in a long
stream lands hours away.

## Consequences

- Compaction and PTS probing are one step: the timeline cannot be finished
  without decoding, so `tools/compact.mjs` fetches, probes, and remuxes
  together, writing `pts-*.json` for the Rust side to apply.
- Remuxing is `-c:a copy`. The AAC bitstream is bit-identical to the archive;
  only the container changes. Measured container-versus-timeline disagreement on
  30-minute assets: **≤ 43 ms**.
- Assets break at gaps, so no asset spans missing media.
- Probing costs one `ffprobe` per segment (~11 s per 900 segments, parallelised).
  For the full 295 East subset that is ~2M invocations and would want batching
  by concatenated run instead.
- A timestamp discontinuity that wraparound cannot explain is flagged on the
  entry rather than smoothed away.
