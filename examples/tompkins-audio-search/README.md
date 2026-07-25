# Tompkins Square Audio Search

> A searchable, multimodal memory of Tompkins Square Park: ask for words,
> weather, city sounds, or bird calls, then listen at the exact moment the
> archive remembers.

A search and listening tool over the Oda Tompkins Square Park audio archive —
1,236.9 hours across 110 HLS streams, recorded in March–May 2021. Text queries
reach spoken words, general environmental sound, and bird species; every hit
resolves to a playable moment with a stable, shareable URL.

Built on [BogKit](https://github.com/flowercomputers/bogkit): `fold` owns every
consistency relationship in the store, `anny` provides the vector indexes, and
`ese` embeds transcript text inside the pipeline.

## Status

| Milestone | State | Evidence |
|---|---|---|
| 0 — workspace | done | this crate |
| 1 — canonical corpus manifest | done | `data/manifest/`, 110 streams / 1236h 51m 48.000s, hash `7dcb11a0…` |
| 2 — playback mapping | done | 21,759 segments, 0 gaps, 10/10 landmark seeks, browser seek error 0 ms |
| 3 — multimodal pilot | partly done | 47.6 h compacted from stream 9422; CLAP + speech indexed, **re-analysis pending** (see below) |
| 4 — hybrid search | done | RRF + episodes + diversification + conjunction + master timeline, live at `serve` |
| 5 — evaluation gate | **not started** | needs hand-labelled ground truth; nothing here has been validated by listening |
| 6 — 295 East full copy | blocked on cost/bucket approval | |
| 7 — remaining 70 streams | not started | |

## Quick start

Nothing here needs cloud resources or credentials except the steps that
explicitly read the source bucket.

```bash
cargo test -p tompkins-audio-search
```

Freeze the corpus manifest from the local cross-reference catalog — reads no
media, touches no cloud:

```bash
cargo run -p tompkins-audio-search -- manifest all
```

Build and verify a stream's source-to-playback map:

```bash
cargo run -p tompkins-audio-search -- timeline 9561 --pts data/assets/9561/pts-0-21758.json
```

The S3 tooling lives in `tools/` and needs `npm install` once:

```bash
cd tools && npm install
```

```bash
node tools/s3-probe.mjs 9258 9561 9606
```

## What the archive actually looks like

Several facts here differ from what the planning documents assumed. They were
established by bounded read-only probes (`data/probe/`), and they drive the
design.

**The media playlists are live sliding windows, not VOD.** A media playlist
holds ~12 segments and usually lacks `EXT-X-ENDLIST`. It cannot enumerate a
12-hour stream, let alone the 358-hour one. Segment *order* therefore comes
from object numbering (`{stream}/stream_4_{n}.ts`, flat under the stream
prefix), not from a playlist.

**`EXTINF` is not just imprecise, it is wrong.** The playlist tail advertises
1.984 s and 2.005333 s. Decoded PTS over 900 real segments shows durations of
**91 or 94 AAC frames** — 1.941333 s and 2.005333 s. Assuming a single nominal
duration produced **−14.4 s of error over 30 minutes**; extrapolated that is
~5.8 minutes over this 12-hour stream and ~2.9 hours over the 358-hour one.
Decoded PTS is the only acceptable timeline source, and `DurationSource` records
which segments have it.

**PTS wraps.** MPEG-TS presentation timestamps are 33 bits at 90 kHz, so they
roll over every ~26.5 hours. The longest stream crosses that boundary about
thirteen times. `timeline::unwrap_pts` undoes it; without that a late seek in a
long stream lands hours from the target.

**No wall clock is recoverable from HLS.** Not one playlist carries
`EXT-X-PROGRAM-DATE-TIME`. The catalog's `recording_time_source` is
`local_segment_inventory` for all 110 streams, so every timestamp is
`S3DerivedApproximation` — object chronology, not recording time. The UI must
never present it as exact.

**Only one rendition survives.** All five renditions are advertised
(123–352 kbps) but only `stream_4` objects exist. The audio is AAC, 48 kHz,
**stereo**, ~350 kbps measured.

**The S3 inventory is stale.** It exists at
`s3://stream-inventory/oda-production-stream-storage/stream-index/`, but the
newest snapshot is **2021-12-04**, in ORC (127 files, 1.1 GB compressed). Since
the archive is immutable 2021 data that is still valid, but for per-stream work
a prefix-scoped `ListObjectsV2` costs `ceil(segments/1000)` requests — 22 for a
12-hour stream — which is cheaper and resumable. See
`docs/adr/0002-segment-enumeration.md`.

**Two catalog anomalies.** Streams 9225 and 9435 report zero duration; probing
shows 9225 genuinely holds 3 segments (~6 s). Four streams (9420, 9422, 9444,
9606) are linked to Tompkins *and* to Cuenca, "New performance", or NY Nights,
so parts of them are certainly not Tompkins.

## Corpus membership, stated honestly

Two layers, never conflated:

- **Tompkins-linked full streams** — high recall. 110 streams, 1236h 51m
  48.000s. Includes intervals that belonged to other performances.
- **Tompkins-assigned intervals** — higher precision, from stream-change logs
  that are known to be inconsistent. Carried as `AssignedInterval`s, and for
  this corpus *never* marked confident, because projecting a wall-clock
  assignment window onto stream time requires an S3-derived stream start.

The manifest records both. Nothing in the UI may present the full-stream corpus
as exclusively Tompkins.

Performance associations come from the links table, not the catalog's
`performance_ids` / `performance_names` columns: those are ` | `-joined and
**not positionally aligned** — stream 9534 lists two ids against one
de-duplicated name — so zipping them mislabels streams.

## Architecture

```text
Oda HLS (read-only, bounded)
        │
        ▼
canonical corpus manifest ──── deterministic, hashed, provenance-stamped
        │
        ▼
segment enumeration ────────── object numbering, explicit gaps
        │
        ▼
compaction + PTS probe ─────── stream-copy AAC → MP4, decoded timeline
        │
        ├──────────────┬──────────────────┐
        ▼              ▼                  ▼
   CLAP windows   VAD + WhisperX     BirdNET frames
   (10 s / 5 s)   (speech-positive)  (3 s)
        │              │                  │
        └──────────────┴──────────────────┘
                       │
                       ▼
            staged prepared batches ──── checksummed outside the store
                       │
                       ▼
          one short atomic fold commit
                       │
        ┌──────────────┼──────────────┬─────────────┐
        ▼              ▼              ▼             ▼
  acoustic HNSW   transcript      bird labels   timeline
  (CLAP, 512)     BM25 + HNSW     + HNSW        KeyedRanked
        │              │              │             │
        └──────────────┴──────────────┴─────────────┘
                       │
                       ▼
     reciprocal rank fusion → temporal episodes → diversify
                       │
                       ▼
          result + PlaybackSpan + stable URL
```

Inference never holds the write lock. Workers stage a batch, checksum it, and
hand it to the single store owner for a short commit.

### Why fold, and not a vector database

The store owns *relationships*, not just vectors. One `upsert` of a corrected
transcript or a reprocessed window atomically updates the acoustic index, the
transcript BM25 postings, the transcript embedding graph, the bird labels, the
timeline index, the species vocabulary, and the coverage views — and retracts
the previous model output from all of them. A user can never receive a vector
hit whose playback span was removed in another index.

This is why every record type shares one `KeyedStream<RecordKey, Record>`:
atomic fan-out is the property being bought.

Record keys are stable across reprocessing — `(stream_id, start_ms)`, not
`(stream_id, start_ms, model_version)`. Model identity lives in `ModelStamp`
inside the record body, so upserting a new model output *replaces* the old
evidence instead of accumulating a second copy of it.

## Precision, reported honestly

Detection precision and playback precision are different numbers.

| Evidence | Detection region | Jump target |
|---|---|---|
| Aligned spoken word | sub-utterance | within 1 s (high-confidence speech) |
| BirdNET | 3 s frame | inside the correct frame |
| CLAP | 10 s window, 5 s hop | within 5 s, refined for strong hits |
| Arbitrary position | decoded PTS | direct |

Preroll starts playback slightly before the evidence (1.5 s speech, 1 s bird,
2.5 s acoustic) but the visible timestamp and the shared URL always name the
evidence onset, not the preroll position.

A seek into missing media reports `PrecisionKind::AcrossGap` rather than
pretending audio exists.

## Privacy

The archive contains incidental human speech recorded in public space.
Defaults, pending the policy decisions in the handoff's §18:

- speech search and transcripts are **local and private by default**;
- **no speaker identification**, and no diarization unless it serves a defined
  product need;
- transcript confidence and machine-generated status are always visible;
- retraction is supported at stream, interval, transcript, and derived-index
  level — and because retraction is atomic across every index, deleting
  something actually deletes it.

## Cost discipline

The source bucket is not ours and its owner pays for every request. Therefore:

- read-only clients only — the S3 tools import no mutating command;
- no unbounded `ListObjects` walk; every enumeration is prefix-scoped with a
  hard page budget;
- `compact.mjs` refuses to start when the segment count exceeds `--budget`;
- fetches are cached and resumable, so a crash re-reads nothing;
- one pass over the source, then all experimentation runs against the local
  copy.

Spent so far: 11 probe requests, 22 enumeration requests, and 21,759 segment GETs
for the pilot fetch (1.93 GB). Everything since has run against the local copy.

## Known issue: the archive is partly phase-inverted, and it broke the decode

Large stretches of stream 9422 are recorded with the channels partly
phase-inverted — measured L/R correlation runs from -0.05 to -0.95. That is a
fault in the recording, and it is why those stretches sound hollow.

It also broke this pipeline in a way worth recording, because the failure
looked exactly like bad source audio. Every worker decoded with `-ac 1`, which
is `(L+R)/2`. On anti-correlated channels that *subtracts*: on one 20-second
span the downmix sat **9.3 dB below the left channel alone**, and the
500-2000 Hz band where speech lives fell from **6.6% of the energy to 0.01%**.
CLAP, BirdNET and Whisper were being fed the cancellation residue.

Worse, the first diagnosis of it was wrong. Scanning the compacted assets
*through the same downmix* reported 87 of 96 sampled points as dead audio.
Re-measuring a single channel put that at **0 of 12** — the measurement had
reproduced the bug and attributed it to the archive.

Fixed in `common.decode_asset`: mono now means one channel, never a sum. The
CLAP and speech indexes currently in the store were built before that fix and
need re-running — compute only, no further S3 cost.

Two things this argues for, both already in the design and both worth keeping:
compaction is verifiably lossless (a compacted asset and its source segments
decode to byte-identical PCM), so the fault could be isolated to decode rather
than to storage; and `tools/probe-quality.mjs` can survey a 359-hour stream for
usable audio in **44 GetObject calls** against the 43,085 a single 24-hour
window costs.

## The master timeline

The whole corpus sits on one wall-clock axis (`src/master.rs`), so the archive is
scrubbed as a single body of time and a search hit is a *point on that axis*
rather than an offset into some file. `/api/master` serves the placements,
`/api/master/resolve?t=` turns any global position into a `PlaybackSpan`, and
search results carry `global_start_ms`.

Measured over the frozen manifest: **108 placements spanning 53.8 days** from
2021-03-18, **1,236.6 h of audio across 53.8 days of wall clock** — the excess is
real, several microphones ran at once — needing **3 lanes** at peak overlap.
Streams 9225 and 9435 are reported unplaceable rather than positioned at a time
they were not recorded.

Two honesty constraints are built in rather than papered over:

- **Precision is not uniform.** Within a stream, position is exact (decoded
  PTS). *Between* streams it is only as good as `estimated_recording_start_utc`,
  which is S3 object chronology for all 110 streams. The UI says so on every
  view.
- **Concurrency is visible.** Resolving an instant returns every source that was
  recording then, playable first, and the player names the others — so "295 East
  8th Street was also running" is something the listener can see.

Scrubbing into a stretch with no indexed audio reports what *was* recording there
and offers the next playable position, instead of silently doing nothing.

## Speech: what the first pilot actually showed

Stream 9561 returned far less speech than a park recording suggests — 152 s of
VAD-positive audio in 43,516 s (0.35%), 20 spans kept. Two separable causes,
worth keeping straight:

**The source really is speech-poor.** CLAP agrees independently of the VAD: over
8,697 windows the speech prompt's cosine has a median of 0.101 and a maximum of
0.413, and "speech" is the top-scoring label in only 222 windows (2.6%). 9561 is
a quiet mobile recorder, median −40.7 dBFS.

**But the guards were too strict.** 33 of 60 candidate transcripts were refused
for `avg_logprob < -1.0`. Distant speech across a park scores low on token
probability *because it is distant*, not because it is invented, so that floor
was discarding the material the archive is made of. The VAD found 76 regions
where CLAP put "speech" on top in 222 windows — the same disagreement from the
other side.

Retuned as a result: VAD threshold 0.5 → 0.35, `min_avg_logprob` −1.0 → −1.35,
default model `base` → `small`. The no-speech and repetition guards target real
hallucination signatures and are unchanged. Refusals now go to
`*.refused.jsonl` with their scores, so the next threshold argument can be made
from the discarded text rather than from a count.

## Pilot results (stream 9561, 12.1 h mobile source)

| | |
|---|---|
| CLAP windows | 8,697 (10 s window, 5 s hop) |
| Zero-shot tags fired | 545 windows (6.3%) — the rest are represented by embedding alone |
| BirdNET detections | 191 across 25 species (House Sparrow 91, Barred Owl 24, Mourning Dove 17) |
| Speech | VAD found 152 s in 43,516 s (0.3%); 20 spans kept, 40 refused by the hallucination guards |
| Segment records | 21,759, zero gaps |
| Commit refusals | 0 |
| Timeline drift avoided | 348,202 ms (5.8 min) versus assumed durations |
| Browser seek error | 0 ms across three assets |

Two calibration findings worth carrying forward. CLAP's zero-shot scores needed
per-label calibration: scored by softmax, "bicycle" fired on 42% of windows and
*no* window came out untagged, because a per-window normalisation cannot express
"none of these apply". Scored against each label's own distribution over the
corpus, the same tag lands on 0.13%. And this recording is very quiet — median
−40.7 dBFS with 0.3% speech — so the speech track is sparse by nature, not by
failure.

## Layout

```text
src/domain.rs     record types, stable keys, PlaybackSpan, precision
src/manifest.rs   milestone 1: deterministic hashed corpus manifest
src/timeline.rs   milestone 2: PTS timeline, gaps, wraparound, assets
src/timeutil.rs   dependency-free UTC parsing for catalog columns
tools/            bounded read-only S3 access (Node, JS SDK)
docs/adr/         decision records
data/             generated artifacts, not checked in
```

The Intel `aws` CLI at `/usr/local/aws-cli` hangs on this machine, so the S3
tools use the JS SDK with the shared-credentials provider and the `oda` profile.
