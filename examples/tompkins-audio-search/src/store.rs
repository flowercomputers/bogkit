//! The store: one keyed stream, many indexes, one atomic write.
//!
//! See `docs/adr/0001-one-keyed-stream.md`. Every record kind travels through
//! a single [`KeyedStream`], so committing one record updates the acoustic
//! vector index, the transcript postings, the transcript embedding graph, the
//! bird labels and embeddings, the per-stream timeline, the species
//! vocabulary and the record table *in one transaction* — and retracting it
//! removes it from all of them. That is the property this project needs from
//! Bog; vector search alone would not have required it.
//!
//! The pipeline is built from `fn` items rather than capturing closures, which
//! keeps the whole graph type nameable ([`Pipeline`]) and lets the store be an
//! ordinary struct with methods instead of a local variable behind macros.
//!
//! Inference never runs inside a transaction. Workers stage prepared batches
//! outside the store and hand them to [`Store::commit_batch`], which holds the
//! write lock only for as long as the commit takes.

use std::collections::BTreeMap;
use std::path::Path;

use anny::metric::Cosine;
use fold::pipeline::{FilterMap, Keyed, Scored, terminal};
use fold::stream::KeyedStream;

use crate::domain::{
    Modality, Ms, Record, RecordKey, SpeciesId, StreamId,
};

/// CLAP audio/text embedding width.
pub const CLAP_DIM: usize = 512;
/// `ese` transcript embedding width (the `dim-512` feature).
pub const TEXT_DIM: usize = ese::DIMENSIONS;
/// BirdNET embedding width.
pub const BIRD_DIM: usize = 1024;

/// Candidate pool per vector ranker. `TOP_K` is a compile-time constant in
/// `anny`, so this is fixed at build time; it is set well above any display
/// limit because rank fusion needs depth to work with.
pub const POOL: usize = 50;

type ClapIndex = terminal::search::Hnsw<RecordKey, f32, Cosine, CLAP_DIM, 32, POOL, 120, 200, 16>;
type TextIndex = terminal::search::Hnsw<RecordKey, f32, Cosine, TEXT_DIM, 32, POOL, 120, 200, 16>;
type BirdIndex = terminal::search::Hnsw<RecordKey, f32, Cosine, BIRD_DIM, 32, POOL, 120, 200, 16>;

/// What flows into the pipeline.
pub type Delta = Keyed<RecordKey, Record>;

/// One fan-out branch: select the records this index cares about, project them
/// into its input, and drop the rest. The projection must be pure — `fold`
/// re-applies it when cancelling a record, so a branch that consulted the
/// clock or a mutable table would leave uncancelled state behind.
type Branch<O, S> = FilterMap<fn(&Delta) -> Option<O>, S, Delta, O>;

/// The whole graph, as one type.
pub type Pipeline = (
    // key -> record: resolves any hit to its evidence and playback span
    terminal::Table<RecordKey, Record>,
    // general environmental sound
    Branch<Keyed<RecordKey, [f32; CLAP_DIM]>, ClapIndex>,
    // controlled zero-shot acoustic tags, searched lexically
    Branch<Keyed<RecordKey, String>, terminal::search::Bm25<RecordKey, String>>,
    // speech: exact phrase
    Branch<Keyed<RecordKey, String>, terminal::search::Bm25<RecordKey, String>>,
    // speech: paraphrase, embedded inside the pipeline by ese
    Branch<Keyed<RecordKey, [f32; TEXT_DIM]>, TextIndex>,
    // bird species names, common and scientific
    Branch<Keyed<RecordKey, String>, terminal::search::Bm25<RecordKey, String>>,
    // bird audio similarity
    Branch<Keyed<RecordKey, [f32; BIRD_DIM]>, BirdIndex>,
    // stream-relative time index: the basis of episode grouping, compound
    // queries and "what else happened here"
    Branch<Keyed<StreamId, Scored<Ms, RecordKey>>, terminal::KeyedRanked<StreamId, Ms, RecordKey>>,
    // species vocabulary with occurrence counts, for the filter UI
    Branch<String, terminal::Bag<String>>,
    // segment timeline: stream time -> media sequence, for playback resolution
    Branch<Keyed<StreamId, Scored<Ms, u32>>, terminal::KeyedRanked<StreamId, Ms, u32>>,
    // per-stream record census, for coverage reporting
    Branch<Keyed<StreamId, String>, terminal::Multimap<StreamId, String>>,
    terminal::Count,
);

// ---------------------------------------------------------------------------
// branch projections
// ---------------------------------------------------------------------------

fn to_fixed<const N: usize>(v: &[f32]) -> Option<[f32; N]> {
    // a wrong-width embedding is a pipeline bug, not something to pad over:
    // silently reshaping it would put a meaningless point in the graph
    if v.len() != N {
        return None;
    }
    Some(std::array::from_fn(|i| v[i]))
}

fn clap_branch(d: &Delta) -> Option<Keyed<RecordKey, [f32; CLAP_DIM]>> {
    let Record::Acoustic(r) = &d.val else { return None };
    Some(Keyed::new(d.key.clone(), to_fixed(&r.clap_embedding)?))
}

fn tag_branch(d: &Delta) -> Option<Keyed<RecordKey, String>> {
    let Record::Acoustic(r) = &d.val else { return None };
    if r.zero_shot_tags.is_empty() {
        return None;
    }
    // only tags that cleared their tuned threshold reach here; the CLAP
    // embedding remains the primary representation
    let text = r
        .zero_shot_tags
        .iter()
        .map(|t| t.label.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Some(Keyed::new(d.key.clone(), text))
}

fn speech_text_branch(d: &Delta) -> Option<Keyed<RecordKey, String>> {
    let Record::Speech(r) = &d.val else { return None };
    if r.text.trim().is_empty() {
        return None;
    }
    Some(Keyed::new(d.key.clone(), r.text.clone()))
}

fn speech_embedding_branch(d: &Delta) -> Option<Keyed<RecordKey, [f32; TEXT_DIM]>> {
    let Record::Speech(r) = &d.val else { return None };
    if r.text.trim().is_empty() {
        return None;
    }
    // ese is a pure function of the text, which is exactly what retraction
    // requires: cancelling this record re-derives the same vector
    Some(Keyed::new(d.key.clone(), ese::encode_single(&r.text)))
}

/// Generic terms every bird detection is indexed under.
///
/// Without these, "a bird while it is raining" matches nothing: BM25 sees only
/// species names, and "House Sparrow" contains no token "bird". A user asking
/// for *any* bird should reach every detection, so the generic vocabulary is
/// part of the indexed text rather than something the query has to know to
/// avoid.
const BIRD_GENERIC_TERMS: &str = "bird birds birdsong birdcall call song chirp singing";

fn bird_name_branch(d: &Delta) -> Option<Keyed<RecordKey, String>> {
    let Record::Bird(r) = &d.val else { return None };
    Some(Keyed::new(
        d.key.clone(),
        format!(
            "{} {} {BIRD_GENERIC_TERMS}",
            r.common_name, r.scientific_name
        ),
    ))
}

fn bird_embedding_branch(d: &Delta) -> Option<Keyed<RecordKey, [f32; BIRD_DIM]>> {
    let Record::Bird(r) = &d.val else { return None };
    Some(Keyed::new(
        d.key.clone(),
        to_fixed(r.birdnet_embedding.as_deref()?)?,
    ))
}

fn time_branch(d: &Delta) -> Option<Keyed<StreamId, Scored<Ms, RecordKey>>> {
    let (start, _) = d.val.extent()?;
    // segments are indexed separately; this view is analysis evidence only
    if matches!(d.val, Record::Segment(_)) {
        return None;
    }
    Some(Keyed::new(
        d.key.stream_id(),
        Scored::new(start, d.key.clone()),
    ))
}

fn species_branch(d: &Delta) -> Option<String> {
    let Record::Bird(r) = &d.val else { return None };
    Some(r.common_name.clone())
}

fn segment_branch(d: &Delta) -> Option<Keyed<StreamId, Scored<Ms, u32>>> {
    let Record::Segment(r) = &d.val else { return None };
    Some(Keyed::new(
        r.stream_id,
        Scored::new(r.cumulative_start_ms, r.media_sequence),
    ))
}

fn census_branch(d: &Delta) -> Option<Keyed<StreamId, String>> {
    let m = d.key.modality()?;
    Some(Keyed::new(d.key.stream_id(), m.as_str().to_string()))
}

fn pipeline() -> Pipeline {
    (
        terminal::Table::new("records"),
        FilterMap::new(
            clap_branch as fn(&Delta) -> Option<_>,
            ClapIndex::new("clap_vectors", Cosine, 42),
        ),
        FilterMap::new(
            tag_branch as fn(&Delta) -> Option<_>,
            terminal::search::Bm25::new("tag_text"),
        ),
        FilterMap::new(
            speech_text_branch as fn(&Delta) -> Option<_>,
            terminal::search::Bm25::new("speech_text"),
        ),
        FilterMap::new(
            speech_embedding_branch as fn(&Delta) -> Option<_>,
            TextIndex::new("speech_vectors", Cosine, 42),
        ),
        FilterMap::new(
            bird_name_branch as fn(&Delta) -> Option<_>,
            terminal::search::Bm25::new("bird_names"),
        ),
        FilterMap::new(
            bird_embedding_branch as fn(&Delta) -> Option<_>,
            BirdIndex::new("bird_vectors", Cosine, 42),
        ),
        FilterMap::new(
            time_branch as fn(&Delta) -> Option<_>,
            terminal::KeyedRanked::new("by_time"),
        ),
        FilterMap::new(
            species_branch as fn(&Delta) -> Option<_>,
            terminal::Bag::new("species_vocab"),
        ),
        FilterMap::new(
            segment_branch as fn(&Delta) -> Option<_>,
            terminal::KeyedRanked::new("segments_by_time"),
        ),
        FilterMap::new(
            census_branch as fn(&Delta) -> Option<_>,
            terminal::Multimap::new("census"),
        ),
        terminal::Count::new("record_count"),
    )
}

// ---------------------------------------------------------------------------
// store
// ---------------------------------------------------------------------------

/// A hit from one ranker, before fusion.
#[derive(Clone, PartialEq, Debug)]
pub struct RankedHit {
    pub key: RecordKey,
    /// The ranker's own score. Never compared across rankers — see
    /// [`crate::rank`], which fuses by rank precisely because these scales are
    /// incommensurable.
    pub score: f64,
}

/// Which ranker produced a candidate.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ranker {
    SpeechBm25,
    SpeechSemantic,
    ClapText,
    ClapAudio,
    BirdName,
    BirdAudio,
    AcousticTag,
}

impl Ranker {
    pub fn as_str(&self) -> &'static str {
        match self {
            Ranker::SpeechBm25 => "speech-bm25",
            Ranker::SpeechSemantic => "speech-semantic",
            Ranker::ClapText => "clap-text",
            Ranker::ClapAudio => "clap-audio",
            Ranker::BirdName => "bird-name",
            Ranker::BirdAudio => "bird-audio",
            Ranker::AcousticTag => "acoustic-tag",
        }
    }

    pub fn modality(&self) -> Modality {
        match self {
            Ranker::SpeechBm25 | Ranker::SpeechSemantic => Modality::Speech,
            Ranker::ClapText | Ranker::ClapAudio | Ranker::AcousticTag => Modality::Sound,
            Ranker::BirdName | Ranker::BirdAudio => Modality::Bird,
        }
    }
}

/// Why a record was refused at commit time.
#[derive(Clone, PartialEq, Debug)]
pub struct Rejection {
    pub key: RecordKey,
    pub reason: String,
}

pub struct Store {
    st: KeyedStream<RecordKey, Record, Pipeline>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Self {
        Store {
            st: KeyedStream::new(path, pipeline()),
        }
    }

    /// Commit a prepared batch in one transaction.
    ///
    /// Records are validated first and refused individually: a malformed
    /// embedding would otherwise be stored in the record table but be absent
    /// from the vector index, which is exactly the kind of silent partial
    /// state the single-stream design exists to prevent.
    pub fn commit_batch(&mut self, records: &[Record]) -> Vec<Rejection> {
        let mut rejected = Vec::new();
        let mut accepted = Vec::new();
        for r in records {
            match validate(r) {
                Ok(()) => accepted.push(r),
                Err(reason) => rejected.push(Rejection { key: r.key(), reason }),
            }
        }
        self.st.wtx(|tx| {
            for r in accepted {
                tx.upsert(&r.key(), r);
            }
        });
        rejected
    }

    /// Retract records by key. Removes them from every index they reached.
    pub fn retract(&mut self, keys: &[RecordKey]) -> usize {
        self.st.wtx(|tx| keys.iter().filter(|k| tx.remove(k).is_some()).count())
    }

    pub fn get(&self, key: &RecordKey) -> Option<Record> {
        self.st.get(key)
    }

    pub fn record_count(&self) -> i64 {
        self.st.rtx(|(.., count)| count.get())
    }

    /// Exact-phrase speech search.
    pub fn search_speech_text(&self, query: &str, limit: usize) -> Vec<RankedHit> {
        self.st.rtx(|(_, _, _, speech, ..)| {
            speech
                .search(query, limit)
                .into_iter()
                .map(|h| RankedHit { key: h.val, score: h.score })
                .collect()
        })
    }

    /// Paraphrase/semantic speech search over `ese` embeddings.
    pub fn search_speech_semantic(&self, query: &str) -> Vec<RankedHit> {
        let q = ese::encode_single(query);
        self.st.rtx(|(_, _, _, _, vecs, ..)| {
            vecs.search(&q)
                .into_iter()
                // cosine distance: smaller is closer, so invert for a score
                .map(|h| RankedHit { key: h.val, score: -(h.score as f64) })
                .collect()
        })
    }

    /// CLAP search from a query embedding — text-to-audio or audio-to-audio
    /// depending on which CLAP tower produced it.
    pub fn search_clap(&self, embedding: &[f32; CLAP_DIM]) -> Vec<RankedHit> {
        self.st.rtx(|(_, clap, ..)| {
            clap.search(embedding)
                .into_iter()
                .map(|h| RankedHit { key: h.val, score: -(h.score as f64) })
                .collect()
        })
    }

    pub fn search_bird_audio(&self, embedding: &[f32; BIRD_DIM]) -> Vec<RankedHit> {
        self.st.rtx(|(_, _, _, _, _, _, birds, ..)| {
            birds
                .search(embedding)
                .into_iter()
                .map(|h| RankedHit { key: h.val, score: -(h.score as f64) })
                .collect()
        })
    }

    pub fn search_bird_names(&self, query: &str, limit: usize) -> Vec<RankedHit> {
        self.st.rtx(|(_, _, _, _, _, names, ..)| {
            names
                .search(query, limit)
                .into_iter()
                .map(|h| RankedHit { key: h.val, score: h.score })
                .collect()
        })
    }

    pub fn search_acoustic_tags(&self, query: &str, limit: usize) -> Vec<RankedHit> {
        self.st.rtx(|(_, _, tags, ..)| {
            tags.search(query, limit)
                .into_iter()
                .map(|h| RankedHit { key: h.val, score: h.score })
                .collect()
        })
    }

    /// Every piece of analysis evidence in a stream-relative window.
    ///
    /// This is what makes compound queries mean something: "a cardinal while
    /// it is raining" requires bird and rain evidence in the *same* temporal
    /// neighbourhood, not two unrelated result lists.
    pub fn evidence_in_window(
        &self,
        stream_id: StreamId,
        from_ms: Ms,
        to_ms: Ms,
    ) -> Vec<(Ms, RecordKey)> {
        self.st.rtx(|(_, _, _, _, _, _, _, by_time, ..)| {
            by_time
                .range(&stream_id, from_ms..to_ms)
                .map(|(s, _)| (s.score, s.val))
                .collect()
        })
    }

    /// Resolve a stream position to the media sequence that contains it, by
    /// taking the latest segment starting at or before the requested time.
    pub fn segment_at(&self, stream_id: StreamId, at_ms: Ms) -> Option<(Ms, u32)> {
        self.st.rtx(|(.., segments, _, _)| {
            segments
                .range(&stream_id, 0..=at_ms)
                .next_back()
                .map(|(s, _)| (s.score, s.val))
        })
    }

    /// Species vocabulary with occurrence counts, for the filter UI.
    pub fn species_vocabulary(&self) -> Vec<(String, i64)> {
        self.st.rtx(|(_, _, _, _, _, _, _, _, vocab, ..)| {
            let mut v: Vec<(String, i64)> = vocab.iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v
        })
    }

    /// Which modalities have been committed for a stream — the basis of the
    /// coverage report.
    pub fn coverage(&self, stream_id: StreamId) -> Vec<String> {
        self.st
            .rtx(|(.., census, _)| census.get(&stream_id))
    }

    /// Records grouped by modality across a window, for episode building.
    pub fn evidence_by_modality(
        &self,
        stream_id: StreamId,
        from_ms: Ms,
        to_ms: Ms,
    ) -> BTreeMap<&'static str, Vec<(Ms, RecordKey)>> {
        let mut out: BTreeMap<&'static str, Vec<(Ms, RecordKey)>> = BTreeMap::new();
        for (t, k) in self.evidence_in_window(stream_id, from_ms, to_ms) {
            if let Some(m) = k.modality() {
                out.entry(m.as_str()).or_default().push((t, k));
            }
        }
        out
    }

    /// Every key belonging to a stream.
    ///
    /// Scans the record table rather than reconstructing keys, so nothing is
    /// missed because a record type was forgotten by the caller.
    pub fn keys_for_stream(&self, stream_id: StreamId) -> Vec<RecordKey> {
        self.st.rtx(|(records, ..)| {
            records
                .iter()
                .map(|(k, _): (RecordKey, Record)| k)
                .filter(|k| k.stream_id() == stream_id)
                .collect()
        })
    }

    /// Retract an entire stream: every record, from every index.
    ///
    /// This is the deletion guarantee the archive needs — §14 of the handoff
    /// requires retraction at stream, interval and transcript level, and the
    /// single-stream design is what makes it one atomic operation rather than
    /// a cleanup pass over seven indexes that can half-fail.
    pub fn forget_stream(&mut self, stream_id: StreamId) -> usize {
        let keys = self.keys_for_stream(stream_id);
        self.retract(&keys)
    }

    pub fn checkpoint(&mut self) {
        self.st.checkpoint()
    }
}

/// Reject a record that cannot be indexed coherently.
pub fn validate(r: &Record) -> Result<(), String> {
    match r {
        Record::Acoustic(a) => {
            if a.clap_embedding.len() != CLAP_DIM {
                return Err(format!(
                    "clap embedding is {} wide, expected {CLAP_DIM}",
                    a.clap_embedding.len()
                ));
            }
            if a.end_ms <= a.start_ms {
                return Err("acoustic window has non-positive duration".into());
            }
        }
        Record::Bird(b) => {
            if let Some(e) = &b.birdnet_embedding {
                if e.len() != BIRD_DIM {
                    return Err(format!(
                        "birdnet embedding is {} wide, expected {BIRD_DIM}",
                        e.len()
                    ));
                }
            }
            if b.end_ms <= b.start_ms {
                return Err("bird detection has non-positive duration".into());
            }
            if b.species_id.trim().is_empty() {
                return Err("bird detection has no species id".into());
            }
        }
        Record::Speech(s) => {
            if s.utterance_end_ms <= s.utterance_start_ms {
                return Err("speech span has non-positive duration".into());
            }
            // a word outside its utterance means the aligner and the VAD
            // disagree, and sub-second seeking would be wrong
            for w in &s.words {
                if w.start_ms < s.utterance_start_ms || w.end_ms > s.utterance_end_ms {
                    return Err(format!(
                        "word {:?} at {}..{} falls outside its utterance {}..{}",
                        w.text, w.start_ms, w.end_ms, s.utterance_start_ms, s.utterance_end_ms
                    ));
                }
            }
        }
        Record::Segment(g) => {
            if g.cumulative_end_ms < g.cumulative_start_ms {
                return Err("segment ends before it starts".into());
            }
        }
        Record::Stream(_) => {}
    }
    Ok(())
}

/// Normalise a species name into a stable id: the scientific name is stable
/// across BirdNET label revisions in a way the common name is not.
pub fn species_id(scientific_name: &str) -> SpeciesId {
    scientific_name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("tompkins-store-test-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn stamp(version: &str) -> ModelStamp {
        ModelStamp {
            model_name: "test".into(),
            model_version: version.into(),
            checkpoint_hash: format!("ckpt-{version}"),
            config_hash: "cfg".into(),
            input_hash: "audio".into(),
            created_at_utc: "2026-07-25T00:00:00Z".into(),
        }
    }

    fn span() -> SourceSpan {
        SourceSpan {
            first_media_sequence: 0,
            last_media_sequence: 5,
            asset_id: Some("9561-00000".into()),
            asset_offset_ms: Some(0),
        }
    }

    fn acoustic(start: Ms, tags: &[&str], seed: f32, version: &str) -> Record {
        Record::Acoustic(AcousticWindowRecord {
            stream_id: 9561,
            start_ms: start,
            end_ms: start + 10_000,
            clap_embedding: (0..CLAP_DIM).map(|i| seed + i as f32 * 0.001).collect(),
            zero_shot_tags: tags
                .iter()
                .map(|t| AcousticTag { label: (*t).into(), score: 0.7 })
                .collect(),
            rms_dbfs: -30.0,
            speech_probability: 0.1,
            model: stamp(version),
            source_span: span(),
        })
    }

    fn speech(start: Ms, text: &str) -> Record {
        Record::Speech(SpeechSpanRecord {
            stream_id: 9561,
            utterance_start_ms: start,
            utterance_end_ms: start + 4_000,
            text: text.into(),
            language: "en".into(),
            words: vec![],
            speaker_label: None,
            vad_confidence: 0.9,
            transcript_confidence: 0.8,
            no_speech_probability: 0.05,
            model: stamp("v1"),
            source_span: span(),
        })
    }

    fn bird(start: Ms, common: &str, scientific: &str, embed: bool) -> Record {
        Record::Bird(BirdDetectionRecord {
            stream_id: 9561,
            start_ms: start,
            end_ms: start + 3_000,
            species_id: species_id(scientific),
            scientific_name: scientific.into(),
            common_name: common.into(),
            confidence: 0.82,
            birdnet_embedding: embed.then(|| vec![0.5; BIRD_DIM]),
            location_prior_used: true,
            week_prior_used: true,
            model: stamp("v2.4"),
            source_span: span(),
        })
    }

    #[test]
    fn one_write_reaches_every_index_it_should() {
        let mut s = Store::open(tmp("fanout"));
        s.commit_batch(&[
            acoustic(0, &["heavy rain", "traffic"], 0.1, "v1"),
            speech(0, "the dog will not stop barking"),
            bird(1_000, "Northern Cardinal", "Cardinalis cardinalis", true),
        ]);

        assert_eq!(s.record_count(), 3);
        // each modality is reachable from its own ranker
        assert!(!s.search_acoustic_tags("rain", 10).is_empty(), "tag index");
        assert!(!s.search_speech_text("barking dog", 10).is_empty(), "bm25");
        assert!(!s.search_speech_semantic("a noisy animal").is_empty(), "semantic");
        assert!(!s.search_bird_names("cardinal", 10).is_empty(), "bird names");
        assert!(!s.search_bird_audio(&[0.5; BIRD_DIM]).is_empty(), "bird vectors");
        assert!(!s.search_clap(&[0.1; CLAP_DIM]).is_empty(), "clap vectors");
        // and from the shared timeline
        assert_eq!(s.evidence_in_window(9561, 0, 60_000).len(), 3);
        // and the vocabulary picked up the species
        assert_eq!(
            s.species_vocabulary(),
            vec![("Northern Cardinal".to_string(), 1)]
        );
    }

    #[test]
    fn retraction_removes_the_record_from_every_index_at_once() {
        // the guarantee: no ranker may return a hit whose record is gone
        let mut s = Store::open(tmp("retract"));
        let keys = vec![
            RecordKey::Acoustic(9561, 0),
            RecordKey::Speech(9561, 0),
            RecordKey::Bird(9561, 1_000, species_id("Cardinalis cardinalis")),
        ];
        s.commit_batch(&[
            acoustic(0, &["heavy rain"], 0.1, "v1"),
            speech(0, "the dog will not stop barking"),
            bird(1_000, "Northern Cardinal", "Cardinalis cardinalis", true),
        ]);
        assert_eq!(s.retract(&keys), 3);

        assert_eq!(s.record_count(), 0);
        assert!(s.search_acoustic_tags("rain", 10).is_empty());
        assert!(s.search_speech_text("barking", 10).is_empty());
        assert!(s.search_speech_semantic("a noisy animal").is_empty());
        assert!(s.search_bird_names("cardinal", 10).is_empty());
        assert!(s.search_bird_audio(&[0.5; BIRD_DIM]).is_empty());
        assert!(s.search_clap(&[0.1; CLAP_DIM]).is_empty());
        assert!(s.evidence_in_window(9561, 0, 60_000).is_empty());
        assert!(s.species_vocabulary().is_empty(), "vocabulary retracted too");
    }

    #[test]
    fn reprocessing_replaces_evidence_and_leaves_nothing_stale() {
        // the milestone 3 acceptance criterion, and the reason model_version
        // is not part of the stable key
        let mut s = Store::open(tmp("reprocess"));
        s.commit_batch(&[acoustic(0, &["heavy rain", "siren"], 0.1, "v1")]);
        assert!(!s.search_acoustic_tags("siren", 10).is_empty());
        assert_eq!(s.record_count(), 1);

        // same window, new checkpoint, different conclusion
        s.commit_batch(&[acoustic(0, &["wind in trees"], 0.9, "v2")]);

        // still one record, not two
        assert_eq!(s.record_count(), 1);
        // the old tags are gone from the text index
        assert!(
            s.search_acoustic_tags("siren", 10).is_empty(),
            "stale term survived a model upgrade"
        );
        assert!(!s.search_acoustic_tags("wind trees", 10).is_empty());
        // and the vector index points at the new embedding, not the old one
        let near_new = s.search_clap(&[0.9; CLAP_DIM]);
        assert_eq!(near_new.len(), 1);
        let stored = s.get(&RecordKey::Acoustic(9561, 0)).unwrap();
        let Record::Acoustic(a) = stored else { panic!() };
        assert_eq!(a.model.model_version, "v2");
        assert!((a.clap_embedding[0] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn a_malformed_embedding_is_refused_rather_than_half_indexed() {
        let mut s = Store::open(tmp("validate"));
        let mut bad = acoustic(0, &["rain"], 0.1, "v1");
        if let Record::Acoustic(a) = &mut bad {
            a.clap_embedding.truncate(10);
        }
        let rejected = s.commit_batch(&[bad]);
        assert_eq!(rejected.len(), 1);
        assert!(rejected[0].reason.contains("expected 512"));
        // nothing was stored, so there is no record without a vector
        assert_eq!(s.record_count(), 0);
        assert!(s.get(&RecordKey::Acoustic(9561, 0)).is_none());
    }

    #[test]
    fn a_word_outside_its_utterance_is_refused() {
        // misaligned words would make sub-second speech seeking wrong
        let mut s = Store::open(tmp("words"));
        let mut r = speech(10_000, "hello there");
        if let Record::Speech(sp) = &mut r {
            sp.words = vec![WordTiming {
                text: "hello".into(),
                start_ms: 9_000, // before the utterance starts
                end_ms: 9_500,
                confidence: 0.9,
            }];
        }
        let rejected = s.commit_batch(&[r]);
        assert_eq!(rejected.len(), 1);
        assert!(rejected[0].reason.contains("outside its utterance"));
    }

    #[test]
    fn bird_records_without_embeddings_are_still_searchable_by_name() {
        // the first version only stores embeddings for bird-positive regions,
        // so a label-only detection must not be dropped
        let mut s = Store::open(tmp("noembed"));
        s.commit_batch(&[bird(0, "House Sparrow", "Passer domesticus", false)]);
        assert_eq!(s.record_count(), 1);
        assert!(!s.search_bird_names("sparrow", 10).is_empty());
        assert!(s.search_bird_audio(&[0.5; BIRD_DIM]).is_empty());
    }

    #[test]
    fn the_time_index_supports_compound_queries() {
        // "a bird while it is raining" needs both kinds of evidence in one
        // neighbourhood, which is a range scan rather than two result lists
        let mut s = Store::open(tmp("compound"));
        s.commit_batch(&[
            acoustic(30_000, &["heavy rain"], 0.2, "v1"),
            bird(32_000, "Northern Cardinal", "Cardinalis cardinalis", true),
            // a cardinal an hour later, with no rain nearby
            bird(3_632_000, "Northern Cardinal", "Cardinalis cardinalis", true),
        ]);

        let together = s.evidence_by_modality(9561, 25_000, 45_000);
        assert_eq!(together.get("sound").map(Vec::len), Some(1));
        assert_eq!(together.get("bird").map(Vec::len), Some(1));

        let alone = s.evidence_by_modality(9561, 3_625_000, 3_645_000);
        assert_eq!(alone.get("bird").map(Vec::len), Some(1));
        assert!(alone.get("sound").is_none(), "no rain in this neighbourhood");
    }

    #[test]
    fn segment_lookup_finds_the_containing_segment() {
        let mut s = Store::open(tmp("segments"));
        let segs: Vec<Record> = (0..10)
            .map(|n| {
                Record::Segment(SegmentRecord {
                    stream_id: 9561,
                    media_sequence: n,
                    source_object_key: format!("9561/stream_4_{n}.ts"),
                    playlist_duration_ms: 2005,
                    source_pts_start: None,
                    source_pts_end: None,
                    cumulative_start_ms: n as Ms * 2005,
                    cumulative_end_ms: (n as Ms + 1) * 2005,
                    source_etag_or_checksum: None,
                    compacted_asset_id: Some("9561-00000".into()),
                    asset_start_ms: Some(n as Ms * 2005),
                    is_gap: false,
                })
            })
            .collect();
        s.commit_batch(&segs);

        // exactly on a boundary
        assert_eq!(s.segment_at(9561, 4 * 2005), Some((4 * 2005, 4)));
        // inside a segment resolves to that segment, not the next
        assert_eq!(s.segment_at(9561, 4 * 2005 + 900), Some((4 * 2005, 4)));
        // segments do not appear in the analysis-evidence timeline
        assert!(s.evidence_in_window(9561, 0, 60_000).is_empty());
    }

    #[test]
    fn reopening_recovers_every_index_from_disk() {
        let path = tmp("reopen");
        {
            let mut s = Store::open(&path);
            s.commit_batch(&[
                acoustic(0, &["heavy rain"], 0.1, "v1"),
                bird(0, "Blue Jay", "Cyanocitta cristata", true),
                speech(0, "watch out for the bicycle"),
            ]);
            s.checkpoint();
        }
        let s = Store::open(&path);
        assert_eq!(s.record_count(), 3);
        // the HNSW graphs are in-memory and must rebuild from persisted vectors
        assert!(!s.search_clap(&[0.1; CLAP_DIM]).is_empty(), "clap recovered");
        assert!(!s.search_bird_audio(&[0.5; BIRD_DIM]).is_empty(), "bird recovered");
        assert!(!s.search_speech_semantic("mind the bike").is_empty(), "ese recovered");
        assert!(!s.search_speech_text("bicycle", 10).is_empty(), "bm25 recovered");
        assert_eq!(s.evidence_in_window(9561, 0, 10_000).len(), 3);
    }

    #[test]
    fn re_upserting_an_identical_record_does_not_churn() {
        let mut s = Store::open(tmp("idempotent"));
        let r = acoustic(0, &["heavy rain"], 0.1, "v1");
        s.commit_batch(&[r.clone()]);
        s.commit_batch(&[r.clone()]);
        s.commit_batch(&[r]);
        assert_eq!(s.record_count(), 1);
        assert_eq!(s.search_acoustic_tags("rain", 10).len(), 1);
    }

    #[test]
    fn species_ids_are_stable_against_label_formatting() {
        assert_eq!(species_id("Cardinalis cardinalis"), "cardinalis_cardinalis");
        assert_eq!(species_id("  Passer domesticus "), "passer_domesticus");
        // a subspecies or hyphenated epithet still yields one stable token
        assert_eq!(
            species_id("Setophaga coronata coronata"),
            "setophaga_coronata_coronata"
        );
    }
}
