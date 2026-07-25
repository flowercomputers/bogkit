//! Milestone 1: freeze a canonical corpus manifest.
//!
//! The manifest is the one place the corpus is *decided*. Everything
//! downstream — compaction, inference, search, evaluation — reads it rather
//! than re-querying the catalog, because re-deriving the selection is how you
//! end up indexing two renditions of one stream or quietly changing what
//! "Tompkins" means between runs.
//!
//! Two properties matter:
//!
//! 1. **Determinism.** Streams are sorted, list fields are sorted and
//!    de-duplicated, and the file contains no wall-clock stamp, so
//!    regenerating from the same catalog produces byte-identical output. The
//!    [`manifest_hash`](CorpusManifest::manifest_hash) is a blake3 over a
//!    canonical rendering, and the input catalog is hashed too, so a manifest
//!    can always be traced to the evidence it came from.
//!
//! 2. **Honesty about membership.** 106 of the 110 streams are linked only to
//!    Tompkins performances; 4 (9420, 9422, 9444, 9606) are also linked to
//!    Cuenca, "New performance", or NY Nights, which means parts of them are
//!    certainly *not* Tompkins. Those are marked
//!    [`FullStreamLinkedAmbiguous`](TompkinsLinkType::FullStreamLinkedAmbiguous),
//!    and the performance log's assignment windows are carried separately as
//!    [`AssignedInterval`]s. The full-stream set is never presented as
//!    exclusively Tompkins.
//!
//! Performance associations come from the links table, not the catalog's
//! `performance_ids` / `performance_names` columns: those are ` | `-joined and
//! **not positionally aligned** (stream 9534 lists two ids against one
//! de-duplicated name), so zipping them would mislabel streams.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{
    AssignedInterval, IngestStatus, Ms, RecordingTimeQuality, StreamId, StreamRecord,
    TompkinsLinkType,
};
use crate::timeutil::parse_utc_ms;

/// Case-insensitive substring that identifies a Tompkins performance.
pub const TOMPKINS_FILTER: &str = "tompkins";

/// The stationary source that accounts for ~90% of the linked duration.
pub const EAST_295: &str = "295 East 8th Street";

/// The bucket holding current stream media. The older `oda-stream-storage`
/// and `oda-streams-processed` buckets are deliberately excluded.
pub const SOURCE_BUCKET: &str = "oda-production-stream-storage";

/// Which slice of the linked corpus a manifest describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Selection {
    /// All 110 Tompkins-linked streams (1,236.9 h).
    AllTompkinsLinked,
    /// The 40 `295 East 8th Street` streams (1,115.8 h).
    East295Stationary,
}

impl Selection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Selection::AllTompkinsLinked => "all-tompkins-linked",
            Selection::East295Stationary => "east-295-stationary",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "all" | "all-tompkins-linked" => Some(Selection::AllTompkinsLinked),
            "295" | "east-295" | "east-295-stationary" => Some(Selection::East295Stationary),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// catalog input
// ---------------------------------------------------------------------------

/// One row of `oda_stream_catalog.csv`, with only the columns we rely on.
#[derive(Clone, Debug)]
pub struct CatalogRow {
    pub stream_id: StreamId,
    pub stream_name: String,
    pub performance_names_raw: String,
    pub recording_start_estimate_utc: String,
    pub recording_end_estimate_utc: String,
    pub recording_time_source: String,
    pub duration_seconds_estimate: f64,
    pub stream_change_log_count: u32,
    pub playlist_url: String,
    pub s3_uri: String,
}

/// One row of `oda_stream_performance_links.csv`: the authoritative
/// (stream, performance) association.
#[derive(Clone, Debug)]
pub struct LinkRow {
    pub stream_id: StreamId,
    pub performance_id: u32,
    pub performance_name: String,
    pub first_assignment_utc: String,
    pub last_assignment_utc: String,
    pub stream_change_log_count: u32,
}

/// A CSV cell that may be empty, `9.0`-shaped, or genuinely integral.
fn cell_u32(s: &str) -> u32 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }
    s.parse::<u32>()
        .or_else(|_| s.parse::<f64>().map(|f| f as u32))
        .unwrap_or(0)
}

fn cell_f64(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() { 0.0 } else { s.parse().unwrap_or(0.0) }
}

fn reader(path: &Path) -> Result<csv::Reader<std::fs::File>, String> {
    csv::ReaderBuilder::new()
        .flexible(true)
        .from_path(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Index the header row so columns are addressed by name, not position.
fn header_index(rdr: &mut csv::Reader<std::fs::File>) -> Result<BTreeMap<String, usize>, String> {
    let headers = rdr.headers().map_err(|e| e.to_string())?;
    Ok(headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.trim().to_string(), i))
        .collect())
}

pub fn load_catalog(path: &Path) -> Result<Vec<CatalogRow>, String> {
    let mut rdr = reader(path)?;
    let idx = header_index(&mut rdr)?;
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| e.to_string())?;
        let Ok(stream_id) = col(&rec, "stream_id").trim().parse::<StreamId>() else {
            continue; // a row without a usable id cannot be addressed at all
        };
        out.push(CatalogRow {
            stream_id,
            stream_name: col(&rec, "stream_name").trim().to_string(),
            performance_names_raw: col(&rec, "performance_names"),
            recording_start_estimate_utc: col(&rec, "recording_start_estimate_utc"),
            recording_end_estimate_utc: col(&rec, "recording_end_estimate_utc"),
            recording_time_source: col(&rec, "recording_time_source"),
            duration_seconds_estimate: cell_f64(&col(&rec, "recording_duration_seconds_estimate")),
            stream_change_log_count: cell_u32(&col(&rec, "stream_change_log_count")),
            playlist_url: col(&rec, "playlist_url").trim().to_string(),
            s3_uri: col(&rec, "s3_uri").trim().to_string(),
        });
    }
    Ok(out)
}

pub fn load_links(path: &Path) -> Result<Vec<LinkRow>, String> {
    let mut rdr = reader(path)?;
    let idx = header_index(&mut rdr)?;
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| e.to_string())?;
        let Ok(stream_id) = col(&rec, "stream_id").trim().parse::<StreamId>() else {
            continue;
        };
        out.push(LinkRow {
            stream_id,
            performance_id: cell_u32(&col(&rec, "performance_id")),
            performance_name: col(&rec, "performance_name").trim().to_string(),
            first_assignment_utc: col(&rec, "first_assignment_utc"),
            last_assignment_utc: col(&rec, "last_assignment_utc"),
            stream_change_log_count: cell_u32(&col(&rec, "stream_change_log_count")),
        });
    }
    Ok(out)
}

pub fn is_tompkins(performance_name: &str) -> bool {
    performance_name.to_lowercase().contains(TOMPKINS_FILTER)
}

// ---------------------------------------------------------------------------
// manifest
// ---------------------------------------------------------------------------

/// One stream, frozen.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ManifestStream {
    pub stream_id: StreamId,
    pub stream_name: String,
    pub source_bucket: String,
    pub source_prefix: String,
    pub playlist_url: String,
    pub duration_ms: Ms,
    pub estimated_recording_start_utc: Option<String>,
    pub estimated_recording_end_utc: Option<String>,
    pub recording_time_quality: RecordingTimeQuality,
    pub tompkins_link_type: TompkinsLinkType,
    /// Sorted, de-duplicated. From the links table.
    pub tompkins_performance_ids: Vec<u32>,
    /// Sorted, de-duplicated. Non-Tompkins performances this stream also
    /// served — the reason [`TompkinsLinkType::FullStreamLinkedAmbiguous`]
    /// exists.
    pub other_performance_names: Vec<String>,
    pub assigned_intervals: Vec<AssignedInterval>,
    pub stream_change_log_count: u32,
    /// Filled by the S3 probe stage, once a rendition has been verified to
    /// have media objects present. `None` means "not yet verified"; nothing
    /// downstream may read media without it.
    pub selected_rendition: Option<String>,
    pub segment_count: Option<u32>,
    pub source_manifest_hash: Option<String>,
}

/// Provenance for the inputs a manifest was built from.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ManifestProvenance {
    pub catalog_path: String,
    pub catalog_blake3: String,
    pub links_path: String,
    pub links_blake3: String,
    pub performance_filter: String,
    pub selection: Selection,
}

/// The frozen corpus.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CorpusManifest {
    pub manifest_version: u32,
    pub provenance: ManifestProvenance,
    /// Sorted by `stream_id`.
    pub streams: Vec<ManifestStream>,
    pub totals: ManifestTotals,
    /// blake3 of [`canonical_bytes`](CorpusManifest::canonical_bytes).
    pub manifest_hash: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ManifestTotals {
    pub stream_count: usize,
    pub duration_ms: Ms,
    pub ambiguous_stream_count: usize,
    pub streams_missing_playlist_url: usize,
    pub streams_missing_recording_time: usize,
    pub streams_with_zero_duration: usize,
}

impl ManifestTotals {
    pub fn duration_hours(&self) -> f64 {
        self.duration_ms as f64 / 3_600_000.0
    }

    /// `1236h 51m 48.000s`, matching how the corpus is quoted upstream.
    pub fn duration_hms(&self) -> String {
        let total_ms = self.duration_ms;
        let (h, rem) = (total_ms / 3_600_000, total_ms % 3_600_000);
        let (m, rem) = (rem / 60_000, rem % 60_000);
        format!("{h}h {m}m {}.{:03}s", rem / 1000, rem % 1000)
    }
}

pub const MANIFEST_VERSION: u32 = 1;

/// Build the manifest. Pure: same inputs, same bytes.
pub fn build(
    catalog: &[CatalogRow],
    links: &[LinkRow],
    selection: Selection,
    provenance: ManifestProvenance,
) -> CorpusManifest {
    // group links by stream so the association is read from the table that
    // actually has one row per (stream, performance)
    let mut by_stream: BTreeMap<StreamId, Vec<&LinkRow>> = BTreeMap::new();
    for l in links {
        by_stream.entry(l.stream_id).or_default().push(l);
    }

    // a stream is in the corpus iff any of its links names a Tompkins
    // performance — the same predicate the cross-reference used
    let tompkins_ids: BTreeSet<StreamId> = by_stream
        .iter()
        .filter(|(_, ls)| ls.iter().any(|l| is_tompkins(&l.performance_name)))
        .map(|(id, _)| *id)
        .collect();

    let catalog_by_id: BTreeMap<StreamId, &CatalogRow> =
        catalog.iter().map(|r| (r.stream_id, r)).collect();

    let mut streams: Vec<ManifestStream> = Vec::new();
    for stream_id in &tompkins_ids {
        let Some(row) = catalog_by_id.get(stream_id) else {
            continue; // linked but absent from the catalog: reported as drift
        };
        if selection == Selection::East295Stationary && row.stream_name != EAST_295 {
            continue;
        }
        let links = &by_stream[stream_id];

        let mut tompkins_performance_ids: Vec<u32> = links
            .iter()
            .filter(|l| is_tompkins(&l.performance_name))
            .map(|l| l.performance_id)
            .collect();
        tompkins_performance_ids.sort_unstable();
        tompkins_performance_ids.dedup();

        let mut other_performance_names: Vec<String> = links
            .iter()
            .filter(|l| !is_tompkins(&l.performance_name))
            .map(|l| l.performance_name.clone())
            .collect();
        other_performance_names.sort();
        other_performance_names.dedup();

        let tompkins_link_type = if other_performance_names.is_empty() {
            TompkinsLinkType::FullStreamLinked
        } else {
            TompkinsLinkType::FullStreamLinkedAmbiguous
        };

        let recording_time_quality =
            RecordingTimeQuality::from_catalog_source(&row.recording_time_source);
        let stream_start_ms = parse_utc_ms(&row.recording_start_estimate_utc);
        let duration_ms = (row.duration_seconds_estimate.max(0.0) * 1000.0).round() as Ms;

        // Project each Tompkins assignment window onto stream-relative time.
        // Both endpoints are approximations of approximations — the log times
        // are wall clock, the stream start is S3-derived — so nothing here is
        // ever marked confident for this corpus.
        let mut assigned_intervals: Vec<AssignedInterval> = Vec::new();
        if let Some(start) = stream_start_ms {
            for l in links.iter().filter(|l| is_tompkins(&l.performance_name)) {
                let (Some(f), Some(t)) = (
                    parse_utc_ms(&l.first_assignment_utc),
                    parse_utc_ms(&l.last_assignment_utc),
                ) else {
                    continue;
                };
                let lo = (f - start).max(0) as Ms;
                let hi = (t - start).max(0) as Ms;
                assigned_intervals.push(AssignedInterval {
                    performance_id: l.performance_id,
                    start_ms: lo.min(hi),
                    end_ms: lo.max(hi).min(duration_ms.max(lo.max(hi))),
                    confident: recording_time_quality
                        == RecordingTimeQuality::ExactProgramDateTime,
                });
            }
        }
        assigned_intervals.sort_by_key(|i| (i.start_ms, i.end_ms, i.performance_id));

        let source_prefix = if row.s3_uri.is_empty() {
            format!("s3://{SOURCE_BUCKET}/{stream_id}/")
        } else {
            row.s3_uri.clone()
        };

        streams.push(ManifestStream {
            stream_id: *stream_id,
            stream_name: row.stream_name.clone(),
            source_bucket: SOURCE_BUCKET.to_string(),
            source_prefix,
            playlist_url: row.playlist_url.clone(),
            duration_ms,
            estimated_recording_start_utc: non_empty(&row.recording_start_estimate_utc),
            estimated_recording_end_utc: non_empty(&row.recording_end_estimate_utc),
            recording_time_quality,
            tompkins_link_type,
            tompkins_performance_ids,
            other_performance_names,
            assigned_intervals,
            stream_change_log_count: row
                .stream_change_log_count
                .max(links.iter().map(|l| l.stream_change_log_count).max().unwrap_or(0)),
            selected_rendition: None,
            segment_count: None,
            source_manifest_hash: None,
        });
    }

    streams.sort_by_key(|s| s.stream_id);

    let totals = ManifestTotals {
        stream_count: streams.len(),
        duration_ms: streams.iter().map(|s| s.duration_ms).sum(),
        ambiguous_stream_count: streams
            .iter()
            .filter(|s| s.tompkins_link_type == TompkinsLinkType::FullStreamLinkedAmbiguous)
            .count(),
        streams_missing_playlist_url: streams.iter().filter(|s| s.playlist_url.is_empty()).count(),
        streams_missing_recording_time: streams
            .iter()
            .filter(|s| s.estimated_recording_start_utc.is_none())
            .count(),
        streams_with_zero_duration: streams.iter().filter(|s| s.duration_ms == 0).count(),
    };

    let mut manifest = CorpusManifest {
        manifest_version: MANIFEST_VERSION,
        provenance,
        streams,
        totals,
        manifest_hash: String::new(),
    };
    manifest.manifest_hash = blake3::hash(&manifest.canonical_bytes()).to_hex().to_string();
    manifest
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

impl CorpusManifest {
    /// A canonical, line-oriented rendering used only for hashing.
    ///
    /// Hand-written rather than derived from the JSON so that adding a
    /// presentational field to the file does not silently change the identity
    /// of a corpus that has not actually changed.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&format!("version\t{}\n", self.manifest_version));
        s.push_str(&format!("selection\t{}\n", self.provenance.selection.as_str()));
        s.push_str(&format!("filter\t{}\n", self.provenance.performance_filter));
        s.push_str(&format!("catalog\t{}\n", self.provenance.catalog_blake3));
        s.push_str(&format!("links\t{}\n", self.provenance.links_blake3));
        for st in &self.streams {
            s.push_str(&format!(
                "stream\t{}\t{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\n",
                st.stream_id,
                st.stream_name,
                st.source_prefix,
                st.duration_ms,
                st.recording_time_quality,
                st.tompkins_link_type,
                st.tompkins_performance_ids
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                st.other_performance_names.join(","),
                st.selected_rendition.as_deref().unwrap_or("-"),
            ));
            for iv in &st.assigned_intervals {
                s.push_str(&format!(
                    "  interval\t{}\t{}\t{}\t{}\n",
                    iv.performance_id, iv.start_ms, iv.end_ms, iv.confident
                ));
            }
        }
        s.into_bytes()
    }

    pub fn get(&self, stream_id: StreamId) -> Option<&ManifestStream> {
        self.streams
            .binary_search_by_key(&stream_id, |s| s.stream_id)
            .ok()
            .map(|i| &self.streams[i])
    }

    /// Per-source-name rollup, largest first — the table used to pick the
    /// first large-scale ingestion target.
    pub fn by_source_name(&self) -> Vec<(String, usize, f64)> {
        let mut agg: BTreeMap<&str, (usize, Ms)> = BTreeMap::new();
        for s in &self.streams {
            let e = agg.entry(s.stream_name.as_str()).or_insert((0, 0));
            e.0 += 1;
            e.1 += s.duration_ms;
        }
        let mut v: Vec<(String, usize, f64)> = agg
            .into_iter()
            .map(|(n, (c, d))| (n.to_string(), c, d as f64 / 3_600_000.0))
            .collect();
        v.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        v
    }

    pub fn to_stream_records(&self) -> Vec<StreamRecord> {
        self.streams
            .iter()
            .map(|s| StreamRecord {
                stream_id: s.stream_id,
                stream_name: s.stream_name.clone(),
                source_bucket: s.source_bucket.clone(),
                source_prefix: s.source_prefix.clone(),
                selected_rendition: s.selected_rendition.clone(),
                estimated_recording_start_utc: s.estimated_recording_start_utc.clone(),
                estimated_recording_end_utc: s.estimated_recording_end_utc.clone(),
                recording_time_quality: s.recording_time_quality,
                duration_ms: s.duration_ms,
                tompkins_link_type: s.tompkins_link_type,
                performance_ids: s.tompkins_performance_ids.clone(),
                performance_names: s.other_performance_names.clone(),
                assigned_intervals: s.assigned_intervals.clone(),
                stream_change_log_count: s.stream_change_log_count,
                source_manifest_hash: s.source_manifest_hash.clone(),
                ingest_status: IngestStatus::Declared,
            })
            .collect()
    }

    pub fn write_json(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn read_json(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// drift
// ---------------------------------------------------------------------------

/// Discrepancies between the inputs and what a manifest could represent.
///
/// Reported rather than fixed: a stream that is linked to Tompkins but absent
/// from the catalog is a real problem with the evidence, and silently
/// dropping it would hide it.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct DriftReport {
    /// Linked to a Tompkins performance but no catalog row.
    pub linked_without_catalog_row: Vec<StreamId>,
    /// Two catalog rows claiming the same stream id.
    pub duplicate_catalog_rows: Vec<StreamId>,
    /// In the manifest but with no playlist URL to read.
    pub missing_playlist_url: Vec<StreamId>,
    /// In the manifest with a zero or absent duration estimate.
    pub zero_duration: Vec<StreamId>,
    /// Ambiguous membership: also served a non-Tompkins performance.
    pub ambiguous_membership: Vec<(StreamId, Vec<String>)>,
    /// An assignment window that could not be projected onto stream time.
    pub unprojectable_assignments: Vec<StreamId>,
}

pub fn drift(
    catalog: &[CatalogRow],
    links: &[LinkRow],
    manifest: &CorpusManifest,
) -> DriftReport {
    let mut report = DriftReport::default();

    let mut seen: BTreeSet<StreamId> = BTreeSet::new();
    let mut dupes: BTreeSet<StreamId> = BTreeSet::new();
    for row in catalog {
        if !seen.insert(row.stream_id) {
            dupes.insert(row.stream_id);
        }
    }
    report.duplicate_catalog_rows = dupes.into_iter().collect();

    let linked: BTreeSet<StreamId> = links
        .iter()
        .filter(|l| is_tompkins(&l.performance_name))
        .map(|l| l.stream_id)
        .collect();
    report.linked_without_catalog_row = linked.difference(&seen).copied().collect();

    for s in &manifest.streams {
        if s.playlist_url.is_empty() {
            report.missing_playlist_url.push(s.stream_id);
        }
        if s.duration_ms == 0 {
            report.zero_duration.push(s.stream_id);
        }
        if s.tompkins_link_type == TompkinsLinkType::FullStreamLinkedAmbiguous {
            report
                .ambiguous_membership
                .push((s.stream_id, s.other_performance_names.clone()));
        }
        if s.assigned_intervals.is_empty() {
            report.unprojectable_assignments.push(s.stream_id);
        }
    }
    report
}

/// blake3 of a file, for input provenance.
pub fn file_hash(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Default location of the cross-reference outputs produced on 2026-07-23.
pub fn default_catalog_dir() -> PathBuf {
    PathBuf::from(
        "/Users/danielbrewster/.codex/visualizations/2026/07/23/\
         019f8f8d-2b21-71d0-b8d3-a8cb068d11aa/oda-audio-cross-reference/output",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(id: StreamId, name: &str, secs: f64) -> CatalogRow {
        CatalogRow {
            stream_id: id,
            stream_name: name.into(),
            performance_names_raw: String::new(),
            recording_start_estimate_utc: "2021-04-06 16:02:01+00:00".into(),
            recording_end_estimate_utc: "2021-04-07 21:57:54+00:00".into(),
            recording_time_source: "local_segment_inventory".into(),
            duration_seconds_estimate: secs,
            stream_change_log_count: 3,
            playlist_url: format!("https://example.invalid/{id}/stream.m3u8"),
            s3_uri: format!("s3://{SOURCE_BUCKET}/{id}/"),
        }
    }

    fn link(id: StreamId, pid: u32, name: &str) -> LinkRow {
        LinkRow {
            stream_id: id,
            performance_id: pid,
            performance_name: name.into(),
            first_assignment_utc: "2021-04-06 17:02:01+00:00".into(),
            last_assignment_utc: "2021-04-06 18:02:01+00:00".into(),
            stream_change_log_count: 2,
        }
    }

    fn prov(selection: Selection) -> ManifestProvenance {
        ManifestProvenance {
            catalog_path: "catalog.csv".into(),
            catalog_blake3: "aa".into(),
            links_path: "links.csv".into(),
            links_blake3: "bb".into(),
            performance_filter: TOMPKINS_FILTER.into(),
            selection,
        }
    }

    #[test]
    fn selects_only_streams_with_a_tompkins_link() {
        let catalog = vec![cat(1, "A", 10.0), cat(2, "B", 20.0), cat(3, "C", 30.0)];
        let links = vec![
            link(1, 88, "Tompkins Square Park"),
            link(2, 80, "Cuenca"),
            link(3, 1, "Tompkins Square Park – New York, New York"),
        ];
        let m = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        assert_eq!(
            m.streams.iter().map(|s| s.stream_id).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn regenerating_the_manifest_is_byte_identical() {
        // the acceptance criterion for milestone 1: no wall clock, no hash
        // map ordering, no set iteration leaking into the output
        let catalog = vec![cat(9422, EAST_295, 1_289_570.0), cat(9420, "Esperanza", 107_753.0)];
        let links = vec![
            link(9422, 88, "Tompkins Square Park"),
            link(9422, 80, "Cuenca"),
            link(9420, 88, "Tompkins Square Park"),
        ];
        let a = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        let b = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        assert_eq!(a.manifest_hash, b.manifest_hash);
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn input_order_does_not_change_the_hash() {
        let catalog = vec![cat(3, "C", 30.0), cat(1, "A", 10.0), cat(2, "B", 20.0)];
        let links = vec![
            link(2, 88, "Tompkins Square Park"),
            link(1, 98, "Tompkins Square Park"),
            link(3, 1, "Tompkins Square Park"),
        ];
        let forward = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));

        let mut rev_catalog = catalog.clone();
        rev_catalog.reverse();
        let mut rev_links = links.clone();
        rev_links.reverse();
        let backward = build(&rev_catalog, &rev_links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));

        assert_eq!(forward.manifest_hash, backward.manifest_hash);
    }

    #[test]
    fn ambiguous_streams_are_marked_not_dropped() {
        // 9422 served Cuenca before Tompkins: it stays in the high-recall
        // corpus but must never be labelled exclusively Tompkins
        let catalog = vec![cat(9422, EAST_295, 1_289_570.0)];
        let links = vec![
            link(9422, 88, "Tompkins Square Park"),
            link(9422, 80, "Cuenca"),
        ];
        let m = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        let s = &m.streams[0];
        assert_eq!(s.tompkins_link_type, TompkinsLinkType::FullStreamLinkedAmbiguous);
        assert_eq!(s.other_performance_names, vec!["Cuenca".to_string()]);
        assert_eq!(m.totals.ambiguous_stream_count, 1);
    }

    #[test]
    fn assignment_windows_project_to_stream_relative_time() {
        // start 16:02:01, assignment 17:02:01 -> one hour into the stream
        let catalog = vec![cat(9420, "Esperanza", 107_753.0)];
        let links = vec![link(9420, 88, "Tompkins Square Park")];
        let m = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        let iv = &m.streams[0].assigned_intervals[0];
        assert_eq!(iv.start_ms, 3_600_000);
        assert_eq!(iv.end_ms, 7_200_000);
        // S3-derived stream start means this projection is never confident
        assert!(!iv.confident);
    }

    #[test]
    fn no_duplicate_rendition_is_ever_selected() {
        // a frozen manifest holds one entry per stream id, so there is no way
        // to index two renditions of the same stream
        let catalog = vec![cat(9422, EAST_295, 100.0)];
        let links = vec![
            link(9422, 88, "Tompkins Square Park"),
            link(9422, 98, "Tompkins Square Park"),
        ];
        let m = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        assert_eq!(m.streams.len(), 1);
        assert_eq!(m.streams[0].tompkins_performance_ids, vec![88, 98]);
        assert!(m.streams[0].selected_rendition.is_none(), "unverified");
    }

    #[test]
    fn east_295_subset_filters_by_source_name() {
        let catalog = vec![cat(1, EAST_295, 10.0), cat(2, "Victor Esther", 20.0)];
        let links = vec![
            link(1, 88, "Tompkins Square Park"),
            link(2, 88, "Tompkins Square Park"),
        ];
        let m = build(&catalog, &links, Selection::East295Stationary, prov(Selection::East295Stationary));
        assert_eq!(m.streams.len(), 1);
        assert_eq!(m.streams[0].stream_id, 1);
        // a different selection must be a different corpus identity
        let all = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        assert_ne!(m.manifest_hash, all.manifest_hash);
    }

    #[test]
    fn drift_reports_links_without_catalog_rows() {
        let catalog = vec![cat(1, "A", 10.0)];
        let links = vec![
            link(1, 88, "Tompkins Square Park"),
            link(999, 88, "Tompkins Square Park"),
        ];
        let m = build(&catalog, &links, Selection::AllTompkinsLinked, prov(Selection::AllTompkinsLinked));
        let d = drift(&catalog, &links, &m);
        assert_eq!(d.linked_without_catalog_row, vec![999]);
        assert_eq!(m.streams.len(), 1, "unrepresentable stream is not invented");
    }

    #[test]
    fn totals_format_as_hours_minutes_seconds() {
        let t = ManifestTotals {
            duration_ms: (1236 * 3600 + 51 * 60 + 48) * 1000,
            ..Default::default()
        };
        assert_eq!(t.duration_hms(), "1236h 51m 48.000s");
    }
}
