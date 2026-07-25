//! Milestone 2: the source-to-playback map.
//!
//! This is the first technical risk the project has to retire, because every
//! search result is only as good as its ability to reopen the right moment.
//! Three properties of the actual archive drive the design:
//!
//! 1. **The playlists are live sliding windows, not VOD.** A media playlist
//!    holds ~12 segments and usually has no `EXT-X-ENDLIST`, so it cannot
//!    enumerate a 12-hour — let alone 358-hour — stream. Segment *order*
//!    therefore comes from object numbering (`stream_4_{n}.ts`), and segment
//!    *duration* cannot come from `EXTINF` because we only ever see the last
//!    dozen values.
//!
//! 2. **Segments are whole numbers of AAC frames, and not all the same
//!    number.** The observed durations are 2.005333 s (94 frames) and 1.984 s
//!    (93 frames). Assuming a single nominal duration across 21,759 segments
//!    accumulates tens of seconds of error, so decoded PTS is authoritative
//!    wherever it exists and [`DurationSource`] records which is which.
//!
//! 3. **MPEG-TS PTS is 33 bits at 90 kHz, so it wraps every ~26.5 hours.**
//!    A 358-hour stream crosses that boundary about thirteen times.
//!    [`unwrap_pts`] undoes it; without this a late seek in a long stream
//!    lands hours away.
//!
//! Time is accumulated in 90 kHz ticks — the native MPEG-TS timebase, in
//! which one 1024-sample AAC frame at 48 kHz is exactly 1920 ticks — so the
//! timeline is free of the rounding drift that accumulating milliseconds
//! would introduce.
//!
//! Missing media becomes an explicit [`gap`](SegmentEntry::is_gap) entry that
//! occupies its estimated duration. A gap must never be closed up: doing so
//! silently shifts every later timestamp and quietly invalidates the whole
//! index.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain::{Ms, PlaybackSpan, PrecisionKind, StreamId, preroll_for, stable_url};

/// 90 kHz MPEG-TS ticks.
pub type Ticks = u64;

pub const TICKS_PER_SECOND: Ticks = 90_000;
pub const TICKS_PER_MS: Ticks = 90;

/// One 1024-sample AAC frame at 48 kHz, in ticks. Segment durations are
/// always an integer multiple of this.
pub const TICKS_PER_AAC_FRAME: Ticks = 1_920;

/// The common segment length: 94 AAC frames = 2.005333 s.
pub const NOMINAL_SEGMENT_TICKS: Ticks = 94 * TICKS_PER_AAC_FRAME;

/// PTS is 33 bits; it wraps at this value.
pub const PTS_WRAP: i64 = 1 << 33;

pub fn ticks_to_ms(t: Ticks) -> Ms {
    t / TICKS_PER_MS
}

pub fn ms_to_ticks(ms: Ms) -> Ticks {
    ms * TICKS_PER_MS
}

/// Where a segment's duration came from. Anything other than
/// [`Pts`](DurationSource::Pts) is an estimate, and the UI says so.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DurationSource {
    /// Decoded presentation timestamps. Authoritative.
    Pts,
    /// An `EXTINF` value from the live playlist tail.
    PlaylistExtinf,
    /// Assumed [`NOMINAL_SEGMENT_TICKS`] because nothing better exists yet.
    NominalAssumed,
    /// A stand-in for media missing from the source.
    GapEstimate,
}

// ---------------------------------------------------------------------------
// segment index (what tools/s3-enumerate.mjs writes)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexedObject {
    pub n: u32,
    pub key: String,
    pub bytes: u64,
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub last_modified: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SegmentIndex {
    pub stream_id: StreamId,
    pub bucket: String,
    pub rendition: String,
    pub object_count: usize,
    pub first_segment: Option<u32>,
    pub last_segment: Option<u32>,
    pub segments: Vec<IndexedObject>,
}

impl SegmentIndex {
    /// Read the enumerator's output. The JS tool writes camelCase, so the
    /// mapping is done explicitly rather than by attribute, which keeps the
    /// two sides legible to each other.
    pub fn read_json(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;

        let segments = v["segments"]
            .as_array()
            .ok_or("segment index has no `segments` array")?
            .iter()
            .map(|s| IndexedObject {
                n: s["n"].as_u64().unwrap_or(0) as u32,
                key: s["key"].as_str().unwrap_or_default().to_string(),
                bytes: s["bytes"].as_u64().unwrap_or(0),
                etag: s["etag"].as_str().unwrap_or_default().to_string(),
                last_modified: s["lastModified"].as_str().map(str::to_string),
            })
            .collect::<Vec<_>>();

        Ok(SegmentIndex {
            stream_id: v["streamId"].as_u64().unwrap_or(0) as StreamId,
            bucket: v["bucket"].as_str().unwrap_or_default().to_string(),
            rendition: v["rendition"].as_str().unwrap_or("stream_4").to_string(),
            object_count: v["objectCount"].as_u64().unwrap_or(segments.len() as u64) as usize,
            first_segment: v["firstSegment"].as_u64().map(|n| n as u32),
            last_segment: v["lastSegment"].as_u64().map(|n| n as u32),
            segments,
        })
    }
}

// ---------------------------------------------------------------------------
// timeline
// ---------------------------------------------------------------------------

/// One entry on the stream timeline: a real segment, or a stand-in for
/// missing media.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SegmentEntry {
    pub media_sequence: u32,
    pub source_object_key: String,
    pub bytes: u64,
    pub etag: String,
    pub duration_ticks: Ticks,
    pub duration_source: DurationSource,
    pub cumulative_start_ticks: Ticks,
    pub pts_start: Option<i64>,
    pub pts_end: Option<i64>,
    /// True when this entry stands in for media absent from the source.
    pub is_gap: bool,
    /// How many source segments are missing, for a gap entry.
    pub missing_count: u32,
    /// True when the decoder reported a timestamp discontinuity here.
    pub discontinuity: bool,
    pub compacted_asset_id: Option<String>,
    pub asset_start_ticks: Option<Ticks>,
}

impl SegmentEntry {
    pub fn cumulative_end_ticks(&self) -> Ticks {
        self.cumulative_start_ticks + self.duration_ticks
    }
    pub fn cumulative_start_ms(&self) -> Ms {
        ticks_to_ms(self.cumulative_start_ticks)
    }
    pub fn cumulative_end_ms(&self) -> Ms {
        ticks_to_ms(self.cumulative_end_ticks())
    }
}

/// A compacted playable asset covering a contiguous run of segments.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AssetSpan {
    pub asset_id: String,
    pub first_media_sequence: u32,
    pub last_media_sequence: u32,
    pub stream_start_ticks: Ticks,
    pub stream_end_ticks: Ticks,
}

impl AssetSpan {
    pub fn contains_ticks(&self, t: Ticks) -> bool {
        t >= self.stream_start_ticks && t < self.stream_end_ticks
    }
}

/// The full source-to-playback map for one stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Timeline {
    pub stream_id: StreamId,
    pub rendition: String,
    /// Ordered by `cumulative_start_ticks`; includes gap entries.
    pub entries: Vec<SegmentEntry>,
    pub assets: Vec<AssetSpan>,
    pub total_ticks: Ticks,
}

/// What a seek landed on.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Resolution {
    pub media_sequence: u32,
    pub source_object_key: String,
    pub offset_in_segment_ticks: Ticks,
    pub asset_id: Option<String>,
    pub asset_offset_ticks: Option<Ticks>,
    /// True when the requested time falls inside missing media. The player
    /// must show a gap rather than pretending audio exists.
    pub in_gap: bool,
    pub duration_source: DurationSource,
}

impl Timeline {
    /// Build the nominal timeline from an object listing.
    ///
    /// Every missing run of segment numbers becomes one explicit gap entry
    /// whose duration is the nominal length times the number of absent
    /// segments — an estimate, flagged as such, that keeps later timestamps
    /// where they belong.
    pub fn from_index(index: &SegmentIndex) -> Self {
        let mut objects = index.segments.clone();
        objects.sort_by_key(|o| o.n);
        objects.dedup_by_key(|o| o.n);

        let mut entries: Vec<SegmentEntry> = Vec::with_capacity(objects.len());
        let mut cursor: Ticks = 0;
        let mut prev_n: Option<u32> = None;

        for o in &objects {
            if let Some(p) = prev_n {
                let expected = p + 1;
                if o.n > expected {
                    let missing = o.n - expected;
                    let duration = NOMINAL_SEGMENT_TICKS * missing as Ticks;
                    entries.push(SegmentEntry {
                        media_sequence: expected,
                        source_object_key: format!(
                            "{}/{}_[{}..{}].ts MISSING",
                            index.stream_id, index.rendition, expected, o.n - 1
                        ),
                        bytes: 0,
                        etag: String::new(),
                        duration_ticks: duration,
                        duration_source: DurationSource::GapEstimate,
                        cumulative_start_ticks: cursor,
                        pts_start: None,
                        pts_end: None,
                        is_gap: true,
                        missing_count: missing,
                        discontinuity: true,
                        compacted_asset_id: None,
                        asset_start_ticks: None,
                    });
                    cursor += duration;
                }
            }
            entries.push(SegmentEntry {
                media_sequence: o.n,
                source_object_key: o.key.clone(),
                bytes: o.bytes,
                etag: o.etag.clone(),
                duration_ticks: NOMINAL_SEGMENT_TICKS,
                duration_source: DurationSource::NominalAssumed,
                cumulative_start_ticks: cursor,
                pts_start: None,
                pts_end: None,
                is_gap: false,
                missing_count: 0,
                discontinuity: false,
                compacted_asset_id: None,
                asset_start_ticks: None,
            });
            cursor += NOMINAL_SEGMENT_TICKS;
            prev_n = Some(o.n);
        }

        Timeline {
            stream_id: index.stream_id,
            rendition: index.rendition.clone(),
            entries,
            assets: Vec::new(),
            total_ticks: cursor,
        }
    }

    /// Replace assumed durations with decoded ones and re-accumulate.
    ///
    /// `probes` maps media sequence to `(pts_start, duration_ticks)` as
    /// reported by ffprobe. Segments without a probe keep their previous
    /// duration, so a partially-probed timeline is still coherent — it is
    /// just less precise, and [`drift_report`](Timeline::drift_report) says
    /// by how much.
    pub fn apply_pts(&mut self, probes: &BTreeMap<u32, PtsProbe>) {
        let mut cursor: Ticks = 0;
        let mut prev_pts_end: Option<i64> = None;

        for e in &mut self.entries {
            if let Some(p) = probes.get(&e.media_sequence).filter(|_| !e.is_gap) {
                let unwrapped = unwrap_pts(p.pts_start, prev_pts_end);
                // a jump the wrap logic cannot explain is a real
                // discontinuity, not a rounding artifact
                if let Some(prev) = prev_pts_end {
                    let jump = unwrapped - prev;
                    e.discontinuity = jump.abs() > TICKS_PER_AAC_FRAME as i64 * 2;
                }
                e.pts_start = Some(unwrapped);
                e.pts_end = Some(unwrapped + p.duration_ticks as i64);
                e.duration_ticks = p.duration_ticks;
                e.duration_source = DurationSource::Pts;
                prev_pts_end = e.pts_end;
            }
            e.cumulative_start_ticks = cursor;
            cursor += e.duration_ticks;
        }
        self.total_ticks = cursor;
    }

    /// Group segments into compacted assets of roughly `target` duration,
    /// breaking at gaps so no asset spans missing media.
    pub fn assign_assets(&mut self, target_ticks: Ticks) {
        self.assets.clear();
        let mut current: Option<AssetSpan> = None;
        let mut index = 0usize;

        for i in 0..self.entries.len() {
            let (is_gap, seq, start, end) = {
                let e = &self.entries[i];
                (e.is_gap, e.media_sequence, e.cumulative_start_ticks, e.cumulative_end_ticks())
            };

            if is_gap {
                if let Some(a) = current.take() {
                    self.assets.push(a);
                }
                continue;
            }

            let too_long = current
                .as_ref()
                .is_some_and(|a| end - a.stream_start_ticks > target_ticks);
            if too_long {
                if let Some(a) = current.take() {
                    self.assets.push(a);
                }
            }

            match current.as_mut() {
                Some(a) => {
                    a.last_media_sequence = seq;
                    a.stream_end_ticks = end;
                    a.asset_id = format!("{}-{}-{}", self.stream_id, a.first_media_sequence, seq);
                }
                None => {
                    current = Some(AssetSpan {
                        asset_id: format!("{}-{}", self.stream_id, seq),
                        first_media_sequence: seq,
                        last_media_sequence: seq,
                        stream_start_ticks: start,
                        stream_end_ticks: end,
                    });
                    index += 1;
                }
            }
        }
        if let Some(a) = current.take() {
            self.assets.push(a);
        }

        // stamp each segment with the asset that carries it
        let assets = self.assets.clone();
        for e in &mut self.entries {
            if e.is_gap {
                continue;
            }
            if let Some(a) = assets.iter().find(|a| {
                e.media_sequence >= a.first_media_sequence
                    && e.media_sequence <= a.last_media_sequence
            }) {
                e.compacted_asset_id = Some(a.asset_id.clone());
                e.asset_start_ticks =
                    Some(e.cumulative_start_ticks - a.stream_start_ticks);
            }
        }
    }

    /// Adopt the asset table that compaction actually produced.
    ///
    /// Preferred over [`assign_assets`](Timeline::assign_assets), which only
    /// *plans* a chunking. The files on disk are the ground truth, and two
    /// implementations of the same chunking rule inevitably disagree — ours did,
    /// by one segment, because `compact.mjs` closes an asset once it reaches the
    /// target while the planner closed it before exceeding the target. Reading
    /// the table removes that whole class of mismatch, in the same spirit as
    /// naming assets by segment range instead of a counter.
    pub fn apply_asset_table(&mut self, assets: &[AssetSpan]) -> Result<(), String> {
        // recompute each asset's stream extent from the timeline rather than
        // trusting the recorded ticks, so a stale table cannot shift playback
        let mut adopted: Vec<AssetSpan> = Vec::with_capacity(assets.len());
        for a in assets {
            let covered: Vec<&SegmentEntry> = self
                .entries
                .iter()
                .filter(|e| {
                    !e.is_gap
                        && e.media_sequence >= a.first_media_sequence
                        && e.media_sequence <= a.last_media_sequence
                })
                .collect();
            let Some(first) = covered.first() else {
                return Err(format!(
                    "asset {} covers segments {}..{}, none of which are on the timeline",
                    a.asset_id, a.first_media_sequence, a.last_media_sequence
                ));
            };
            let start = first.cumulative_start_ticks;
            let end = covered
                .last()
                .map(|e| e.cumulative_end_ticks())
                .unwrap_or(start);
            adopted.push(AssetSpan {
                asset_id: a.asset_id.clone(),
                first_media_sequence: a.first_media_sequence,
                last_media_sequence: a.last_media_sequence,
                stream_start_ticks: start,
                stream_end_ticks: end,
            });
        }
        adopted.sort_by_key(|a| a.stream_start_ticks);
        self.assets = adopted;

        let assets = self.assets.clone();
        for e in &mut self.entries {
            e.compacted_asset_id = None;
            e.asset_start_ticks = None;
            if e.is_gap {
                continue;
            }
            if let Some(a) = assets.iter().find(|a| {
                e.media_sequence >= a.first_media_sequence
                    && e.media_sequence <= a.last_media_sequence
            }) {
                e.compacted_asset_id = Some(a.asset_id.clone());
                e.asset_start_ticks = Some(e.cumulative_start_ticks - a.stream_start_ticks);
            }
        }
        Ok(())
    }

    /// The entry covering `ticks`, by binary search on cumulative start.
    fn entry_at(&self, ticks: Ticks) -> Option<&SegmentEntry> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = match self
            .entries
            .binary_search_by_key(&ticks, |e| e.cumulative_start_ticks)
        {
            Ok(i) => i,
            // the entry before the insertion point is the one that contains it
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let e = &self.entries[idx];
        if ticks < e.cumulative_end_ticks() {
            Some(e)
        } else {
            None // past the end of the stream
        }
    }

    /// Resolve a stream-relative position to a source segment and asset offset.
    pub fn resolve_ticks(&self, ticks: Ticks) -> Option<Resolution> {
        let e = self.entry_at(ticks)?;
        let offset = ticks - e.cumulative_start_ticks;
        Some(Resolution {
            media_sequence: e.media_sequence,
            source_object_key: e.source_object_key.clone(),
            offset_in_segment_ticks: offset,
            asset_id: e.compacted_asset_id.clone(),
            asset_offset_ticks: e.asset_start_ticks.map(|s| s + offset),
            in_gap: e.is_gap,
            duration_source: e.duration_source,
        })
    }

    pub fn resolve_ms(&self, ms: Ms) -> Option<Resolution> {
        self.resolve_ticks(ms_to_ticks(ms))
    }

    /// Build the [`PlaybackSpan`] a search hit is required to have.
    ///
    /// Returns `None` when the evidence cannot be played at all, which is the
    /// signal to refuse the record rather than surface an unplayable hit.
    pub fn playback_span(
        &self,
        start_ms: Ms,
        end_ms: Ms,
        kind: PrecisionKind,
        precision_ms: Ms,
    ) -> Option<PlaybackSpan> {
        let r = self.resolve_ms(start_ms)?;
        // landing in missing media is reported honestly, not hidden
        let kind = if r.in_gap { PrecisionKind::AcrossGap } else { kind };
        Some(PlaybackSpan {
            stream_id: self.stream_id,
            stream_start_ms: start_ms,
            stream_end_ms: end_ms,
            asset_key: r.asset_id.clone(),
            asset_offset_ms: r.asset_offset_ticks.map(ticks_to_ms),
            preroll_ms: preroll_for(kind),
            precision_ms,
            precision_kind: kind,
            stable_url: stable_url(self.stream_id, start_ms),
        })
    }

    pub fn total_ms(&self) -> Ms {
        ticks_to_ms(self.total_ticks)
    }

    pub fn gap_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_gap).count()
    }

    pub fn missing_segment_count(&self) -> u32 {
        self.entries.iter().map(|e| e.missing_count).sum()
    }

    pub fn discontinuity_count(&self) -> usize {
        self.entries.iter().filter(|e| e.discontinuity).count()
    }

    /// How far the assumed timeline was from the decoded one.
    ///
    /// The number that matters is `max_abs_ms`: it is the worst-case seek
    /// error a user would have hit if we had trusted nominal durations, and
    /// on a long stream it grows without bound.
    pub fn drift_report(&self, nominal: &Timeline) -> DriftMeasurement {
        let mut max_abs: i64 = 0;
        let mut at_sequence = 0u32;
        let mut compared = 0usize;

        let nominal_by_seq: BTreeMap<u32, Ticks> = nominal
            .entries
            .iter()
            .map(|e| (e.media_sequence, e.cumulative_start_ticks))
            .collect();

        for e in &self.entries {
            if let Some(&n) = nominal_by_seq.get(&e.media_sequence) {
                compared += 1;
                let d = e.cumulative_start_ticks as i64 - n as i64;
                if d.abs() > max_abs.abs() {
                    max_abs = d;
                    at_sequence = e.media_sequence;
                }
            }
        }

        DriftMeasurement {
            compared_segments: compared,
            max_abs_ticks: max_abs,
            max_abs_ms: max_abs / TICKS_PER_MS as i64,
            at_media_sequence: at_sequence,
            pts_segments: self
                .entries
                .iter()
                .filter(|e| e.duration_source == DurationSource::Pts)
                .count(),
            assumed_segments: self
                .entries
                .iter()
                .filter(|e| e.duration_source == DurationSource::NominalAssumed)
                .count(),
        }
    }

    pub fn write_json(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            path,
            serde_json::to_string(self).map_err(|e| e.to_string())? + "\n",
        )
        .map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn read_json(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Load the asset table written by `tools/compact.mjs`.
pub fn read_asset_table(path: &Path) -> Result<Vec<AssetSpan>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let arr = v["assets"].as_array().ok_or("asset table has no `assets` array")?;
    Ok(arr
        .iter()
        .filter_map(|a| {
            Some(AssetSpan {
                asset_id: a["assetId"].as_str()?.to_string(),
                first_media_sequence: a["firstMediaSequence"].as_u64()? as u32,
                last_media_sequence: a["lastMediaSequence"].as_u64()? as u32,
                stream_start_ticks: a["streamStartTicks"].as_u64().unwrap_or(0),
                stream_end_ticks: a["streamEndTicks"].as_u64().unwrap_or(0),
            })
        })
        .collect())
}

/// Load the PTS probes written by `tools/compact.mjs`.
///
/// The JS side writes `{"probes": {"0": {"ptsStart": .., "durationTicks": ..}}}`
/// with string keys, so the mapping is explicit rather than derived.
pub fn read_pts_probes(path: &Path) -> Result<BTreeMap<u32, PtsProbe>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let obj = v["probes"]
        .as_object()
        .ok_or("pts file has no `probes` object")?;
    let mut out = BTreeMap::new();
    for (k, val) in obj {
        let Ok(n) = k.parse::<u32>() else { continue };
        let (Some(pts), Some(dur)) = (
            val["ptsStart"].as_i64(),
            val["durationTicks"].as_u64(),
        ) else {
            continue;
        };
        out.insert(n, PtsProbe { pts_start: pts, duration_ticks: dur });
    }
    Ok(out)
}

/// How far a measured window's *position* could be off because the segments
/// before it were never decoded.
///
/// Everything inside a PTS-backed run is exact. But that run's offset from the
/// start of the stream is the sum of everything preceding it, and if that
/// prefix is still `NominalAssumed` its error accumulates at the same rate the
/// probed sample exhibits. For stream 9422 the probed 24 hours run 690 s short
/// of nominal over 43,085 segments; the 300,859 un-probed segments before them
/// therefore displace the window by roughly 80 minutes.
///
/// This is an estimate, not a measurement, and is reported as such — but
/// reporting nothing would present a position that is over an hour wrong as if
/// it were the same quality as the exact times inside the window.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PositionUncertainty {
    /// Assumed segments preceding the first decoded one.
    pub assumed_prefix_segments: u32,
    pub measured_segments: usize,
    /// Mean (measured - nominal) duration per segment, from the probed sample.
    pub mean_error_ticks_per_segment: f64,
    /// Estimated displacement of the measured window. Negative means the
    /// window truly sits *earlier* than the timeline places it.
    pub estimated_offset_ms: i64,
}

impl Timeline {
    /// Estimate the positional error of the decoded window, if any.
    ///
    /// Returns `None` when the stream is fully decoded — then there is no
    /// assumed prefix and position is exact throughout.
    pub fn position_uncertainty(&self) -> Option<PositionUncertainty> {
        let first_pts = self
            .entries
            .iter()
            .position(|e| e.duration_source == DurationSource::Pts)?;
        let assumed_prefix = self.entries[..first_pts]
            .iter()
            .filter(|e| e.duration_source == DurationSource::NominalAssumed)
            .count() as u32;
        if assumed_prefix == 0 {
            return None;
        }
        let measured: Vec<&SegmentEntry> = self
            .entries
            .iter()
            .filter(|e| e.duration_source == DurationSource::Pts)
            .collect();
        if measured.is_empty() {
            return None;
        }
        let total: u64 = measured.iter().map(|e| e.duration_ticks).sum();
        let mean = total as f64 / measured.len() as f64;
        let per_segment = mean - NOMINAL_SEGMENT_TICKS as f64;
        Some(PositionUncertainty {
            assumed_prefix_segments: assumed_prefix,
            measured_segments: measured.len(),
            mean_error_ticks_per_segment: per_segment,
            estimated_offset_ms: (assumed_prefix as f64 * per_segment / TICKS_PER_MS as f64)
                as i64,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DriftMeasurement {
    pub compared_segments: usize,
    pub max_abs_ticks: i64,
    pub max_abs_ms: i64,
    pub at_media_sequence: u32,
    pub pts_segments: usize,
    pub assumed_segments: usize,
}

/// One segment's decoded timing, as reported by ffprobe.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PtsProbe {
    pub pts_start: i64,
    pub duration_ticks: Ticks,
}

/// Undo 33-bit PTS wraparound.
///
/// MPEG-TS presentation timestamps are 33 bits at 90 kHz, so they roll over
/// every 2^33 / 90000 ≈ 26.5 hours. The archive's longest stream is 358
/// hours, which crosses that boundary roughly thirteen times; treating a
/// wrapped value as a backwards jump would put a late seek hours off target.
///
/// The rule: a raw timestamp that sits more than half a wrap period *below*
/// where the stream had reached belongs to the next epoch.
pub fn unwrap_pts(raw: i64, previous_unwrapped: Option<i64>) -> i64 {
    let Some(prev) = previous_unwrapped else {
        return raw;
    };
    // how many whole wraps the stream has already accumulated
    let epoch = prev.div_euclid(PTS_WRAP);
    let candidate = raw + epoch * PTS_WRAP;
    if candidate < prev - PTS_WRAP / 2 {
        candidate + PTS_WRAP
    } else if candidate > prev + PTS_WRAP / 2 {
        candidate - PTS_WRAP
    } else {
        candidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(n: u32) -> IndexedObject {
        IndexedObject {
            n,
            key: format!("9561/stream_4_{n}.ts"),
            bytes: 88_614,
            etag: format!("etag{n}"),
            last_modified: None,
        }
    }

    fn index(ns: &[u32]) -> SegmentIndex {
        SegmentIndex {
            stream_id: 9561,
            bucket: "oda-production-stream-storage".into(),
            rendition: "stream_4".into(),
            object_count: ns.len(),
            first_segment: ns.first().copied(),
            last_segment: ns.last().copied(),
            segments: ns.iter().copied().map(obj).collect(),
        }
    }

    #[test]
    fn nominal_timeline_accumulates_without_rounding_drift() {
        // 1000 segments of 94 AAC frames: in ticks this is exact, whereas
        // accumulating 2005 ms per segment would lose a third of a
        // millisecond each time
        let t = Timeline::from_index(&index(&(0..1000).collect::<Vec<_>>()));
        assert_eq!(t.total_ticks, 1000 * NOMINAL_SEGMENT_TICKS);
        assert_eq!(t.total_ticks, 180_480_000);
        // exactly 2005.333... s
        assert_eq!(t.total_ms(), 2_005_333);
        assert_eq!(t.gap_count(), 0);
    }

    #[test]
    fn missing_media_becomes_an_explicit_gap_that_holds_its_place() {
        // segments 5..9 are absent; the entry after the gap must keep the
        // timestamp it would have had, not slide five segments earlier
        let t = Timeline::from_index(&index(&[0, 1, 2, 3, 4, 10, 11]));
        assert_eq!(t.gap_count(), 1);
        assert_eq!(t.missing_segment_count(), 5);

        let gap = t.entries.iter().find(|e| e.is_gap).unwrap();
        assert_eq!(gap.duration_ticks, 5 * NOMINAL_SEGMENT_TICKS);
        assert_eq!(gap.duration_source, DurationSource::GapEstimate);

        let after = t.entries.iter().find(|e| e.media_sequence == 10).unwrap();
        assert_eq!(after.cumulative_start_ticks, 10 * NOMINAL_SEGMENT_TICKS);
        // and the total covers the hole rather than compressing it
        assert_eq!(t.total_ticks, 12 * NOMINAL_SEGMENT_TICKS);
    }

    #[test]
    fn seeking_into_a_gap_is_reported_not_hidden() {
        let t = Timeline::from_index(&index(&[0, 1, 2, 10, 11]));
        let inside = ticks_to_ms(4 * NOMINAL_SEGMENT_TICKS);
        let r = t.resolve_ms(inside).unwrap();
        assert!(r.in_gap, "a seek into missing media must say so");

        let span = t
            .playback_span(inside, inside + 3000, PrecisionKind::BirdFrame, 3000)
            .unwrap();
        // the precision kind is downgraded to the truth about the media
        assert_eq!(span.precision_kind, PrecisionKind::AcrossGap);
    }

    #[test]
    fn pts_overrides_assumed_durations_and_exposes_the_drift() {
        // a stream where every third segment is 93 frames rather than 94 —
        // the mix actually observed in the archive
        let ns: Vec<u32> = (0..3000).collect();
        let nominal = Timeline::from_index(&index(&ns));

        let mut probes = BTreeMap::new();
        let mut pts = 0i64;
        for &n in &ns {
            let frames = if n % 3 == 0 { 93 } else { 94 };
            let dur = frames * TICKS_PER_AAC_FRAME;
            probes.insert(n, PtsProbe { pts_start: pts, duration_ticks: dur });
            pts += dur as i64;
        }

        let mut actual = nominal.clone();
        actual.apply_pts(&probes);

        let d = actual.drift_report(&nominal);
        assert_eq!(d.pts_segments, 3000);
        assert_eq!(d.assumed_segments, 0);
        // 1000 segments one frame short: 1000 * 1920 ticks = 21.3 s of error
        assert_eq!(d.max_abs_ticks, -(1000 * TICKS_PER_AAC_FRAME as i64));
        assert!(
            d.max_abs_ms <= -21_000,
            "expected >21 s of drift, got {} ms",
            d.max_abs_ms
        );
    }

    #[test]
    fn an_undecoded_prefix_makes_the_measured_window_position_uncertain() {
        // the real shape of stream 9422: a 24-hour window decoded out of the
        // middle of a much longer stream whose earlier segments were never
        // fetched
        let ns: Vec<u32> = (0..5000).collect();
        let mut t = Timeline::from_index(&index(&ns));

        // decode only segments 4000..5000, and make them 91 frames like the
        // real archive rather than the assumed 94
        let mut probes = BTreeMap::new();
        let mut pts = 0i64;
        for n in 4000..5000u32 {
            let dur = 91 * TICKS_PER_AAC_FRAME;
            probes.insert(n, PtsProbe { pts_start: pts, duration_ticks: dur });
            pts += dur as i64;
        }
        t.apply_pts(&probes);

        let u = t.position_uncertainty().expect("an assumed prefix exists");
        assert_eq!(u.assumed_prefix_segments, 4000);
        assert_eq!(u.measured_segments, 1000);
        // three frames short per segment, so the prefix over-counts
        assert_eq!(u.mean_error_ticks_per_segment, -3.0 * TICKS_PER_AAC_FRAME as f64);
        // 4000 segments x 3 frames x 1920 ticks / 90 = 256,000 ms
        assert_eq!(u.estimated_offset_ms, -256_000);
        assert!(u.estimated_offset_ms < 0, "the window truly sits earlier");
    }

    #[test]
    fn a_fully_decoded_stream_has_no_positional_uncertainty() {
        let ns: Vec<u32> = (0..500).collect();
        let mut t = Timeline::from_index(&index(&ns));
        let mut probes = BTreeMap::new();
        let mut pts = 0i64;
        for &n in &ns {
            let dur = 91 * TICKS_PER_AAC_FRAME;
            probes.insert(n, PtsProbe { pts_start: pts, duration_ticks: dur });
            pts += dur as i64;
        }
        t.apply_pts(&probes);
        assert!(t.position_uncertainty().is_none(), "nothing is assumed");
    }

    #[test]
    fn pts_unwraps_across_the_33_bit_boundary() {
        // the boundary a 358-hour stream crosses about thirteen times
        let near_wrap = PTS_WRAP - 1000;
        assert_eq!(unwrap_pts(near_wrap, None), near_wrap);

        // the very next timestamp reads as a tiny number, but means "just after"
        let wrapped = unwrap_pts(500, Some(near_wrap));
        assert_eq!(wrapped, PTS_WRAP + 500);
        assert!(wrapped > near_wrap, "time must move forwards through a wrap");

        // and it keeps working on later epochs
        let second = unwrap_pts(500, Some(2 * PTS_WRAP - 1000));
        assert_eq!(second, 2 * PTS_WRAP + 500);

        // a genuine small step is left alone
        assert_eq!(unwrap_pts(5000, Some(4000)), 5000);
    }

    #[test]
    fn thirteen_wraps_stay_monotonic() {
        // simulate 358 hours: without unwrapping this sequence would appear
        // to jump backwards thirteen times
        let mut prev: Option<i64> = None;
        let mut last = i64::MIN;
        let step = NOMINAL_SEGMENT_TICKS as i64;
        let mut raw = 0i64;
        for _ in 0..(358 * 3600 * 90_000 / step) {
            let u = unwrap_pts(raw % PTS_WRAP, prev);
            assert!(u > last, "PTS went backwards at raw {raw}");
            last = u;
            prev = Some(u);
            raw += step;
        }
        // ~13 wraps' worth of ticks
        assert!(last > 13 * PTS_WRAP, "expected to cross 13 wraps, reached {last}");
    }

    #[test]
    fn assets_break_at_gaps_and_map_offsets() {
        // 30-minute assets over a stream with a hole in the middle
        let ns: Vec<u32> = (0..2000).chain(2100..3000).collect();
        let mut t = Timeline::from_index(&index(&ns));
        t.assign_assets(30 * 60 * TICKS_PER_SECOND);

        assert!(t.assets.len() >= 2, "expected several assets");
        // no asset may span the missing run
        for a in &t.assets {
            assert!(
                !(a.first_media_sequence < 2000 && a.last_media_sequence >= 2100),
                "asset {} spans the gap",
                a.asset_id
            );
        }

        // a segment's asset offset plus the asset's start is its stream time
        let e = t.entries.iter().find(|e| e.media_sequence == 2500).unwrap();
        let a = t
            .assets
            .iter()
            .find(|a| Some(&a.asset_id) == e.compacted_asset_id.as_ref())
            .unwrap();
        assert_eq!(
            a.stream_start_ticks + e.asset_start_ticks.unwrap(),
            e.cumulative_start_ticks
        );
    }

    #[test]
    fn resolution_is_exact_at_segment_boundaries_and_inside_them() {
        let t = Timeline::from_index(&index(&(0..100).collect::<Vec<_>>()));
        // exactly on a boundary
        let r = t.resolve_ticks(50 * NOMINAL_SEGMENT_TICKS).unwrap();
        assert_eq!(r.media_sequence, 50);
        assert_eq!(r.offset_in_segment_ticks, 0);
        // one tick before the boundary belongs to the previous segment
        let r = t.resolve_ticks(50 * NOMINAL_SEGMENT_TICKS - 1).unwrap();
        assert_eq!(r.media_sequence, 49);
        assert_eq!(r.offset_in_segment_ticks, NOMINAL_SEGMENT_TICKS - 1);
        // halfway into a segment
        let r = t.resolve_ticks(50 * NOMINAL_SEGMENT_TICKS + 900).unwrap();
        assert_eq!(r.media_sequence, 50);
        assert_eq!(r.offset_in_segment_ticks, 900);
    }

    #[test]
    fn seeking_past_the_end_returns_nothing() {
        let t = Timeline::from_index(&index(&(0..10).collect::<Vec<_>>()));
        assert!(t.resolve_ticks(t.total_ticks).is_none());
        assert!(t.resolve_ticks(t.total_ticks + 1).is_none());
        // the last representable instant still resolves
        assert!(t.resolve_ticks(t.total_ticks - 1).is_some());
    }

    #[test]
    fn no_increasing_seek_drift_late_in_a_long_stream() {
        // the acceptance criterion: a landmark near the end of a 12-hour
        // stream must resolve to the segment that actually contains it
        let ns: Vec<u32> = (0..21_759).collect();
        let mut probes = BTreeMap::new();
        let mut pts = 0i64;
        for &n in &ns {
            let frames = if n % 7 == 0 { 93 } else { 94 };
            let dur = frames * TICKS_PER_AAC_FRAME;
            probes.insert(n, PtsProbe { pts_start: pts, duration_ticks: dur });
            pts += dur as i64;
        }
        let mut t = Timeline::from_index(&index(&ns));
        t.apply_pts(&probes);

        for &seq in &[0u32, 1, 10_000, 21_000, 21_758] {
            let e = t.entries.iter().find(|e| e.media_sequence == seq).unwrap();
            // resolving the midpoint of a segment returns that same segment,
            // no matter how late in the stream it sits
            let mid = e.cumulative_start_ticks + e.duration_ticks / 2;
            let r = t.resolve_ticks(mid).unwrap();
            assert_eq!(r.media_sequence, seq, "drifted at segment {seq}");
        }
    }

    #[test]
    fn duplicate_object_listings_do_not_double_the_timeline() {
        // S3 pagination overlaps are a real failure mode; a repeated key must
        // not appear twice on the timeline
        let mut idx = index(&[0, 1, 2, 3]);
        idx.segments.push(obj(2));
        idx.segments.push(obj(3));
        let t = Timeline::from_index(&idx);
        assert_eq!(t.entries.len(), 4);
        assert_eq!(t.total_ticks, 4 * NOMINAL_SEGMENT_TICKS);
    }

    #[test]
    fn playback_span_carries_the_stable_url_and_preroll() {
        let t = Timeline::from_index(&index(&(0..1000).collect::<Vec<_>>()));
        let span = t
            .playback_span(600_000, 610_000, PrecisionKind::AlignedWord, 1000)
            .unwrap();
        assert_eq!(span.stable_url, "/listen/9561?t=600.000");
        assert_eq!(span.preroll_ms, 1_500);
        // the shared link names the evidence, not the preroll position
        assert_eq!(span.stream_start_ms, 600_000);
    }
}
