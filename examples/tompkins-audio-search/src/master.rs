//! One wall-clock axis over the whole corpus.
//!
//! Every stream is placed on a single timeline running from the earliest
//! estimated recording start to the latest estimated end, so the archive can be
//! scrubbed as one continuous body of time and a search hit is a *point on that
//! axis* rather than an offset into some particular file.
//!
//! Two properties of the real data shape this, and both are visible in the API
//! rather than smoothed over.
//!
//! **Precision is not uniform.** Within a stream, position is exact: it comes
//! from decoded PTS (see [`crate::timeline`]). *Between* streams it is only as
//! good as `estimated_recording_start_utc`, which for all 110 Tompkins-linked
//! streams is `local_segment_inventory` — S3 object chronology, not recording
//! time. So a global position carries two different error bars depending on
//! whether you are comparing inside one stream or across two, and
//! [`Placement::time_quality`] says which.
//!
//! **Streams overlap.** Several microphones recorded the park at once — 295
//! East 8th Street ran for weeks while mobile sources came and went — so at a
//! given instant the corpus may hold zero, one, or several simultaneous
//! recordings. A single line cannot represent that, so placements are assigned
//! [`lane`](Placement::lane)s like a Gantt chart, and resolving a global time
//! can return more than one candidate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::{Ms, RecordingTimeQuality, StreamId};
use crate::manifest::CorpusManifest;
use crate::timeline::Timeline;
use crate::timeutil::{format_utc_ms, parse_utc_ms};

/// One stream's position on the master axis.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Placement {
    pub stream_id: StreamId,
    pub stream_name: String,
    /// Offset from the master epoch, in milliseconds.
    pub global_start_ms: Ms,
    pub global_end_ms: Ms,
    /// Extent of the stream itself. Measured from PTS when the stream has been
    /// compacted, otherwise the catalog's estimate.
    pub duration_ms: Ms,
    /// How much to trust `global_start_ms`. Never `ExactProgramDateTime` for
    /// this corpus.
    pub time_quality: RecordingTimeQuality,
    /// True when *some* of this stream is backed by compacted audio.
    ///
    /// Not the same as "a timeline exists": a 358-hour stream can be fully
    /// enumerated while only 24 hours of it have been fetched and compacted.
    /// Claiming the whole placement is playable would put a green bar over
    /// audio nobody can hear.
    pub indexed: bool,
    /// Global-time ranges actually backed by compacted assets. Empty when
    /// nothing has been compacted.
    pub playable_spans: Vec<(Ms, Ms)>,
    /// True when the duration came from decoded PTS rather than the catalog.
    pub duration_measured: bool,
    /// Row to draw this placement on, so overlapping streams do not collide.
    pub lane: usize,
    pub tompkins_link: String,
    pub estimated_start_utc: Option<String>,
}

impl Placement {
    pub fn contains_global(&self, t: Ms) -> bool {
        t >= self.global_start_ms && t < self.global_end_ms
    }

    /// Global position -> stream-relative position.
    pub fn to_stream_ms(&self, global_ms: Ms) -> Option<Ms> {
        if !self.contains_global(global_ms) {
            return None;
        }
        Some(global_ms - self.global_start_ms)
    }

    /// Stream-relative position -> global position.
    pub fn to_global_ms(&self, stream_ms: Ms) -> Ms {
        self.global_start_ms + stream_ms
    }
}

/// The whole corpus on one axis.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct MasterTimeline {
    /// UTC milliseconds of global t = 0: the earliest estimated start.
    pub epoch_utc_ms: i64,
    pub epoch_utc: String,
    /// Total span of the axis.
    pub total_ms: Ms,
    /// Sorted by `global_start_ms`.
    pub placements: Vec<Placement>,
    pub lane_count: usize,
    /// Sum of stream durations. Exceeds `total_ms` wherever streams overlap,
    /// which is the honest measure of how much audio exists versus how much
    /// wall clock it spans.
    pub recorded_ms: Ms,
    /// Milliseconds of the axis covered by at least one *indexed* stream.
    pub indexed_ms: Ms,
}

impl MasterTimeline {
    /// Place every stream in the manifest, using measured durations where a
    /// decoded timeline is available.
    ///
    /// Streams whose estimated start cannot be parsed are dropped: inventing a
    /// position for them would put audio at a time it certainly was not
    /// recorded, which is worse than admitting we cannot place it. They are
    /// returned separately so the gap is reportable.
    pub fn build(
        manifest: &CorpusManifest,
        timelines: &BTreeMap<StreamId, Timeline>,
    ) -> (Self, Vec<StreamId>) {
        let mut dated: Vec<(i64, &crate::manifest::ManifestStream, Ms, bool)> = Vec::new();
        let mut unplaceable: Vec<StreamId> = Vec::new();

        for s in &manifest.streams {
            let start = s
                .estimated_recording_start_utc
                .as_deref()
                .and_then(parse_utc_ms);
            let Some(start) = start else {
                unplaceable.push(s.stream_id);
                continue;
            };
            // a decoded timeline is authoritative about how long the stream is
            let (duration, measured) = match timelines.get(&s.stream_id) {
                Some(t) if t.total_ms() > 0 => (t.total_ms(), true),
                _ => (s.duration_ms, false),
            };
            if duration == 0 {
                // a zero-length stream cannot be scrubbed to; 9225 and 9435 are
                // genuinely ~6 s and ~0 s stubs
                unplaceable.push(s.stream_id);
                continue;
            }
            dated.push((start, s, duration, measured));
        }

        if dated.is_empty() {
            return (
                MasterTimeline {
                    epoch_utc_ms: 0,
                    epoch_utc: format_utc_ms(0),
                    total_ms: 0,
                    placements: Vec::new(),
                    lane_count: 0,
                    recorded_ms: 0,
                    indexed_ms: 0,
                },
                unplaceable,
            );
        }

        let epoch = dated.iter().map(|(s, ..)| *s).min().unwrap();
        dated.sort_by_key(|(s, m, ..)| (*s, m.stream_id));

        let mut placements: Vec<Placement> = dated
            .iter()
            .map(|(start, s, duration, measured)| {
                let global_start = (start - epoch).max(0) as Ms;
                Placement {
                    stream_id: s.stream_id,
                    stream_name: s.stream_name.clone(),
                    global_start_ms: global_start,
                    global_end_ms: global_start + duration,
                    duration_ms: *duration,
                    time_quality: s.recording_time_quality,
                    indexed: false,   // set below, from real asset coverage
                    playable_spans: Vec::new(),
                    duration_measured: *measured,
                    lane: 0,
                    tompkins_link: format!("{:?}", s.tompkins_link_type),
                    estimated_start_utc: s.estimated_recording_start_utc.clone(),
                }
            })
            .collect();

        // Derive playable coverage from the compacted asset table rather than
        // from the mere presence of a timeline.
        for p in placements.iter_mut() {
            if let Some(tl) = timelines.get(&p.stream_id) {
                let mut spans: Vec<(Ms, Ms)> = tl
                    .assets
                    .iter()
                    .map(|a| {
                        (
                            p.global_start_ms + crate::timeline::ticks_to_ms(a.stream_start_ticks),
                            p.global_start_ms + crate::timeline::ticks_to_ms(a.stream_end_ticks),
                        )
                    })
                    .collect();
                spans = merge_intervals(spans.into_iter());
                p.indexed = !spans.is_empty();
                p.playable_spans = spans;
            }
        }

        assign_lanes(&mut placements);

        let lane_count = placements.iter().map(|p| p.lane + 1).max().unwrap_or(0);
        let total_ms = placements.iter().map(|p| p.global_end_ms).max().unwrap_or(0);
        let recorded_ms = placements.iter().map(|p| p.duration_ms).sum();
        // playable milliseconds, not placement milliseconds
        let indexed_ms: Ms = merge_intervals(
            placements
                .iter()
                .flat_map(|p| p.playable_spans.iter().copied()),
        )
        .into_iter()
        .map(|(a, b)| b - a)
        .sum();

        (
            MasterTimeline {
                epoch_utc_ms: epoch,
                epoch_utc: format_utc_ms(epoch),
                total_ms,
                placements,
                lane_count,
                recorded_ms,
                indexed_ms,
            },
            unplaceable,
        )
    }

    /// Every stream covering a global position, indexed ones first.
    ///
    /// More than one is normal: concurrent microphones. The caller picks, and
    /// the UI shows the alternatives so a listener can switch source at the
    /// same moment in time.
    pub fn at(&self, global_ms: Ms) -> Vec<&Placement> {
        let mut hits: Vec<&Placement> = self
            .placements
            .iter()
            .filter(|p| p.contains_global(global_ms))
            .collect();
        // prefer something playable, then the longer stream as the steadier
        // source, then a stable id order
        let playable_here = |p: &Placement| {
            p.playable_spans
                .iter()
                .any(|(a, b)| global_ms >= *a && global_ms < *b)
        };
        hits.sort_by(|a, b| {
            // a stream that is playable *at this instant* beats one that is
            // merely playable somewhere else
            playable_here(b)
                .cmp(&playable_here(a))
                .then(b.indexed.cmp(&a.indexed))
                .then(b.duration_ms.cmp(&a.duration_ms))
                .then(a.stream_id.cmp(&b.stream_id))
        });
        hits
    }

    pub fn placement(&self, stream_id: StreamId) -> Option<&Placement> {
        self.placements.iter().find(|p| p.stream_id == stream_id)
    }

    /// Map a stream-relative time onto the master axis.
    pub fn to_global(&self, stream_id: StreamId, stream_ms: Ms) -> Option<Ms> {
        self.placement(stream_id).map(|p| p.to_global_ms(stream_ms))
    }

    /// The next indexed placement starting at or after `global_ms`, for
    /// continuous playback across a stretch of axis with no audio.
    pub fn next_indexed_after(&self, global_ms: Ms) -> Option<Ms> {
        self.placements
            .iter()
            .flat_map(|p| p.playable_spans.iter())
            .filter_map(|(a, b)| {
                if global_ms < *a {
                    Some(*a)          // the span starts later
                } else if global_ms < *b {
                    Some(global_ms)   // already inside playable audio
                } else {
                    None
                }
            })
            .min()
    }

    /// Contiguous runs of axis the scrubber can actually play.
    pub fn indexed_coverage(&self) -> Vec<(Ms, Ms)> {
        merge_intervals(
            self.placements
                .iter()
                .flat_map(|p| p.playable_spans.iter().copied()),
        )
    }

    /// Whether a global position is backed by compacted audio.
    pub fn is_playable(&self, global_ms: Ms) -> bool {
        self.placements
            .iter()
            .any(|p| p.playable_spans.iter().any(|(a, b)| global_ms >= *a && global_ms < *b))
    }

    /// UTC milliseconds for a global position.
    pub fn to_utc_ms(&self, global_ms: Ms) -> i64 {
        self.epoch_utc_ms + global_ms as i64
    }

    pub fn to_utc(&self, global_ms: Ms) -> String {
        format_utc_ms(self.to_utc_ms(global_ms))
    }
}

/// Greedy interval colouring: the first lane whose last placement has ended.
fn assign_lanes(placements: &mut [Placement]) {
    let mut lane_ends: Vec<Ms> = Vec::new();
    // placements arrive sorted by start, which is what makes greedy optimal
    for p in placements.iter_mut() {
        match lane_ends.iter().position(|end| *end <= p.global_start_ms) {
            Some(lane) => {
                lane_ends[lane] = p.global_end_ms;
                p.lane = lane;
            }
            None => {
                lane_ends.push(p.global_end_ms);
                p.lane = lane_ends.len() - 1;
            }
        }
    }
}

fn merge_intervals(items: impl Iterator<Item = (Ms, Ms)>) -> Vec<(Ms, Ms)> {
    let mut v: Vec<(Ms, Ms)> = items.collect();
    v.sort_unstable();
    let mut out: Vec<(Ms, Ms)> = Vec::new();
    for (start, end) in v {
        match out.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    out
}

fn union_length<'a>(items: impl Iterator<Item = &'a Placement>) -> Ms {
    merge_intervals(items.map(|p| (p.global_start_ms, p.global_end_ms)))
        .into_iter()
        .map(|(a, b)| b - a)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TompkinsLinkType;
    use crate::manifest::{ManifestProvenance, ManifestStream, ManifestTotals, Selection};

    fn stream(id: StreamId, name: &str, start: &str, secs: u64) -> ManifestStream {
        ManifestStream {
            stream_id: id,
            stream_name: name.into(),
            source_bucket: "b".into(),
            source_prefix: format!("s3://b/{id}/"),
            playlist_url: String::new(),
            duration_ms: secs * 1000,
            estimated_recording_start_utc: (!start.is_empty()).then(|| start.to_string()),
            estimated_recording_end_utc: None,
            recording_time_quality: RecordingTimeQuality::S3DerivedApproximation,
            tompkins_link_type: TompkinsLinkType::FullStreamLinked,
            tompkins_performance_ids: vec![88],
            other_performance_names: vec![],
            assigned_intervals: vec![],
            stream_change_log_count: 0,
            selected_rendition: None,
            segment_count: None,
            source_manifest_hash: None,
        }
    }

    fn manifest(streams: Vec<ManifestStream>) -> CorpusManifest {
        CorpusManifest {
            manifest_version: 1,
            provenance: ManifestProvenance {
                catalog_path: String::new(),
                catalog_blake3: String::new(),
                links_path: String::new(),
                links_blake3: String::new(),
                performance_filter: "tompkins".into(),
                selection: Selection::AllTompkinsLinked,
            },
            streams,
            totals: ManifestTotals::default(),
            manifest_hash: String::new(),
        }
    }

    #[test]
    fn epoch_is_the_earliest_estimated_start() {
        let m = manifest(vec![
            stream(2, "later", "2021-04-10 12:00:00+00:00", 3600),
            stream(1, "earliest", "2021-03-18 00:34:44+00:00", 3600),
        ]);
        let (mt, _) = MasterTimeline::build(&m, &BTreeMap::new());
        assert_eq!(mt.epoch_utc, "2021-03-18T00:34:44Z");
        // the earliest stream sits at global zero
        assert_eq!(mt.placement(1).unwrap().global_start_ms, 0);
        // and the later one is placed at its true wall-clock distance
        let expected = crate::timeutil::parse_utc_ms("2021-04-10 12:00:00+00:00").unwrap()
            - crate::timeutil::parse_utc_ms("2021-03-18 00:34:44+00:00").unwrap();
        assert_eq!(mt.placement(2).unwrap().global_start_ms, expected as Ms);
    }

    #[test]
    fn global_and_stream_time_round_trip() {
        let m = manifest(vec![stream(9561, "mobile", "2021-04-27 02:00:00+00:00", 43_516)]);
        let (mt, _) = MasterTimeline::build(&m, &BTreeMap::new());
        let p = mt.placement(9561).unwrap();
        // an event two hours into the stream
        let global = p.to_global_ms(7_200_000);
        assert_eq!(p.to_stream_ms(global), Some(7_200_000));
        // and its wall clock reads back
        assert_eq!(mt.to_utc(global), "2021-04-27T04:00:00Z");
    }

    #[test]
    fn concurrent_streams_get_separate_lanes() {
        // 295 East ran for weeks while mobile sources came and went; a single
        // line cannot show that
        let m = manifest(vec![
            stream(1, "295 East 8th Street", "2021-04-01 00:00:00+00:00", 7 * 86_400),
            stream(2, "Victor Esther Mobile", "2021-04-02 00:00:00+00:00", 3600),
            stream(3, "Esperanza Mobile", "2021-04-02 00:30:00+00:00", 3600),
        ]);
        let (mt, _) = MasterTimeline::build(&m, &BTreeMap::new());
        assert!(mt.lane_count >= 3, "three overlapping streams need three lanes");
        let lanes: Vec<usize> = [1, 2, 3]
            .iter()
            .map(|id| mt.placement(*id).unwrap().lane)
            .collect();
        assert_eq!(lanes.len(), 3);
        // all distinct, because all three overlap at 2021-04-02 00:30
        assert_eq!(
            lanes.iter().collect::<std::collections::BTreeSet<_>>().len(),
            3
        );
    }

    #[test]
    fn sequential_streams_reuse_a_lane() {
        let m = manifest(vec![
            stream(1, "a", "2021-04-01 00:00:00+00:00", 3600),
            stream(2, "b", "2021-04-01 02:00:00+00:00", 3600),
            stream(3, "c", "2021-04-01 04:00:00+00:00", 3600),
        ]);
        let (mt, _) = MasterTimeline::build(&m, &BTreeMap::new());
        assert_eq!(mt.lane_count, 1, "no overlap means one lane suffices");
    }

    #[test]
    fn resolving_an_instant_can_return_several_sources() {
        let m = manifest(vec![
            stream(1, "stationary", "2021-04-01 00:00:00+00:00", 86_400),
            stream(2, "mobile", "2021-04-01 06:00:00+00:00", 3600),
        ]);
        let (mt, _) = MasterTimeline::build(&m, &BTreeMap::new());
        // 06:30 is inside both
        let t = 6 * 3_600_000 + 1_800_000;
        let hits = mt.at(t);
        assert_eq!(hits.len(), 2, "two microphones were running");
        // and each maps the same instant to its own stream-relative offset
        assert_eq!(hits.iter().find(|p| p.stream_id == 1).unwrap().to_stream_ms(t), Some(t));
        assert_eq!(
            hits.iter().find(|p| p.stream_id == 2).unwrap().to_stream_ms(t),
            Some(1_800_000)
        );
    }

    #[test]
    fn indexed_streams_are_preferred_when_resolving() {
        let m = manifest(vec![
            stream(1, "not indexed", "2021-04-01 00:00:00+00:00", 86_400),
            stream(2, "indexed", "2021-04-01 00:00:00+00:00", 86_400),
        ]);
        let mut timelines = BTreeMap::new();
        timelines.insert(2u32, empty_timeline(2, 86_400_000));
        let (mt, _) = MasterTimeline::build(&m, &timelines);
        // the playable stream comes first, because an unplayable hit is useless
        assert_eq!(mt.at(1000)[0].stream_id, 2);
        assert!(mt.at(1000)[0].indexed);
    }

    #[test]
    fn a_decoded_duration_overrides_the_catalog_estimate() {
        // the catalog says 12.121 h from segment counting; PTS says 12.117 h
        let m = manifest(vec![stream(9561, "mobile", "2021-04-27 02:00:00+00:00", 43_636)]);
        let mut timelines = BTreeMap::new();
        timelines.insert(9561u32, empty_timeline(9561, 43_516_000));
        let (mt, _) = MasterTimeline::build(&m, &timelines);
        let p = mt.placement(9561).unwrap();
        assert_eq!(p.duration_ms, 43_516_000);
        assert!(p.duration_measured);
        assert_eq!(p.global_end_ms, 43_516_000);
    }

    #[test]
    fn streams_without_a_usable_start_are_reported_not_invented() {
        // placing audio at a time it was certainly not recorded is worse than
        // admitting we cannot place it
        let m = manifest(vec![
            stream(1, "placeable", "2021-04-01 00:00:00+00:00", 3600),
            stream(2, "no start", "", 3600),
            stream(3, "zero length", "2021-04-01 00:00:00+00:00", 0),
        ]);
        let (mt, unplaceable) = MasterTimeline::build(&m, &BTreeMap::new());
        assert_eq!(mt.placements.len(), 1);
        assert_eq!(unplaceable, vec![2, 3]);
    }

    #[test]
    fn recorded_time_exceeds_wall_clock_where_sources_overlap() {
        // the honest distinction between how much audio exists and how much
        // time it spans
        let m = manifest(vec![
            stream(1, "a", "2021-04-01 00:00:00+00:00", 3600),
            stream(2, "b", "2021-04-01 00:00:00+00:00", 3600),
        ]);
        let (mt, _) = MasterTimeline::build(&m, &BTreeMap::new());
        assert_eq!(mt.total_ms, 3_600_000, "one hour of wall clock");
        assert_eq!(mt.recorded_ms, 7_200_000, "two hours of audio");
    }

    #[test]
    fn coverage_merges_adjacent_indexed_spans() {
        let m = manifest(vec![
            stream(1, "a", "2021-04-01 00:00:00+00:00", 3600),
            // starts exactly when the first ends
            stream(2, "b", "2021-04-01 01:00:00+00:00", 3600),
            // and one much later, leaving a hole
            stream(3, "c", "2021-04-01 05:00:00+00:00", 3600),
        ]);
        let mut timelines = BTreeMap::new();
        for id in [1u32, 2, 3] {
            timelines.insert(id, empty_timeline(id, 3_600_000));
        }
        let (mt, _) = MasterTimeline::build(&m, &timelines);
        let cov = mt.indexed_coverage();
        assert_eq!(cov, vec![(0, 7_200_000), (18_000_000, 21_600_000)]);
        assert_eq!(mt.indexed_ms, 7_200_000 + 3_600_000);
    }

    #[test]
    fn playback_can_skip_forward_to_the_next_audio() {
        let m = manifest(vec![
            stream(1, "a", "2021-04-01 00:00:00+00:00", 3600),
            stream(2, "b", "2021-04-01 05:00:00+00:00", 3600),
        ]);
        let mut timelines = BTreeMap::new();
        timelines.insert(1u32, empty_timeline(1, 3_600_000));
        timelines.insert(2u32, empty_timeline(2, 3_600_000));
        let (mt, _) = MasterTimeline::build(&m, &timelines);
        // landing in the four-hour hole should offer the next real audio
        assert_eq!(mt.next_indexed_after(3_700_000), Some(18_000_000));
        // and inside playable audio it offers the position itself
        assert_eq!(mt.next_indexed_after(1_000_000), Some(1_000_000));
        // and past the end there is nothing to offer
        assert!(mt.next_indexed_after(mt.total_ms + 1).is_none());
    }

    #[test]
    fn a_partly_compacted_stream_only_claims_the_hours_it_can_play() {
        // 24 hours fetched out of a 358-hour stream: the placement spans the
        // whole recording, but only the compacted window is playable, and the
        // scrubber must not promise the rest
        let hours_358 = 358 * 3_600_000u64;
        let m = manifest(vec![stream(9422, "295 East 8th Street", "2021-04-06 21:54:16+00:00", 358 * 3600)]);
        let mut timelines = BTreeMap::new();
        let from = 167 * 3_600_000u64;
        timelines.insert(9422u32, partly_compacted(9422, hours_358, from, from + 24 * 3_600_000));
        let (mt, _) = MasterTimeline::build(&m, &timelines);

        let p = mt.placement(9422).unwrap();
        assert_eq!(p.global_end_ms - p.global_start_ms, hours_358, "placed at full length");
        assert!(p.indexed, "part of it is playable");
        assert_eq!(p.playable_spans.len(), 1);
        assert_eq!(mt.indexed_ms, 24 * 3_600_000, "only the fetched day counts");

        // playable inside the window, not outside it
        assert!(mt.is_playable(p.global_start_ms + from + 3_600_000));
        assert!(!mt.is_playable(p.global_start_ms + 3_600_000));
        // and from before the window, the next audio is the window's start
        assert_eq!(
            mt.next_indexed_after(p.global_start_ms),
            Some(p.global_start_ms + from)
        );
    }

    #[test]
    fn a_stream_with_a_timeline_but_no_assets_is_not_playable() {
        // enumerated and mapped, but nothing fetched yet
        let m = manifest(vec![stream(1, "a", "2021-04-01 00:00:00+00:00", 3600)]);
        let mut timelines = BTreeMap::new();
        timelines.insert(1u32, partly_compacted(1, 3_600_000, 0, 0));
        let (mt, _) = MasterTimeline::build(&m, &timelines);
        assert!(!mt.placement(1).unwrap().indexed);
        assert_eq!(mt.indexed_ms, 0);
        assert!(mt.indexed_coverage().is_empty());
    }

    #[test]
    fn the_axis_never_claims_exact_wall_clock_for_this_corpus() {
        let m = manifest(vec![stream(1, "a", "2021-04-01 00:00:00+00:00", 3600)]);
        let (mt, _) = MasterTimeline::build(&m, &BTreeMap::new());
        assert_eq!(
            mt.placements[0].time_quality,
            RecordingTimeQuality::S3DerivedApproximation
        );
        assert_ne!(
            mt.placements[0].time_quality,
            RecordingTimeQuality::ExactProgramDateTime
        );
    }

    /// A timeline with a known total and one asset covering all of it.
    fn empty_timeline(stream_id: StreamId, total_ms: Ms) -> Timeline {
        partly_compacted(stream_id, total_ms, 0, total_ms)
    }

    /// A timeline whose compacted audio covers only `[from_ms, to_ms)` — the
    /// shape produced by fetching 24 hours out of a 358-hour stream.
    fn partly_compacted(stream_id: StreamId, total_ms: Ms, from_ms: Ms, to_ms: Ms) -> Timeline {
        use crate::timeline::{AssetSpan, DurationSource, SegmentEntry, Timeline};
        let ticks = total_ms * 90;
        let mut tl = Timeline {
            stream_id,
            rendition: "stream_4".into(),
            entries: vec![SegmentEntry {
                media_sequence: 0,
                source_object_key: format!("{stream_id}/stream_4_0.ts"),
                bytes: 0,
                etag: String::new(),
                duration_ticks: ticks,
                duration_source: DurationSource::Pts,
                cumulative_start_ticks: 0,
                pts_start: Some(0),
                pts_end: Some(ticks as i64),
                is_gap: false,
                missing_count: 0,
                discontinuity: false,
                compacted_asset_id: Some(format!("{stream_id}-0-0")),
                asset_start_ticks: Some(0),
            }],
            assets: Vec::new(),
            total_ticks: ticks,
        };
        if to_ms > from_ms {
            tl.assets.push(AssetSpan {
                asset_id: format!("{stream_id}-{from_ms}-{to_ms}"),
                first_media_sequence: 0,
                last_media_sequence: 0,
                stream_start_ticks: from_ms * 90,
                stream_end_ticks: to_ms * 90,
            });
        }
        tl
    }
}
