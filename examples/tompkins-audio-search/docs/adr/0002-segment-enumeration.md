# ADR 0002 — Enumerate segments by prefix listing, not the S3 inventory

**Status:** accepted
**Context:** milestone 1/2
**Supersedes:** the handoff's instruction to "use this inventory for object
discovery"

## Context

The handoff directs us to the daily S3 inventory at
`s3://stream-inventory/oda-production-stream-storage/stream-index/` and warns
against an unbounded `ListObjects` walk of the production bucket. Both concerns
are right: the bucket holds millions of ~2-second objects, and a naive walk
would be slow and expensive for its owner.

Probing the inventory (`tools/s3-inventory.mjs`, 4 requests) found:

- it is **not daily** any more — the newest snapshot is **2021-12-04**;
- the format is **ORC**, schema `struct<bucket, key, last_modified_date>`;
- **127 data files totalling 1.107 GB compressed**, covering the whole bucket.

Meanwhile the media playlists turned out to be live sliding windows (~12
segments, usually no `ENDLIST`), so they cannot enumerate a stream either. Some
enumeration is unavoidable.

## Decision

Enumerate per stream with a **prefix-scoped `ListObjectsV2`** on
`{stream_id}/stream_4_`, paginated at 1000 keys with a hard page budget derived
from the expected object count.

Measured cost for the 12-hour pilot stream 9561: **22 requests, 2.0 seconds,
21,759 objects, zero gaps.** For the full 295 East subset (~2.0M segments) it is
roughly 2,000 requests — about $0.01 at current `LIST` pricing.

Reading the inventory instead would mean 127 GETs plus a 1.1 GB transfer, and an
ORC reader (pyarrow) in a toolchain that is otherwise Node and Rust — to obtain
a *stale* snapshot of the whole bucket when we want a current view of 110
prefixes.

This is not the walk the handoff warns against. A prefix scan is bounded by
construction: one stream, a known expected count, and exceeding the page budget
is a hard error rather than a long crawl.

## Why the stale inventory is nevertheless sound

The archive is immutable 2021 data and bucket versioning was never enabled, so a
2021-12-04 snapshot is still an accurate object list for our streams. The
inventory remains the right tool for a whole-corpus census, and
`tools/s3-inventory.mjs` is kept for that. It is simply the wrong tool for
enumerating a handful of prefixes.

## Consequences

- The frozen playlist's `EXT-X-MEDIA-SEQUENCE` gives a stream's last segment
  number in **one** request, which cheaply cross-checks the enumeration and the
  catalog's duration estimate. All four probed streams agreed.
- S3 returns keys lexicographically, in which `stream_4_100000` precedes
  `stream_4_10001`. The enumerator parses the integer and sorts numerically;
  `Timeline::from_index` also de-duplicates, because overlapping pagination
  would otherwise double a segment on the timeline.
- Missing object numbers become explicit gap entries, never closed up.
- If a full-corpus census is ever needed, add `--via-inventory` and pay the
  1.1 GB once.
