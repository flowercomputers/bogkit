//! Rank fusion, temporal episode grouping, and diversification.
//!
//! Three problems, in order.
//!
//! **Incommensurable scores.** BM25 relevance, CLAP cosine distance and
//! BirdNET confidence do not live on comparable scales, and no amount of
//! normalisation makes them so — a BM25 score of 8.0 says nothing about
//! whether it beats a cosine distance of 0.31. So fusion uses *rank* only,
//! via reciprocal rank fusion. Raw scores are carried through for display and
//! for filtering, never for cross-ranker comparison.
//!
//! **Overlapping windows.** CLAP windows overlap by design (10 s window, 5 s
//! hop), so one siren produces three or four adjacent hits. Returned raw,
//! a single event fills the page. Hits are merged into [`Episode`]s spanning
//! contiguous time, and the strongest evidence inside an episode chooses the
//! jump point while the rest survives as supporting badges.
//!
//! **Compound queries.** "a cardinal while it is raining" must mean bird
//! evidence *and* rain evidence in the same temporal neighbourhood, not the
//! union of two unrelated result lists. Because episodes are built from time,
//! that requirement is expressible as a filter on an episode's modality set.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{Modality, Ms, PrecisionKind, RecordKey, StreamId};
use crate::store::{RankedHit, Ranker};

/// Reciprocal-rank-fusion constant. 60 is the value from the original paper;
/// it dampens the head so one ranker cannot dominate the fused list.
pub const RRF_K: f64 = 60.0;

/// How close two pieces of evidence must be to count as the same event.
///
/// A CLAP hop is 5 s, so anything under that would split one continuous sound
/// across episodes; much more and genuinely separate events merge.
pub const EPISODE_TOLERANCE_MS: Ms = 6_000;

/// One ranker's hit, with its position in that ranker's list.
#[derive(Clone, PartialEq, Debug)]
pub struct Candidate {
    pub key: RecordKey,
    pub ranker: Ranker,
    pub rank: usize,
    pub score: f64,
}

/// One piece of evidence inside an episode.
#[derive(Clone, PartialEq, Debug)]
pub struct Evidence {
    pub key: RecordKey,
    pub start_ms: Ms,
    pub end_ms: Ms,
    pub ranker: Ranker,
    pub rank: usize,
    /// The ranker's own score, for display only.
    pub score: f64,
    /// This record's total fused contribution.
    pub fused: f64,
    pub precision_kind: PrecisionKind,
}

/// A merged temporal event: what the user actually sees as one result.
#[derive(Clone, PartialEq, Debug)]
pub struct Episode {
    pub stream_id: StreamId,
    pub start_ms: Ms,
    pub end_ms: Ms,
    /// Every hit that supported this episode, best first.
    pub evidence: Vec<Evidence>,
    /// The evidence that chooses the jump point.
    pub best: Evidence,
    pub fused_score: f64,
    pub rankers: BTreeSet<&'static str>,
    pub modalities: BTreeSet<Modality>,
}

impl Episode {
    pub fn duration_ms(&self) -> Ms {
        self.end_ms.saturating_sub(self.start_ms)
    }

    pub fn has_modality(&self, m: Modality) -> bool {
        self.modalities.contains(&m)
    }
}

/// Turn one ranker's hit list into candidates, numbering ranks from 0.
pub fn candidates(ranker: Ranker, hits: &[RankedHit]) -> Vec<Candidate> {
    hits.iter()
        .enumerate()
        .map(|(rank, h)| Candidate {
            key: h.key.clone(),
            ranker,
            rank,
            score: h.score,
        })
        .collect()
}

/// Fuse candidate lists by reciprocal rank.
///
/// Returns each record's fused weight plus the candidates that produced it.
/// A record found by several rankers accumulates their contributions, which is
/// what makes agreement across modalities count for something.
pub fn rrf(lists: &[Vec<Candidate>]) -> Vec<(RecordKey, f64, Vec<Candidate>)> {
    let mut acc: BTreeMap<Vec<u8>, (RecordKey, f64, Vec<Candidate>)> = BTreeMap::new();
    for list in lists {
        for c in list {
            let enc = postcard::to_stdvec(&c.key).expect("record keys encode");
            let e = acc
                .entry(enc)
                .or_insert_with(|| (c.key.clone(), 0.0, Vec::new()));
            e.1 += 1.0 / (RRF_K + c.rank as f64 + 1.0);
            e.2.push(c.clone());
        }
    }
    let mut out: Vec<(RecordKey, f64, Vec<Candidate>)> = acc.into_values().collect();
    // ties broken by key so the ordering is deterministic across runs
    out.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)))
    });
    out
}

/// How precisely a record's onset is known, which decides the jump target.
pub fn precision_of(key: &RecordKey, has_words: bool) -> (PrecisionKind, Ms) {
    match key {
        RecordKey::Speech(..) if has_words => (PrecisionKind::AlignedWord, 1_000),
        RecordKey::Speech(..) => (PrecisionKind::Utterance, 2_000),
        RecordKey::Bird(..) => (PrecisionKind::BirdFrame, 3_000),
        RecordKey::Acoustic(..) => (PrecisionKind::AcousticWindow, 5_000),
        _ => (PrecisionKind::StreamPosition, 0),
    }
}

/// Rank precision kinds so a tie in score prefers the tighter jump.
fn precision_order(k: PrecisionKind) -> u8 {
    match k {
        PrecisionKind::AlignedWord => 0,
        PrecisionKind::RefinedAcousticOnset => 1,
        PrecisionKind::Utterance => 2,
        PrecisionKind::BirdFrame => 3,
        PrecisionKind::AcousticWindow => 4,
        PrecisionKind::StreamPosition => 5,
        PrecisionKind::AcrossGap => 6,
    }
}

/// Group fused records into temporal episodes.
///
/// `extent` supplies each record's stream-relative span; a record without one
/// cannot be placed in time and is skipped rather than guessed at.
pub fn episodes(
    fused: &[(RecordKey, f64, Vec<Candidate>)],
    extent: impl Fn(&RecordKey) -> Option<(Ms, Ms, bool)>,
    tolerance_ms: Ms,
) -> Vec<Episode> {
    // collect evidence per stream, in time order
    let mut per_stream: BTreeMap<StreamId, Vec<Evidence>> = BTreeMap::new();
    for (key, fused_score, cands) in fused {
        let Some((start, end, has_words)) = extent(key) else {
            continue;
        };
        let (precision_kind, _) = precision_of(key, has_words);
        // one Evidence per contributing ranker, so the UI can show which
        // indexes agreed
        for c in cands {
            per_stream.entry(key.stream_id()).or_default().push(Evidence {
                key: key.clone(),
                start_ms: start,
                end_ms: end,
                ranker: c.ranker,
                rank: c.rank,
                score: c.score,
                fused: *fused_score,
                precision_kind,
            });
        }
    }

    let mut out: Vec<Episode> = Vec::new();
    for (stream_id, mut items) in per_stream {
        items.sort_by_key(|e| (e.start_ms, e.end_ms));

        let mut group: Vec<Evidence> = Vec::new();
        let mut group_end: Ms = 0;

        for e in items {
            let contiguous = !group.is_empty() && e.start_ms <= group_end + tolerance_ms;
            if contiguous {
                group_end = group_end.max(e.end_ms);
                group.push(e);
            } else {
                if !group.is_empty() {
                    out.push(finish(stream_id, std::mem::take(&mut group)));
                }
                group_end = e.end_ms;
                group.push(e);
            }
        }
        if !group.is_empty() {
            out.push(finish(stream_id, group));
        }
    }

    out.sort_by(|a, b| {
        b.fused_score
            .total_cmp(&a.fused_score)
            .then_with(|| a.stream_id.cmp(&b.stream_id))
            .then_with(|| a.start_ms.cmp(&b.start_ms))
    });
    out
}

fn finish(stream_id: StreamId, mut evidence: Vec<Evidence>) -> Episode {
    // strongest first; a tie prefers the tighter jump target
    evidence.sort_by(|a, b| {
        b.fused
            .total_cmp(&a.fused)
            .then_with(|| precision_order(a.precision_kind).cmp(&precision_order(b.precision_kind)))
            .then_with(|| a.start_ms.cmp(&b.start_ms))
    });

    // An episode's weight is the sum over *distinct records*, not over
    // evidence rows: a record found by three rankers already earned three RRF
    // contributions, and counting its total once per row would multiply them.
    let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut fused_score = 0.0;
    for e in &evidence {
        let enc = postcard::to_stdvec(&e.key).expect("record keys encode");
        if seen.insert(enc) {
            fused_score += e.fused;
        }
    }

    let start_ms = evidence.iter().map(|e| e.start_ms).min().unwrap_or(0);
    let end_ms = evidence.iter().map(|e| e.end_ms).max().unwrap_or(0);
    let rankers = evidence.iter().map(|e| e.ranker.as_str()).collect();
    let modalities = evidence.iter().filter_map(|e| e.key.modality()).collect();
    let best = evidence[0].clone();

    Episode {
        stream_id,
        start_ms,
        end_ms,
        evidence,
        best,
        fused_score,
        rankers,
        modalities,
    }
}

/// Limits on how much of the page one source or one stretch of time may take.
#[derive(Clone, Copy, Debug)]
pub struct Diversity {
    /// Most episodes any one stream may contribute.
    pub max_per_stream: usize,
    /// Minimum separation between two episodes from the same stream.
    pub min_separation_ms: Ms,
}

impl Default for Diversity {
    fn default() -> Self {
        Diversity {
            max_per_stream: 3,
            min_separation_ms: 120_000,
        }
    }
}

/// Apply source and temporal diversity, preserving fused order.
///
/// The per-stream cap scales with how many sources actually have candidates.
/// A fixed cap of 3 is right when a dozen streams are competing for the page,
/// but when only one stream is indexed it silently truncates every search to
/// three results — the cap exists to stop a source *dominating*, and a source
/// cannot dominate when it is the only one.
pub fn diversify(episodes: Vec<Episode>, limit: usize, d: Diversity) -> Vec<Episode> {
    let sources: BTreeSet<StreamId> = episodes.iter().map(|e| e.stream_id).collect();
    let effective_cap = if sources.len() <= 1 {
        limit
    } else {
        // enough for each source to fill its fair share of the page
        d.max_per_stream.max(limit.div_ceil(sources.len()))
    };

    // Scale temporal separation to the span the candidates actually cover.
    //
    // A fixed 120 s is far too tight for a long archive: ten results can all
    // land inside twenty minutes of a 47-hour corpus, which reads as "the same
    // results over and over" even though every record differs. Spreading them
    // over the candidate span instead gives a page that samples the recording
    // rather than one busy stretch of it. The configured value is a floor, so
    // a deliberately tighter setting still wins.
    let span = match (
        episodes.iter().map(|e| e.start_ms).min(),
        episodes.iter().map(|e| e.start_ms).max(),
    ) {
        (Some(lo), Some(hi)) if hi > lo => hi - lo,
        _ => 0,
    };
    let effective_separation = d
        .min_separation_ms
        .max(span / (limit.max(1) as Ms * 4));

    let mut kept: Vec<Episode> = Vec::new();
    let mut per_stream: BTreeMap<StreamId, usize> = BTreeMap::new();

    for e in episodes {
        if kept.len() >= limit {
            break;
        }
        let count = per_stream.get(&e.stream_id).copied().unwrap_or(0);
        if count >= effective_cap {
            continue;
        }
        // too close to something already shown from the same stream: the
        // listener would hear the same moment twice
        let crowded = kept.iter().any(|k| {
            k.stream_id == e.stream_id
                && e.start_ms.abs_diff(k.start_ms) < effective_separation
        });
        if crowded {
            continue;
        }
        per_stream.insert(e.stream_id, count + 1);
        kept.push(e);
    }
    kept
}

/// Keep only episodes carrying evidence from every requested modality.
///
/// This is what makes a compound query mean conjunction in time rather than
/// the union of separate result lists.
pub fn require_modalities(episodes: Vec<Episode>, required: &BTreeSet<Modality>) -> Vec<Episode> {
    if required.len() < 2 {
        return episodes;
    }
    episodes
        .into_iter()
        .filter(|e| required.iter().all(|m| e.has_modality(*m)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(key: RecordKey, score: f64) -> RankedHit {
        RankedHit { key, score }
    }

    /// Every record is 10 s long and has no word alignment unless stated.
    fn ten_second_extent(k: &RecordKey) -> Option<(Ms, Ms, bool)> {
        k.start_ms().map(|s| (s, s + 10_000, false))
    }

    #[test]
    fn fusion_uses_rank_not_score() {
        // a BM25 score of 900 and a cosine-derived score of -0.01 must
        // contribute equally when both are rank 0
        let a = candidates(
            Ranker::SpeechBm25,
            &[hit(RecordKey::Speech(1, 0), 900.0)],
        );
        let b = candidates(
            Ranker::ClapText,
            &[hit(RecordKey::Acoustic(1, 50_000), -0.01)],
        );
        let fused = rrf(&[a, b]);
        assert_eq!(fused.len(), 2);
        assert!(
            (fused[0].1 - fused[1].1).abs() < 1e-12,
            "wildly different raw scores at the same rank must fuse equally"
        );
    }

    #[test]
    fn agreement_across_rankers_outranks_a_single_strong_hit() {
        let agreed = RecordKey::Acoustic(1, 0);
        let alone = RecordKey::Acoustic(1, 500_000);
        let fused = rrf(&[
            candidates(Ranker::ClapText, &[hit(agreed.clone(), -0.1), hit(alone.clone(), -0.2)]),
            candidates(Ranker::AcousticTag, &[hit(agreed.clone(), 5.0)]),
        ]);
        assert_eq!(fused[0].0, agreed, "two rankers agreeing should win");
        assert_eq!(fused[0].2.len(), 2, "both contributions recorded");
    }

    #[test]
    fn overlapping_windows_collapse_into_one_episode() {
        // one siren across four overlapping CLAP windows at a 5 s hop
        let hits: Vec<RankedHit> = (0..4)
            .map(|i| hit(RecordKey::Acoustic(9561, i * 5_000), -0.1 - i as f64 * 0.01))
            .collect();
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);

        assert_eq!(eps.len(), 1, "one event must not fill four result slots");
        assert_eq!(eps[0].start_ms, 0);
        assert_eq!(eps[0].end_ms, 25_000);
        assert_eq!(eps[0].evidence.len(), 4, "supporting evidence is retained");
    }

    #[test]
    fn genuinely_separate_events_stay_separate() {
        let hits = vec![
            hit(RecordKey::Acoustic(9561, 0), -0.1),
            // an hour later: nothing to do with the first
            hit(RecordKey::Acoustic(9561, 3_600_000), -0.11),
        ];
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        assert_eq!(eps.len(), 2);
    }

    #[test]
    fn the_strongest_evidence_chooses_the_jump_point() {
        // a bird frame and an acoustic window in the same neighbourhood; the
        // bird is the better-ranked hit, so it should own the jump
        let bird = RecordKey::Bird(9561, 12_000, "cardinalis_cardinalis".into());
        let sound = RecordKey::Acoustic(9561, 10_000);
        let fused = rrf(&[
            candidates(Ranker::BirdName, &[hit(bird.clone(), 7.0)]),
            candidates(Ranker::ClapText, &[hit(sound.clone(), -0.4)]),
        ]);
        let eps = episodes(
            &fused,
            |k| match k {
                RecordKey::Bird(_, s, _) => Some((*s, s + 3_000, false)),
                RecordKey::Acoustic(_, s) => Some((*s, s + 10_000, false)),
                _ => None,
            },
            EPISODE_TOLERANCE_MS,
        );
        assert_eq!(eps.len(), 1);
        // both rankers contributed to one episode
        assert_eq!(eps[0].modalities.len(), 2);
        assert_eq!(eps[0].best.precision_kind, PrecisionKind::BirdFrame);
        assert_eq!(eps[0].best.start_ms, 12_000);
    }

    #[test]
    fn a_tie_prefers_the_tighter_jump_target() {
        // identical rank in two rankers, so identical fused weight: the
        // aligned-word span should win the jump over the 10 s window
        let speech = RecordKey::Speech(9561, 10_000);
        let sound = RecordKey::Acoustic(9561, 10_000);
        let fused = rrf(&[
            candidates(Ranker::SpeechBm25, &[hit(speech.clone(), 3.0)]),
            candidates(Ranker::ClapText, &[hit(sound.clone(), -0.2)]),
        ]);
        let eps = episodes(
            &fused,
            |k| match k {
                // this speech span has word alignment
                RecordKey::Speech(_, s) => Some((*s, s + 4_000, true)),
                RecordKey::Acoustic(_, s) => Some((*s, s + 10_000, false)),
                _ => None,
            },
            EPISODE_TOLERANCE_MS,
        );
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].best.precision_kind, PrecisionKind::AlignedWord);
    }

    #[test]
    fn compound_queries_require_temporal_overlap() {
        // rain at 30 s with a cardinal beside it; a second cardinal an hour
        // later with no rain. "a cardinal while it is raining" must return only
        // the first.
        let rain = RecordKey::Acoustic(9561, 30_000);
        let bird_near = RecordKey::Bird(9561, 32_000, "cardinalis_cardinalis".into());
        let bird_far = RecordKey::Bird(9561, 3_632_000, "cardinalis_cardinalis".into());

        let fused = rrf(&[
            candidates(Ranker::ClapText, &[hit(rain.clone(), -0.2)]),
            candidates(
                Ranker::BirdName,
                &[hit(bird_near.clone(), 8.0), hit(bird_far.clone(), 7.9)],
            ),
        ]);
        let eps = episodes(
            &fused,
            |k| match k {
                RecordKey::Bird(_, s, _) => Some((*s, s + 3_000, false)),
                RecordKey::Acoustic(_, s) => Some((*s, s + 10_000, false)),
                _ => None,
            },
            EPISODE_TOLERANCE_MS,
        );
        assert_eq!(eps.len(), 2, "two neighbourhoods before the conjunction");

        let required: BTreeSet<Modality> =
            [Modality::Bird, Modality::Sound].into_iter().collect();
        let both = require_modalities(eps, &required);
        assert_eq!(both.len(), 1, "only the neighbourhood with both survives");
        assert_eq!(both[0].start_ms, 30_000);
        assert!(both[0].has_modality(Modality::Bird));
        assert!(both[0].has_modality(Modality::Sound));
    }

    #[test]
    fn a_single_modality_query_is_not_filtered() {
        let fused = rrf(&[candidates(
            Ranker::ClapText,
            &[hit(RecordKey::Acoustic(9561, 0), -0.1)],
        )]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        let required: BTreeSet<Modality> = [Modality::Sound].into_iter().collect();
        assert_eq!(require_modalities(eps, &required).len(), 1);
    }

    #[test]
    fn diversity_stops_one_stream_taking_the_whole_page() {
        // twenty well-separated hits in one stream, plus one in another
        let mut hits: Vec<RankedHit> = (0..20)
            .map(|i| hit(RecordKey::Acoustic(9561, i * 600_000), -0.1 - i as f64 * 0.001))
            .collect();
        hits.push(hit(RecordKey::Acoustic(9258, 0), -0.5));
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        assert_eq!(eps.len(), 21);

        let out = diversify(eps, 10, Diversity::default());
        let from_9561 = out.iter().filter(|e| e.stream_id == 9561).count();
        // two sources competing for ten slots: the busy one is held to its
        // fair share rather than the whole page
        assert!(from_9561 <= 5, "9561 took {from_9561} of 10 slots");
        assert!(from_9561 >= 3, "but is not over-restricted either");
        assert!(out.iter().any(|e| e.stream_id == 9258), "other source shown");
    }

    #[test]
    fn a_single_source_is_not_capped_to_three_results() {
        // The bug a one-stream corpus exposes: every query returned exactly
        // three episodes, because every episode came from the only indexed
        // stream and the per-source cap is 3.
        let hits: Vec<RankedHit> = (0..20)
            .map(|i| hit(RecordKey::Acoustic(9422, i * 600_000), -0.1 - i as f64 * 0.001))
            .collect();
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);

        let out = diversify(eps, 10, Diversity::default());
        assert_eq!(out.len(), 10, "one source should still fill the page");
    }

    #[test]
    fn several_sources_still_share_the_page_fairly() {
        // and the cap still does its job once there is competition
        let mut hits: Vec<RankedHit> = Vec::new();
        for stream in [9422u32, 9561, 9258] {
            for i in 0..20 {
                hits.push(hit(
                    RecordKey::Acoustic(stream, i * 600_000),
                    -0.1 - (stream as f64 * 0.001) - i as f64 * 0.0001,
                ));
            }
        }
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        let out = diversify(eps, 12, Diversity::default());

        let mut per: BTreeMap<StreamId, usize> = BTreeMap::new();
        for e in &out {
            *per.entry(e.stream_id).or_default() += 1;
        }
        assert!(per.len() >= 2, "several sources represented");
        // 12 slots over 3 sources: nobody takes more than its fair share
        for (stream, n) in &per {
            assert!(*n <= 4, "stream {stream} took {n} of 12 slots");
        }
    }

    #[test]
    fn results_spread_across_a_long_archive_rather_than_one_stretch() {
        // 40 candidates spread over 24 hours, but clustered 10-per-hour in four
        // separate hours. With a fixed 120 s separation the page fills from the
        // first cluster; scaled to the span it should sample all of them.
        let mut hits: Vec<RankedHit> = Vec::new();
        for (ci, base) in [0u64, 8, 16, 23].iter().enumerate() {
            for i in 0..10u64 {
                hits.push(hit(
                    RecordKey::Acoustic(9422, base * 3_600_000 + i * 180_000),
                    -0.1 - ci as f64 * 0.0001 - i as f64 * 0.00001,
                ));
            }
        }
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        let out = diversify(eps, 8, Diversity::default());

        // how many distinct hours of the recording are represented?
        let hours: BTreeSet<u64> = out.iter().map(|e| e.start_ms / 3_600_000).collect();
        assert!(
            hours.len() >= 3,
            "a page should sample the archive, not one stretch of it; got hours {hours:?}"
        );
    }

    #[test]
    fn an_explicitly_tight_separation_is_still_honoured() {
        // the configured value is a floor, not a suggestion to be overridden
        let hits: Vec<RankedHit> = (0..10)
            .map(|i| hit(RecordKey::Acoustic(9422, i * 3_600_000), -0.1 - i as f64 * 0.001))
            .collect();
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        // 6 h apart demanded: only every other hit qualifies
        let out = diversify(
            eps,
            10,
            Diversity { max_per_stream: 10, min_separation_ms: 6 * 3_600_000 },
        );
        assert!(out.len() <= 2, "an explicit 6 h floor must be respected");
    }

    #[test]
    fn diversity_suppresses_near_duplicates_in_time() {
        // Hits 30 s apart survive episode grouping — 30 s exceeds the 6 s
        // merge tolerance — but to a listener they are the same stretch of
        // the recording. Separation is an exclusive bound, so a hit at exactly
        // min_separation_ms is far enough away to keep; only the ones strictly
        // inside the window are suppressed.
        let hits: Vec<RankedHit> = (0..5)
            .map(|i| hit(RecordKey::Acoustic(9561, i * 30_000), -0.1 - i as f64 * 0.01))
            .collect();
        let fused = rrf(&[candidates(Ranker::ClapText, &hits)]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        assert_eq!(eps.len(), 5, "30 s apart exceeds the merge tolerance");

        let d = Diversity { max_per_stream: 10, min_separation_ms: 120_000 };
        let out = diversify(eps.clone(), 10, d);
        // 0 s is kept; 30/60/90 s are inside the window; 120 s is exactly at it
        assert_eq!(
            out.iter().map(|e| e.start_ms).collect::<Vec<_>>(),
            vec![0, 120_000]
        );

        // widening the window past the last hit collapses them all to one
        let tighter = diversify(
            eps,
            10,
            Diversity { max_per_stream: 10, min_separation_ms: 120_001 },
        );
        assert_eq!(tighter.len(), 1);
    }

    #[test]
    fn episode_weight_counts_each_record_once() {
        // a record found by three rankers must not have its total counted
        // three times, which would let one popular record beat a genuine
        // multi-record episode
        let k = RecordKey::Acoustic(9561, 0);
        let fused = rrf(&[
            candidates(Ranker::ClapText, &[hit(k.clone(), -0.1)]),
            candidates(Ranker::AcousticTag, &[hit(k.clone(), 4.0)]),
            candidates(Ranker::ClapAudio, &[hit(k.clone(), -0.2)]),
        ]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].evidence.len(), 3, "three rankers recorded");
        // the record's own fused weight, not three times it
        assert!((eps[0].fused_score - fused[0].1).abs() < 1e-12);
        assert_eq!(eps[0].rankers.len(), 3);
    }

    #[test]
    fn fusion_order_is_deterministic_across_runs() {
        // ties must not depend on hash iteration order, or the same query
        // returns a different page each time
        let hits = vec![
            hit(RecordKey::Acoustic(1, 0), -0.1),
            hit(RecordKey::Acoustic(2, 0), -0.1),
            hit(RecordKey::Acoustic(3, 0), -0.1),
        ];
        let first = rrf(&[candidates(Ranker::ClapText, &hits)]);
        for _ in 0..8 {
            let again = rrf(&[candidates(Ranker::ClapText, &hits)]);
            let a: Vec<_> = first.iter().map(|f| f.0.clone()).collect();
            let b: Vec<_> = again.iter().map(|f| f.0.clone()).collect();
            assert_eq!(a, b);
        }
    }

    #[test]
    fn records_without_a_time_extent_are_skipped_not_guessed() {
        let fused = rrf(&[candidates(
            Ranker::ClapText,
            &[hit(RecordKey::Stream(9561), 1.0)],
        )]);
        let eps = episodes(&fused, ten_second_extent, EPISODE_TOLERANCE_MS);
        assert!(eps.is_empty(), "a record with no time cannot become an episode");
    }
}
