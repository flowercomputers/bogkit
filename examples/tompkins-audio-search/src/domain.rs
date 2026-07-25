//! The canonical data model.
//!
//! Every record the store holds is identified by a [`RecordKey`] that is
//! *stable across reprocessing*: it names a place in the archive, not a
//! model run. Reprocessing a window with a new checkpoint upserts the same
//! key, which retracts the old model output from every index it reached (see
//! [`crate::store`]). Model identity lives in [`ModelStamp`] inside the
//! record body, never in the key.
//!
//! The other invariant: no record earns a place in the search indexes unless
//! it can resolve to a [`PlaybackSpan`]. A hit the user cannot listen to is a
//! bug, so the resolution path is part of the model rather than a detail of
//! the UI.

use serde::{Deserialize, Serialize};

/// Oda stream id — the numeric S3 prefix under the source bucket.
pub type StreamId = u32;

/// Milliseconds. All times in this crate are stream-relative unless the
/// field name says `utc`; see [`RecordingTimeQuality`] for why wall clock is
/// kept at arm's length.
pub type Ms = u64;

/// BirdNET species identity: the scientific name, lowercased and
/// underscored, which is stable across BirdNET label-file revisions in a way
/// the common name is not.
pub type SpeciesId = String;

// ---------------------------------------------------------------------------
// keys
// ---------------------------------------------------------------------------

/// Stable identity for one record.
///
/// This is the primary key of the whole store: one [`fold::stream::KeyedStream`]
/// carries every record type, so a single `upsert` fans out atomically to
/// every index that the record feeds, and `remove` retracts it from all of
/// them. Ordering of the variants is deliberate — `postcard` encodes the
/// discriminant first, so records of a kind sort together.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum RecordKey {
    /// One source stream.
    Stream(StreamId),
    /// One source HLS segment, by media sequence number.
    Segment(StreamId, u32),
    /// One CLAP analysis window, by stream-relative start.
    Acoustic(StreamId, Ms),
    /// One speech utterance, by stream-relative start.
    Speech(StreamId, Ms),
    /// One species detection in one BirdNET frame.
    Bird(StreamId, Ms, SpeciesId),
}

impl RecordKey {
    pub fn stream_id(&self) -> StreamId {
        match self {
            RecordKey::Stream(s)
            | RecordKey::Segment(s, _)
            | RecordKey::Acoustic(s, _)
            | RecordKey::Speech(s, _)
            | RecordKey::Bird(s, _, _) => *s,
        }
    }

    /// Stream-relative onset, for records that have one.
    pub fn start_ms(&self) -> Option<Ms> {
        match self {
            RecordKey::Acoustic(_, t) | RecordKey::Speech(_, t) | RecordKey::Bird(_, t, _) => {
                Some(*t)
            }
            _ => None,
        }
    }

    pub fn modality(&self) -> Option<Modality> {
        match self {
            RecordKey::Acoustic(..) => Some(Modality::Sound),
            RecordKey::Speech(..) => Some(Modality::Speech),
            RecordKey::Bird(..) => Some(Modality::Bird),
            _ => None,
        }
    }
}

/// Which analysis track produced a piece of evidence. Also the user-facing
/// search filter.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Modality {
    Speech,
    Sound,
    Bird,
}

impl Modality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Modality::Speech => "speech",
            Modality::Sound => "sound",
            Modality::Bird => "bird",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "speech" => Some(Modality::Speech),
            "sound" | "sounds" => Some(Modality::Sound),
            "bird" | "birds" => Some(Modality::Bird),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// provenance
// ---------------------------------------------------------------------------

/// Everything needed to reproduce (or invalidate) one model output.
///
/// Carried by every analysis record. `input_hash` covers the decoded audio
/// the model actually saw, so a re-remux that changes the bytes is
/// detectable even when the checkpoint has not moved.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ModelStamp {
    pub model_name: String,
    pub model_version: String,
    /// Hash of the checkpoint file, so a silently-swapped weight file shows up.
    pub checkpoint_hash: String,
    /// Hash of the pipeline configuration (window, hop, thresholds, priors).
    pub config_hash: String,
    /// Hash of the decoded audio span fed to the model.
    pub input_hash: String,
    pub created_at_utc: String,
}

/// How much to trust a wall-clock timestamp.
///
/// The inspected playlists carry no `EXT-X-PROGRAM-DATE-TIME`, so for this
/// corpus nothing is [`ExactProgramDateTime`](RecordingTimeQuality::ExactProgramDateTime).
/// The catalog's `recording_time_source` is `local_segment_inventory` for all
/// 110 Tompkins-linked streams, which is
/// [`S3DerivedApproximation`](RecordingTimeQuality::S3DerivedApproximation) —
/// object modification chronology, not recording time. The UI must say so.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RecordingTimeQuality {
    /// Recovered from `EXT-X-PROGRAM-DATE-TIME`. Trustworthy wall clock.
    ExactProgramDateTime,
    /// From Oda application/database metadata.
    AppMetadataEstimate,
    /// Derived from S3 object chronology. Approximate; can be badly wrong.
    S3DerivedApproximation,
    Unknown,
}

impl RecordingTimeQuality {
    /// Parse the catalog's `recording_time_source` column.
    pub fn from_catalog_source(s: &str) -> Self {
        match s.trim() {
            "program_date_time" => RecordingTimeQuality::ExactProgramDateTime,
            "app_metadata" | "database" => RecordingTimeQuality::AppMetadataEstimate,
            "local_segment_inventory" | "s3_inventory" | "s3_last_modified" => {
                RecordingTimeQuality::S3DerivedApproximation
            }
            "" => RecordingTimeQuality::Unknown,
            _ => RecordingTimeQuality::S3DerivedApproximation,
        }
    }

    /// Short label for the UI. Deliberately not reassuring.
    pub fn label(&self) -> &'static str {
        match self {
            RecordingTimeQuality::ExactProgramDateTime => "exact",
            RecordingTimeQuality::AppMetadataEstimate => "estimated (app metadata)",
            RecordingTimeQuality::S3DerivedApproximation => "approximate (S3 chronology)",
            RecordingTimeQuality::Unknown => "unknown",
        }
    }
}

/// How a stream came to be considered part of the Tompkins corpus.
///
/// The two layers must never be conflated: [`FullStreamLinked`](TompkinsLinkType::FullStreamLinked)
/// is the high-recall set (1,236.9 h over 110 streams, including intervals
/// that belonged to other performances), while
/// [`AssignedInterval`](TompkinsLinkType::AssignedInterval) is the
/// higher-precision set derived from stream-change logs that are known to
/// contain inconsistencies.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TompkinsLinkType {
    /// The whole stream is linked to a Tompkins performance, and to nothing else.
    FullStreamLinked,
    /// Linked to Tompkins *and* to at least one other performance: parts of
    /// this stream are certainly not Tompkins.
    FullStreamLinkedAmbiguous,
    /// A specific interval was assigned to Tompkins by the performance log.
    AssignedInterval,
}

// ---------------------------------------------------------------------------
// records
// ---------------------------------------------------------------------------

/// One source stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct StreamRecord {
    pub stream_id: StreamId,
    pub stream_name: String,
    pub source_bucket: String,
    pub source_prefix: String,
    /// Chosen once, in the manifest, and never re-derived: indexing two
    /// renditions of one stream is the classic way to double the corpus.
    pub selected_rendition: Option<String>,
    pub estimated_recording_start_utc: Option<String>,
    pub estimated_recording_end_utc: Option<String>,
    pub recording_time_quality: RecordingTimeQuality,
    pub duration_ms: Ms,
    pub tompkins_link_type: TompkinsLinkType,
    pub performance_ids: Vec<u32>,
    pub performance_names: Vec<String>,
    /// Intervals the performance log assigned to Tompkins, stream-relative.
    /// Empty when the log could not be projected onto this stream.
    pub assigned_intervals: Vec<AssignedInterval>,
    pub stream_change_log_count: u32,
    pub source_manifest_hash: Option<String>,
    pub ingest_status: IngestStatus,
}

/// A performance-log interval, projected onto stream-relative time.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AssignedInterval {
    pub performance_id: u32,
    pub start_ms: Ms,
    pub end_ms: Ms,
    /// The log is inconsistent in places; this records whether the projection
    /// onto stream time required guessing.
    pub confident: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum IngestStatus {
    /// In the manifest, nothing fetched.
    Declared,
    /// Playlists read, timeline built.
    TimelineMapped,
    /// Media compacted into playable assets.
    Compacted,
    /// Analysis committed.
    Analyzed,
    Failed,
}

/// One source HLS segment: the authoritative bridge from the original archive
/// to a playable asset.
///
/// `cumulative_*` is the stream-relative timeline; `source_pts_*` is what the
/// decoder actually reported. Playlist `EXTINF` durations are rounded and
/// accumulate drift over a 358-hour stream, so where PTS is available it
/// wins — see [`crate::timeline`].
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SegmentRecord {
    pub stream_id: StreamId,
    pub media_sequence: u32,
    pub source_object_key: String,
    pub playlist_duration_ms: Ms,
    pub source_pts_start: Option<i64>,
    pub source_pts_end: Option<i64>,
    pub cumulative_start_ms: Ms,
    pub cumulative_end_ms: Ms,
    pub source_etag_or_checksum: Option<String>,
    /// Which compacted asset holds this segment's audio, once copied.
    pub compacted_asset_id: Option<String>,
    /// Offset of this segment within that asset.
    pub asset_start_ms: Option<Ms>,
    /// True when the timeline had to insert this entry to stand in for media
    /// that is missing from the source. Never let a gap silently compress the
    /// timeline.
    pub is_gap: bool,
}

/// One CLAP analysis window.
///
/// Windows are model-appropriate (10 s window, 5 s hop by default), not
/// storage-appropriate: a 2-second HLS segment is too little acoustic context
/// and would couple model identity to how the archive happens to be chunked.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AcousticWindowRecord {
    pub stream_id: StreamId,
    pub start_ms: Ms,
    pub end_ms: Ms,
    /// CLAP audio embedding, L2-normalized so cosine distance is meaningful.
    pub clap_embedding: Vec<f32>,
    /// Zero-shot prompt-bank labels that cleared their tuned threshold.
    pub zero_shot_tags: Vec<AcousticTag>,
    pub rms_dbfs: f32,
    pub speech_probability: f32,
    pub model: ModelStamp,
    pub source_span: SourceSpan,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct AcousticTag {
    pub label: String,
    pub score: f32,
}

/// One speech utterance.
///
/// Only produced for VAD-positive regions: transcribing 1,237 hours blind
/// would be both expensive and a hallucination generator. `vad_confidence`
/// and `transcript_confidence` are retained so the UI can show, and the user
/// can filter on, how much to believe the text.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SpeechSpanRecord {
    pub stream_id: StreamId,
    pub utterance_start_ms: Ms,
    pub utterance_end_ms: Ms,
    pub text: String,
    pub language: String,
    pub words: Vec<WordTiming>,
    /// Never a person's identity — just "who is speaking" within one stream,
    /// and only when diarization is explicitly enabled.
    pub speaker_label: Option<String>,
    pub vad_confidence: f32,
    pub transcript_confidence: f32,
    /// Whisper's own no-speech estimate. High values on a VAD-positive region
    /// are the signature of a hallucinated transcript.
    pub no_speech_probability: f32,
    pub model: ModelStamp,
    pub source_span: SourceSpan,
}

/// A word and where it lands, which is what makes sub-second speech seeking
/// possible.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct WordTiming {
    pub text: String,
    pub start_ms: Ms,
    pub end_ms: Ms,
    pub confidence: f32,
}

/// One species detection in one BirdNET frame.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct BirdDetectionRecord {
    pub stream_id: StreamId,
    pub start_ms: Ms,
    pub end_ms: Ms,
    pub species_id: SpeciesId,
    pub scientific_name: String,
    pub common_name: String,
    pub confidence: f32,
    /// Only stored for bird-positive regions in this version.
    pub birdnet_embedding: Option<Vec<f32>>,
    pub location_prior_used: bool,
    pub week_prior_used: bool,
    pub model: ModelStamp,
    pub source_span: SourceSpan,
}

/// The original coordinates an analysis record came from — kept so that a
/// hit can always be traced back to source objects, not just to the
/// convenience copy.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SourceSpan {
    pub first_media_sequence: u32,
    pub last_media_sequence: u32,
    pub asset_id: Option<String>,
    pub asset_offset_ms: Option<Ms>,
}

/// The tagged union the store actually carries.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Record {
    Stream(StreamRecord),
    Segment(SegmentRecord),
    Acoustic(AcousticWindowRecord),
    Speech(SpeechSpanRecord),
    Bird(BirdDetectionRecord),
}

impl Record {
    pub fn key(&self) -> RecordKey {
        match self {
            Record::Stream(r) => RecordKey::Stream(r.stream_id),
            Record::Segment(r) => RecordKey::Segment(r.stream_id, r.media_sequence),
            Record::Acoustic(r) => RecordKey::Acoustic(r.stream_id, r.start_ms),
            Record::Speech(r) => RecordKey::Speech(r.stream_id, r.utterance_start_ms),
            Record::Bird(r) => RecordKey::Bird(r.stream_id, r.start_ms, r.species_id.clone()),
        }
    }

    pub fn stream_id(&self) -> StreamId {
        self.key().stream_id()
    }

    /// Stream-relative extent, for records that occupy time.
    pub fn extent(&self) -> Option<(Ms, Ms)> {
        match self {
            Record::Acoustic(r) => Some((r.start_ms, r.end_ms)),
            Record::Speech(r) => Some((r.utterance_start_ms, r.utterance_end_ms)),
            Record::Bird(r) => Some((r.start_ms, r.end_ms)),
            Record::Segment(r) => Some((r.cumulative_start_ms, r.cumulative_end_ms)),
            Record::Stream(_) => None,
        }
    }

    pub fn model(&self) -> Option<&ModelStamp> {
        match self {
            Record::Acoustic(r) => Some(&r.model),
            Record::Speech(r) => Some(&r.model),
            Record::Bird(r) => Some(&r.model),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// playback
// ---------------------------------------------------------------------------

/// What a hit resolves to so the user can hear it.
///
/// `precision_*` is the honest part: detection precision and playback
/// precision are different numbers, and a BirdNET hit is only ever known to
/// within its 3-second frame no matter how exact the seek is.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PlaybackSpan {
    pub stream_id: StreamId,
    pub stream_start_ms: Ms,
    pub stream_end_ms: Ms,
    pub asset_key: Option<String>,
    pub asset_offset_ms: Option<Ms>,
    /// How far before the evidence to start playing, so the user hears it
    /// arrive rather than landing mid-event.
    pub preroll_ms: Ms,
    pub precision_ms: Ms,
    pub precision_kind: PrecisionKind,
    /// The durable identity. Never an expiring S3 URL.
    pub stable_url: String,
}

/// Why a span's precision is what it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PrecisionKind {
    /// Forced-aligned word boundary.
    AlignedWord,
    /// Utterance onset from VAD, without word alignment.
    Utterance,
    /// BirdNET's 3-second frame.
    BirdFrame,
    /// A CLAP window; the hop size bounds the error.
    AcousticWindow,
    /// A refined CLAP onset, narrowed by re-scoring sub-windows.
    RefinedAcousticOnset,
    /// A raw seek to a requested stream position.
    StreamPosition,
    /// The span covers, or abuts, media missing from the source.
    AcrossGap,
}

impl PrecisionKind {
    pub fn label(&self) -> &'static str {
        match self {
            PrecisionKind::AlignedWord => "aligned word",
            PrecisionKind::Utterance => "utterance onset",
            PrecisionKind::BirdFrame => "3s BirdNET frame",
            PrecisionKind::AcousticWindow => "10s CLAP window",
            PrecisionKind::RefinedAcousticOnset => "refined onset",
            PrecisionKind::StreamPosition => "stream position",
            PrecisionKind::AcrossGap => "abuts missing media",
        }
    }
}

/// Preroll by evidence kind, from the handoff's targets: enough lead-in to
/// hear the event arrive, never so much that the user has to wait.
pub fn preroll_for(kind: PrecisionKind) -> Ms {
    match kind {
        PrecisionKind::AlignedWord | PrecisionKind::Utterance => 1_500,
        PrecisionKind::BirdFrame => 1_000,
        PrecisionKind::AcousticWindow | PrecisionKind::RefinedAcousticOnset => 2_500,
        PrecisionKind::StreamPosition | PrecisionKind::AcrossGap => 0,
    }
}

/// The stable, shareable identity of a moment.
///
/// Points at the evidence onset, not the preroll position: the link the user
/// copies should mean "the bird call", not "two seconds before it".
pub fn stable_url(stream_id: StreamId, start_ms: Ms) -> String {
    format!("/listen/{stream_id}?t={:.3}", start_ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_key_round_trips_through_postcard() {
        // the store encodes keys with postcard; a key that does not survive
        // the round trip silently orphans index entries
        for key in [
            RecordKey::Stream(9422),
            RecordKey::Segment(9422, 1_234_567),
            RecordKey::Acoustic(9422, 3_600_000),
            RecordKey::Speech(9422, 3_600_000),
            RecordKey::Bird(9422, 3_600_000, "cardinalis_cardinalis".into()),
        ] {
            let enc = postcard::to_stdvec(&key).unwrap();
            let back: RecordKey = postcard::from_bytes(&enc).unwrap();
            assert_eq!(key, back);
        }
    }

    #[test]
    fn record_key_derives_from_record() {
        let r = Record::Bird(BirdDetectionRecord {
            stream_id: 9422,
            start_ms: 12_000,
            end_ms: 15_000,
            species_id: "cardinalis_cardinalis".into(),
            scientific_name: "Cardinalis cardinalis".into(),
            common_name: "Northern Cardinal".into(),
            confidence: 0.81,
            birdnet_embedding: None,
            location_prior_used: true,
            week_prior_used: true,
            model: ModelStamp::default(),
            source_span: SourceSpan {
                first_media_sequence: 6000,
                last_media_sequence: 6001,
                asset_id: None,
                asset_offset_ms: None,
            },
        });
        assert_eq!(
            r.key(),
            RecordKey::Bird(9422, 12_000, "cardinalis_cardinalis".into())
        );
        assert_eq!(r.extent(), Some((12_000, 15_000)));
    }

    #[test]
    fn stable_url_points_at_evidence_in_seconds() {
        assert_eq!(stable_url(9422, 3_600_500), "/listen/9422?t=3600.500");
    }

    #[test]
    fn tompkins_link_type_distinguishes_ambiguous_streams() {
        // 9420, 9422, 9444 and 9606 are linked to Tompkins *and* to Cuenca,
        // "New performance" or NY Nights. Presenting them as exclusively
        // Tompkins would overstate the corpus.
        assert_ne!(
            TompkinsLinkType::FullStreamLinked,
            TompkinsLinkType::FullStreamLinkedAmbiguous
        );
    }

    #[test]
    fn catalog_time_source_never_claims_exact_wall_clock() {
        // the whole corpus reports `local_segment_inventory`
        assert_eq!(
            RecordingTimeQuality::from_catalog_source("local_segment_inventory"),
            RecordingTimeQuality::S3DerivedApproximation
        );
        // and an unrecognised source must degrade, not get promoted
        assert_eq!(
            RecordingTimeQuality::from_catalog_source("something_new"),
            RecordingTimeQuality::S3DerivedApproximation
        );
    }
}
