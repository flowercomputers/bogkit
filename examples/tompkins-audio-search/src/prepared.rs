//! Reading prepared batches, and the boundary between inference and the store.
//!
//! Inference workers stage JSONL batches outside the store, each accompanied by
//! a `.ready` file holding the batch's SHA-256. This module refuses any batch
//! whose checksum does not match, so a truncated or half-written file can never
//! be committed — a crash costs at most one asset's work.
//!
//! It is also where the two halves of the timestamp story meet. Python emits
//! stream-relative times plus an asset id and offset; the media-sequence
//! coordinates are filled in here from the [`Timeline`], which is the one place
//! that knows how source segments map to stream time. Duplicating that
//! arithmetic in Python would be two implementations of the same subtle thing.
//!
//! A record that cannot be resolved to a playable span is rejected rather than
//! committed: a hit the user cannot listen to is worse than no hit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::domain::{
    AcousticTag, AcousticWindowRecord, BirdDetectionRecord, ModelStamp, Ms, Record, SourceSpan,
    SpeechSpanRecord, StreamId, WordTiming,
};
use crate::timeline::Timeline;

/// One staged batch on disk.
#[derive(Clone, Debug)]
pub struct Batch {
    pub track: String,
    pub asset_id: String,
    pub path: PathBuf,
    pub ready_path: PathBuf,
}

/// Why a batch or record was not committed.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub what: String,
    pub reason: String,
}

/// Discover complete batches under a prepared directory.
///
/// A `.jsonl` without a matching `.ready` is still being written, so it is
/// skipped silently; one whose checksum disagrees is reported, because that is
/// corruption rather than work in progress.
pub fn discover(prepared_dir: &Path) -> Result<(Vec<Batch>, Vec<Refusal>), String> {
    let mut batches = Vec::new();
    let mut refused = Vec::new();

    for track in ["acoustic", "speech", "bird"] {
        let dir = prepared_dir.join(track);
        if !dir.exists() {
            continue;
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        entries.sort();

        for path in entries {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let ready_path = dir.join(format!("{stem}.ready"));
            if !ready_path.exists() {
                continue; // still being written
            }
            match verify(&path, &ready_path) {
                Ok(()) => batches.push(Batch {
                    track: track.to_string(),
                    asset_id: stem,
                    path,
                    ready_path,
                }),
                Err(reason) => refused.push(Refusal {
                    what: path.display().to_string(),
                    reason,
                }),
            }
        }
    }
    Ok((batches, refused))
}

fn verify(path: &Path, ready_path: &Path) -> Result<(), String> {
    let expected = std::fs::read_to_string(ready_path)
        .map_err(|e| format!("{}: {e}", ready_path.display()))?
        .trim()
        .to_string();
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected {
        return Err(format!(
            "checksum mismatch: .ready says {}, file hashes to {}",
            &expected[..expected.len().min(16)],
            &actual[..16]
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// json -> records
// ---------------------------------------------------------------------------

fn stamp(v: &serde_json::Value) -> ModelStamp {
    let m = &v["model"];
    ModelStamp {
        model_name: m["model_name"].as_str().unwrap_or_default().into(),
        model_version: m["model_version"].as_str().unwrap_or_default().into(),
        checkpoint_hash: m["checkpoint_hash"].as_str().unwrap_or_default().into(),
        config_hash: m["config_hash"].as_str().unwrap_or_default().into(),
        input_hash: m["input_hash"].as_str().unwrap_or_default().into(),
        created_at_utc: m["created_at_utc"].as_str().unwrap_or_default().into(),
    }
}

fn floats(v: &serde_json::Value) -> Vec<f32> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
        .unwrap_or_default()
}

/// Fill the source-side coordinates from the timeline.
///
/// The timeline is authoritative about which segments cover a span, so this is
/// the only place media sequences are derived. `None` means the span does not
/// land on playable media, which disqualifies the record.
fn source_span(timeline: &Timeline, start_ms: Ms, end_ms: Ms) -> Option<SourceSpan> {
    let first = timeline.resolve_ms(start_ms)?;
    // an end exactly at the stream's end resolves to nothing, so step back
    let last = timeline
        .resolve_ms(end_ms.saturating_sub(1))
        .unwrap_or_else(|| first.clone());
    if first.in_gap {
        return None; // evidence inside missing media is not evidence
    }
    // A timeline entry exists for every enumerated segment, including the
    // 601,664 of stream 9422 that were never fetched. Resolving to one of those
    // yields coordinates that point at audio nobody has, which is how 7,958
    // unplayable records were once committed with "0 refusals".
    //
    // One millisecond of tolerance, though: an asset boundary rarely falls on a
    // whole millisecond (90 ticks), so a start time truncated to milliseconds
    // can land a few ticks inside the previous, un-fetched segment. That is a
    // rounding artifact, not a record pointing at absent audio.
    let first = match first.asset_id {
        Some(_) => first,
        None => {
            let snapped = timeline.resolve_ms(start_ms + 1)?;
            snapped.asset_id.as_ref()?;
            snapped
        }
    };
    Some(SourceSpan {
        first_media_sequence: first.media_sequence,
        last_media_sequence: last.media_sequence.max(first.media_sequence),
        asset_id: first.asset_id.clone(),
        asset_offset_ms: first.asset_offset_ticks.map(|t| t / 90),
    })
}

/// Parse one JSONL line into a record, resolving its source coordinates.
pub fn parse_record(
    line: &str,
    timeline: &Timeline,
    stream_id: StreamId,
) -> Result<Record, String> {
    let v: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let kind = v["kind"].as_str().unwrap_or_default();

    match kind {
        "acoustic" => {
            let start_ms = v["start_ms"].as_u64().ok_or("acoustic: no start_ms")?;
            let end_ms = v["end_ms"].as_u64().ok_or("acoustic: no end_ms")?;
            let span = source_span(timeline, start_ms, end_ms)
                .ok_or("acoustic: span does not resolve to playable media")?;
            Ok(Record::Acoustic(AcousticWindowRecord {
                stream_id,
                start_ms,
                end_ms,
                clap_embedding: floats(&v["clap_embedding"]),
                zero_shot_tags: v["zero_shot_tags"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| {
                                Some(AcousticTag {
                                    label: t["label"].as_str()?.to_string(),
                                    score: t["score"].as_f64()? as f32,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                rms_dbfs: v["rms_dbfs"].as_f64().unwrap_or(-120.0) as f32,
                speech_probability: v["speech_probability"].as_f64().unwrap_or(0.0) as f32,
                model: stamp(&v),
                source_span: span,
            }))
        }
        "speech" => {
            let start_ms = v["utterance_start_ms"]
                .as_u64()
                .ok_or("speech: no utterance_start_ms")?;
            let end_ms = v["utterance_end_ms"]
                .as_u64()
                .ok_or("speech: no utterance_end_ms")?;
            let span = source_span(timeline, start_ms, end_ms)
                .ok_or("speech: span does not resolve to playable media")?;
            Ok(Record::Speech(SpeechSpanRecord {
                stream_id,
                utterance_start_ms: start_ms,
                utterance_end_ms: end_ms,
                text: v["text"].as_str().unwrap_or_default().to_string(),
                language: v["language"].as_str().unwrap_or("unknown").to_string(),
                words: v["words"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|w| {
                                Some(WordTiming {
                                    text: w["text"].as_str()?.to_string(),
                                    start_ms: w["start_ms"].as_u64()?,
                                    end_ms: w["end_ms"].as_u64()?,
                                    confidence: w["confidence"].as_f64().unwrap_or(0.0) as f32,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                speaker_label: v["speaker_label"].as_str().map(str::to_string),
                vad_confidence: v["vad_confidence"].as_f64().unwrap_or(0.0) as f32,
                transcript_confidence: v["transcript_confidence"].as_f64().unwrap_or(0.0) as f32,
                no_speech_probability: v["no_speech_probability"].as_f64().unwrap_or(0.0) as f32,
                model: stamp(&v),
                source_span: span,
            }))
        }
        "bird" => {
            let start_ms = v["start_ms"].as_u64().ok_or("bird: no start_ms")?;
            let end_ms = v["end_ms"].as_u64().ok_or("bird: no end_ms")?;
            let span = source_span(timeline, start_ms, end_ms)
                .ok_or("bird: span does not resolve to playable media")?;
            let embedding = {
                let e = floats(&v["birdnet_embedding"]);
                if e.is_empty() { None } else { Some(e) }
            };
            Ok(Record::Bird(BirdDetectionRecord {
                stream_id,
                start_ms,
                end_ms,
                species_id: v["species_id"].as_str().unwrap_or_default().to_string(),
                scientific_name: v["scientific_name"].as_str().unwrap_or_default().to_string(),
                common_name: v["common_name"].as_str().unwrap_or_default().to_string(),
                confidence: v["confidence"].as_f64().unwrap_or(0.0) as f32,
                birdnet_embedding: embedding,
                location_prior_used: v["location_prior_used"].as_bool().unwrap_or(false),
                week_prior_used: v["week_prior_used"].as_bool().unwrap_or(false),
                model: stamp(&v),
                source_span: span,
            }))
        }
        other => Err(format!("unknown record kind {other:?}")),
    }
}

/// Read and parse one batch.
pub fn load_batch(
    batch: &Batch,
    timeline: &Timeline,
    stream_id: StreamId,
) -> (Vec<Record>, Vec<Refusal>) {
    let mut records = Vec::new();
    let mut refused = Vec::new();
    let text = match std::fs::read_to_string(&batch.path) {
        Ok(t) => t,
        Err(e) => {
            refused.push(Refusal {
                what: batch.path.display().to_string(),
                reason: e.to_string(),
            });
            return (records, refused);
        }
    };
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_record(line, timeline, stream_id) {
            Ok(r) => records.push(r),
            Err(reason) => refused.push(Refusal {
                what: format!("{}:{}", batch.asset_id, i + 1),
                reason,
            }),
        }
    }
    (records, refused)
}

/// Per-track counts, for the coverage report.
pub fn tally(records: &[Record]) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for r in records {
        let k = match r {
            Record::Acoustic(_) => "acoustic",
            Record::Speech(_) => "speech",
            Record::Bird(_) => "bird",
            Record::Segment(_) => "segment",
            Record::Stream(_) => "stream",
        };
        *out.entry(k).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::{IndexedObject, NOMINAL_SEGMENT_TICKS, SegmentIndex};

    fn timeline_for(ns: &[u32]) -> Timeline {
        let idx = SegmentIndex {
            stream_id: 9561,
            bucket: "b".into(),
            rendition: "stream_4".into(),
            object_count: ns.len(),
            first_segment: ns.first().copied(),
            last_segment: ns.last().copied(),
            segments: ns
                .iter()
                .map(|&n| IndexedObject {
                    n,
                    key: format!("9561/stream_4_{n}.ts"),
                    bytes: 88_000,
                    etag: String::new(),
                    last_modified: None,
                })
                .collect(),
        };
        let mut t = Timeline::from_index(&idx);
        t.assign_assets(30 * 60 * crate::timeline::TICKS_PER_SECOND);
        t
    }

    fn acoustic_line(start: u64, end: u64) -> String {
        let embedding: Vec<f32> = vec![0.1; 512];
        serde_json::json!({
            "kind": "acoustic",
            "stream_id": 9561,
            "start_ms": start,
            "end_ms": end,
            "clap_embedding": embedding,
            "zero_shot_tags": [{"label": "heavy rain", "score": 3.4}],
            "rms_dbfs": -31.2,
            "speech_probability": 0.02,
            "model": {
                "model_name": "clap", "model_version": "v1", "checkpoint_hash": "c",
                "config_hash": "g", "input_hash": "i", "created_at_utc": "2026-07-25T00:00:00Z"
            }
        })
        .to_string()
    }

    #[test]
    fn media_sequences_come_from_the_timeline_not_from_python() {
        let t = timeline_for(&(0..2000).collect::<Vec<_>>());
        // a window 60 s in: 60 s / 2.005333 s per segment is segment 29
        let r = parse_record(&acoustic_line(60_000, 70_000), &t, 9561).unwrap();
        let Record::Acoustic(a) = r else { panic!() };
        assert_eq!(a.source_span.first_media_sequence, 29);
        assert!(a.source_span.last_media_sequence >= 29);
        assert!(a.source_span.asset_id.is_some());
        assert_eq!(a.zero_shot_tags[0].label, "heavy rain");
    }

    #[test]
    fn a_record_that_cannot_be_played_is_refused() {
        // 10 segments is ~20 s of audio; a window at 10 minutes is off the end
        let t = timeline_for(&(0..10).collect::<Vec<_>>());
        let err = parse_record(&acoustic_line(600_000, 610_000), &t, 9561).unwrap_err();
        assert!(err.contains("does not resolve"), "got {err}");
    }

    #[test]
    fn a_span_on_enumerated_but_unfetched_audio_is_refused() {
        // The regression that let 7,958 unplayable records into the store: a
        // 358-hour stream is fully enumerated, so a timeline entry exists
        // everywhere, but only a fetched window has compacted assets. A record
        // landing outside that window resolves to real segment coordinates for
        // audio nobody has.
        let idx = SegmentIndex {
            stream_id: 9422,
            bucket: "b".into(),
            rendition: "stream_4".into(),
            object_count: 3000,
            first_segment: Some(0),
            last_segment: Some(2999),
            segments: (0..3000u32)
                .map(|n| IndexedObject {
                    n,
                    key: format!("9422/stream_4_{n}.ts"),
                    bytes: 88_000,
                    etag: String::new(),
                    last_modified: None,
                })
                .collect(),
        };
        let mut t = Timeline::from_index(&idx);
        // only segments 2000..2999 were fetched and compacted
        t.apply_asset_table(&[crate::timeline::AssetSpan {
            asset_id: "9422-2000-2999".into(),
            first_media_sequence: 2000,
            last_media_sequence: 2999,
            stream_start_ticks: 0,
            stream_end_ticks: 0,
        }])
        .unwrap();

        // inside the compacted window: accepted, with a real asset
        let inside = crate::timeline::ticks_to_ms(2500 * NOMINAL_SEGMENT_TICKS);
        let r = parse_record(&acoustic_line(inside, inside + 10_000), &t, 9422).unwrap();
        let Record::Acoustic(a) = r else { panic!() };
        assert_eq!(a.source_span.asset_id.as_deref(), Some("9422-2000-2999"));

        // outside it: on the timeline, but not on any audio
        let outside = crate::timeline::ticks_to_ms(500 * NOMINAL_SEGMENT_TICKS);
        let err = parse_record(&acoustic_line(outside, outside + 10_000), &t, 9422).unwrap_err();
        assert!(err.contains("does not resolve"), "got {err}");
    }

    #[test]
    fn an_asset_boundary_off_by_a_few_ticks_is_not_a_refusal() {
        // Asset boundaries fall on AAC frames (1920 ticks), which are rarely a
        // whole millisecond (90 ticks). A start time truncated to milliseconds
        // can therefore land a few ticks inside the previous segment — a
        // rounding artifact, not a record pointing at audio nobody has.
        let idx = SegmentIndex {
            stream_id: 9422,
            bucket: "b".into(),
            rendition: "stream_4".into(),
            object_count: 3000,
            first_segment: Some(0),
            last_segment: Some(2999),
            segments: (0..3000u32)
                .map(|n| IndexedObject {
                    n,
                    key: format!("9422/stream_4_{n}.ts"),
                    bytes: 88_000,
                    etag: String::new(),
                    last_modified: None,
                })
                .collect(),
        };
        let mut t = Timeline::from_index(&idx);
        t.apply_asset_table(&[crate::timeline::AssetSpan {
            asset_id: "9422-2000-2999".into(),
            first_media_sequence: 2000,
            last_media_sequence: 2999,
            stream_start_ticks: 0,
            stream_end_ticks: 0,
        }])
        .unwrap();

        // the exact tick boundary, truncated to milliseconds
        let boundary_ticks = 2000 * NOMINAL_SEGMENT_TICKS;
        let truncated_ms = crate::timeline::ticks_to_ms(boundary_ticks);
        assert!(
            truncated_ms * 90 < boundary_ticks,
            "this boundary should truncate below the asset start"
        );
        let r = parse_record(&acoustic_line(truncated_ms, truncated_ms + 10_000), &t, 9422)
            .expect("a one-tick underflow must not lose the window");
        let Record::Acoustic(a) = r else { panic!() };
        assert_eq!(a.source_span.asset_id.as_deref(), Some("9422-2000-2999"));

        // but a genuinely un-fetched position is still refused
        let far = crate::timeline::ticks_to_ms(500 * NOMINAL_SEGMENT_TICKS);
        assert!(parse_record(&acoustic_line(far, far + 10_000), &t, 9422).is_err());
    }

    #[test]
    fn evidence_inside_missing_media_is_refused() {
        // segments 100..199 absent: an analysis window there has no audio
        let ns: Vec<u32> = (0..100).chain(200..400).collect();
        let t = timeline_for(&ns);
        let gap_start_ms =
            crate::timeline::ticks_to_ms(120 * NOMINAL_SEGMENT_TICKS);
        let err = parse_record(&acoustic_line(gap_start_ms, gap_start_ms + 10_000), &t, 9561)
            .unwrap_err();
        assert!(err.contains("does not resolve"), "got {err}");
    }

    #[test]
    fn a_batch_with_a_bad_checksum_is_refused_not_committed() {
        let dir = std::env::temp_dir().join("tompkins-prepared-test");
        let _ = std::fs::remove_dir_all(&dir);
        let acoustic = dir.join("acoustic");
        std::fs::create_dir_all(&acoustic).unwrap();

        let good = acoustic.join("9561-0-100.jsonl");
        std::fs::write(&good, acoustic_line(0, 10_000) + "\n").unwrap();
        let digest = format!("{:x}", Sha256::digest(std::fs::read(&good).unwrap()));
        std::fs::write(acoustic.join("9561-0-100.ready"), digest + "\n").unwrap();

        // a batch whose contents changed after the marker was written
        let bad = acoustic.join("9561-101-200.jsonl");
        std::fs::write(&bad, acoustic_line(0, 10_000) + "\n").unwrap();
        std::fs::write(acoustic.join("9561-101-200.ready"), "0".repeat(64) + "\n").unwrap();

        // and one still being written: no marker at all
        std::fs::write(acoustic.join("9561-201-300.jsonl"), "partial").unwrap();

        let (batches, refused) = discover(&dir).unwrap();
        assert_eq!(batches.len(), 1, "only the intact batch is offered");
        assert_eq!(batches[0].asset_id, "9561-0-100");
        assert_eq!(refused.len(), 1, "the corrupt batch is reported");
        assert!(refused[0].reason.contains("checksum mismatch"));
    }

    #[test]
    fn a_malformed_line_does_not_lose_the_rest_of_the_batch() {
        let t = timeline_for(&(0..2000).collect::<Vec<_>>());
        let dir = std::env::temp_dir().join("tompkins-prepared-partial");
        let _ = std::fs::remove_dir_all(&dir);
        let acoustic = dir.join("acoustic");
        std::fs::create_dir_all(&acoustic).unwrap();
        let path = acoustic.join("9561-0-100.jsonl");
        let body = format!(
            "{}\nnot json at all\n{}\n",
            acoustic_line(0, 10_000),
            acoustic_line(20_000, 30_000)
        );
        std::fs::write(&path, &body).unwrap();
        let digest = format!("{:x}", Sha256::digest(body.as_bytes()));
        std::fs::write(acoustic.join("9561-0-100.ready"), digest + "\n").unwrap();

        let (batches, _) = discover(&dir).unwrap();
        let (records, refused) = load_batch(&batches[0], &t, 9561);
        assert_eq!(records.len(), 2);
        assert_eq!(refused.len(), 1);
        assert!(refused[0].what.ends_with(":2"), "the bad line is named");
    }
}
