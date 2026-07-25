//! Query orchestration: run the relevant rankers, fuse, group, diversify.
//!
//! A settled text query fans out to every track that could answer it, and the
//! results are combined by rank (see [`crate::rank`]). Two things here are
//! worth calling out.
//!
//! **CLAP text embedding lives in a sidecar.** Text-to-audio search needs the
//! text tower of the same checkpoint that embedded the audio, and that is a
//! Python artifact. `pipelines/clap_server.py` holds it; this module speaks to
//! it over localhost. When the sidecar is down, CLAP ranking is *skipped and
//! reported* rather than silently returning nothing — a query that quietly
//! loses its best ranker looks like an empty archive.
//!
//! **Routing and conjunction are separate questions.** Which tracks to search
//! should be generous, because a missed track is a missed result; which
//! modalities must *co-occur* must be strict, because demanding the wrong
//! conjunction returns nothing. [`Intent`] answers both, and only words that
//! unambiguously name one track can create a requirement — otherwise the bare
//! query "singing", which matches both the bird and sound banks, would demand
//! bird and rain evidence in the same moment.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::domain::{Modality, Ms, PlaybackSpan, Record, RecordKey, StreamId, preroll_for};
use crate::rank::{self, Diversity, Episode};
use crate::store::{CLAP_DIM, Ranker, Store};
use crate::timeline::Timeline;

/// How many hits to pull from each lexical ranker. Vector rankers are capped
/// at `store::POOL` by `anny`'s compile-time `TOP_K`.
pub const LEXICAL_POOL: usize = 50;

#[derive(Clone, Debug)]
pub struct Query {
    pub text: String,
    /// Restrict to these modalities; empty means all.
    pub modalities: BTreeSet<Modality>,
    /// Require every inferred modality to co-occur in one episode.
    pub require_conjunction: bool,
    pub species: Option<String>,
    pub stream_id: Option<StreamId>,
    pub min_confidence: f32,
    /// Minimum CLAP cosine similarity for a vector hit to count.
    ///
    /// Nearest-neighbour search returns `k` results whatever the distances are,
    /// so a query for something the archive does not contain still yields a
    /// full page of the least-dissimilar windows. Measured on this corpus, a
    /// window that earns a zero-shot tag sits at ~0.25 similarity while the
    /// untagged filler sits at 0.005-0.043 — an order of magnitude apart, and
    /// indistinguishable in the results until now.
    pub min_similarity: f32,
    pub limit: usize,
    pub diversity: Diversity,
    /// Exclude streams whose Tompkins membership is ambiguous.
    pub confident_only: bool,
}

impl Default for Query {
    fn default() -> Self {
        Query {
            text: String::new(),
            modalities: BTreeSet::new(),
            require_conjunction: true,
            species: None,
            stream_id: None,
            min_confidence: 0.0,
            min_similarity: 0.15,
            limit: 20,
            diversity: Diversity::default(),
            confident_only: false,
        }
    }
}

/// What ran, what did not, and why — surfaced so a degraded query is visible.
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub rankers_run: Vec<(&'static str, usize)>,
    pub rankers_skipped: Vec<(&'static str, String)>,
    pub candidates: usize,
    /// Vector hits dropped for being too dissimilar. Reported so a thin page
    /// reads as "little matched" rather than "the archive is empty".
    pub weak_hits_dropped: usize,
    pub episodes_before_filters: usize,
    pub episodes_after_conjunction: usize,
    /// Tracks that were searched.
    pub inferred_modalities: Vec<&'static str>,
    /// Modalities required to co-occur, if any.
    pub required_modalities: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct SearchResults {
    pub episodes: Vec<Episode>,
    pub diagnostics: Diagnostics,
}

// ---------------------------------------------------------------------------
// clap sidecar
// ---------------------------------------------------------------------------

/// Minimal localhost JSON POST.
///
/// Hand-rolled rather than pulling an HTTP client: the only peer is a sidecar
/// on 127.0.0.1 that this crate also starts, so connection reuse, TLS and
/// redirects are all irrelevant, and one fewer dependency tree is worth more
/// than the convenience.
fn post_json(addr: &str, path: &str, body: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(|e| e.to_string())?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    let (head, payload) = raw
        .split_once("\r\n\r\n")
        .ok_or("malformed response from clap sidecar")?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("?");
    if status != "200" {
        return Err(format!("clap sidecar returned {status}: {}", payload.trim()));
    }
    Ok(payload.to_string())
}

/// Embed query text with CLAP's text tower.
pub fn clap_embed_text(addr: &str, text: &str) -> Result<[f32; CLAP_DIM], String> {
    let body = serde_json::json!({ "texts": [text] }).to_string();
    let payload = post_json(addr, "/embed_text", &body)?;
    let v: serde_json::Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
    let arr = v["embeddings"][0]
        .as_array()
        .ok_or("clap sidecar returned no embedding")?;
    if arr.len() != CLAP_DIM {
        return Err(format!(
            "clap sidecar returned {} dimensions, expected {CLAP_DIM}",
            arr.len()
        ));
    }
    Ok(std::array::from_fn(|i| arr[i].as_f64().unwrap_or(0.0) as f32))
}

/// Embed an uploaded clip, for audio-example search.
pub fn clap_embed_audio(addr: &str, path: &str) -> Result<[f32; CLAP_DIM], String> {
    let body = serde_json::json!({ "path": path }).to_string();
    let payload = post_json(addr, "/embed_audio", &body)?;
    let v: serde_json::Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
    let arr = v["embeddings"][0]
        .as_array()
        .ok_or("clap sidecar returned no embedding")?;
    if arr.len() != CLAP_DIM {
        return Err(format!("clap sidecar returned {} dimensions", arr.len()));
    }
    Ok(std::array::from_fn(|i| arr[i].as_f64().unwrap_or(0.0) as f32))
}

// ---------------------------------------------------------------------------
// modality inference
// ---------------------------------------------------------------------------

/// Words that mark a query as being about weather or the city soundscape.
const SOUND_WORDS: &[&str] = &[
    "rain", "raining", "rainy", "drizzle", "storm", "thunder", "wind", "windy", "traffic", "car",
    "cars", "siren", "ambulance", "police", "fire", "truck", "bus", "horn", "honking",
    "construction", "drilling", "jackhammer", "crowd", "cheering", "music", "drums", "drumming",
    "guitar", "singing", "dog", "barking", "quiet", "silence", "night", "footsteps", "skateboard",
    "bicycle", "bike", "motorcycle", "helicopter", "airplane", "plane", "bell", "bells", "water",
    "fountain", "laughing", "laughter", "children", "playing", "shouting", "noise", "ambience",
];

/// Words that mark a query as being about birds.
const BIRD_WORDS: &[&str] = &[
    "bird", "birds", "birdsong", "birdcall", "cardinal", "sparrow", "jay", "robin", "starling",
    "pigeon", "pigeons", "hawk", "warbler", "finch", "grackle", "mockingbird", "chickadee",
    "wren", "dove", "crow", "gull", "titmouse", "nuthatch", "woodpecker", "chirping", "singing",
    "calling", "dawn", "chorus", "species",
];

/// Words that mark a query as being about speech.
const SPEECH_WORDS: &[&str] = &[
    "said", "say", "says", "saying", "speak", "speaks", "spoke", "speaking", "speech", "talk",
    "talks", "talking", "tell", "tells", "told", "ask", "asks", "asked", "conversation",
    "conversations", "voice", "voices", "shout", "shouts", "shouted", "word", "words", "phrase",
    "quote", "mention", "mentions", "mentioned", "yelling", "transcript",
];

/// Words that belong to more than one bank, so on their own they say which
/// tracks to *search* but not which evidence must *co-occur*.
const AMBIGUOUS_WORDS: &[&str] = &[
    "singing", "calling", "playing", "species", "dog", "children", "shouting", "night", "dawn",
];

fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// What a query is asking for.
///
/// Two different questions, deliberately answered separately. Conflating them
/// is a bug: "singing" appears in both the bird and sound banks, so treating
/// the routing set as the conjunction set would demand bird *and* sound
/// evidence co-occur for the bare query "singing", which returns nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Intent {
    /// Tracks worth searching. Generous — a missed track is a missed result.
    pub route: BTreeSet<Modality>,
    /// Modalities that must co-occur in one episode. Strict — only set from
    /// words that unambiguously name one track, and only applied when there
    /// are at least two.
    pub conjunction: BTreeSet<Modality>,
}

/// Classify a query into routing and conjunction sets.
pub fn analyze(text: &str) -> Intent {
    let toks = tokens(text);
    let hit = |bank: &[&str]| toks.iter().any(|t| bank.contains(&t.as_str()));
    let unambiguous_hit = |bank: &[&str]| {
        toks.iter()
            .any(|t| bank.contains(&t.as_str()) && !AMBIGUOUS_WORDS.contains(&t.as_str()))
    };

    // The acoustic track is always in play. CLAP is a general representation of
    // *all* audio, including speech and birdsong, so it can answer almost any
    // query — and routing a query exclusively to a track that happens to be
    // unindexed returns nothing at all. "a person talking" once produced zero
    // candidates for exactly that reason, while CLAP would have found talking
    // without needing a transcript.
    let mut route = BTreeSet::from([Modality::Sound]);
    if hit(BIRD_WORDS) {
        route.insert(Modality::Bird);
    }
    if hit(SPEECH_WORDS) {
        route.insert(Modality::Speech);
    }
    if !hit(BIRD_WORDS) && !hit(SOUND_WORDS) && !hit(SPEECH_WORDS) {
        // nothing recognisable: let every track try
        route.extend([Modality::Speech, Modality::Bird]);
    }

    let mut conjunction = BTreeSet::new();
    for (bank, m) in [
        (BIRD_WORDS, Modality::Bird),
        (SOUND_WORDS, Modality::Sound),
        (SPEECH_WORDS, Modality::Speech),
    ] {
        if unambiguous_hit(bank) {
            conjunction.insert(m);
        }
    }
    if conjunction.len() < 2 {
        conjunction.clear(); // nothing to require
    }

    Intent { route, conjunction }
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// Run a text query across every relevant track.
pub fn search(
    store: &Store,
    timelines: &std::collections::BTreeMap<StreamId, Timeline>,
    clap_addr: Option<&str>,
    q: &Query,
) -> SearchResults {
    let mut diag = Diagnostics::default();
    let intent = analyze(&q.text);
    diag.inferred_modalities = intent.route.iter().map(|m| m.as_str()).collect();
    diag.required_modalities = intent.conjunction.iter().map(|m| m.as_str()).collect();

    // an explicit modality filter overrides inference entirely
    let active: BTreeSet<Modality> = if q.modalities.is_empty() {
        intent.route.clone()
    } else {
        q.modalities.clone()
    };

    let mut lists: Vec<Vec<rank::Candidate>> = Vec::new();
    let mut push = |diag: &mut Diagnostics,
                    lists: &mut Vec<Vec<rank::Candidate>>,
                    ranker: Ranker,
                    hits: Vec<crate::store::RankedHit>| {
        diag.rankers_run.push((ranker.as_str(), hits.len()));
        if !hits.is_empty() {
            lists.push(rank::candidates(ranker, &hits));
        }
    };

    if active.contains(&Modality::Speech) {
        push(&mut diag, &mut lists, Ranker::SpeechBm25,
             store.search_speech_text(&q.text, LEXICAL_POOL));
        push(&mut diag, &mut lists, Ranker::SpeechSemantic,
             store.search_speech_semantic(&q.text));
    }
    if active.contains(&Modality::Bird) {
        let text = q.species.clone().unwrap_or_else(|| q.text.clone());
        push(&mut diag, &mut lists, Ranker::BirdName,
             store.search_bird_names(&text, LEXICAL_POOL));
    }
    if active.contains(&Modality::Sound) {
        push(&mut diag, &mut lists, Ranker::AcousticTag,
             store.search_acoustic_tags(&q.text, LEXICAL_POOL));
        match clap_addr {
            Some(addr) => match clap_embed_text(addr, &q.text) {
                Ok(e) => {
                    let all = store.search_clap(&e);
                    let before = all.len();
                    // the store returns -(cosine distance), so similarity is
                    // score + 1
                    let kept: Vec<_> = all
                        .into_iter()
                        .filter(|h| (h.score + 1.0) as f32 >= q.min_similarity)
                        .collect();
                    diag.weak_hits_dropped += before - kept.len();
                    push(&mut diag, &mut lists, Ranker::ClapText, kept)
                }
                // reported, not swallowed: losing CLAP changes what the
                // archive appears to contain
                Err(e) => diag
                    .rankers_skipped
                    .push((Ranker::ClapText.as_str(), e)),
            },
            None => diag.rankers_skipped.push((
                Ranker::ClapText.as_str(),
                "no clap sidecar configured".into(),
            )),
        }
    }

    let fused = rank::rrf(&lists);
    diag.candidates = fused.len();

    // resolve each candidate's time span from its record
    let extent = |key: &RecordKey| -> Option<(Ms, Ms, bool)> {
        let record = store.get(key)?;
        if !passes_filters(&record, q) {
            return None;
        }
        let has_words = matches!(&record, Record::Speech(s) if !s.words.is_empty());
        record.extent().map(|(a, b)| (a, b, has_words))
    };

    let eps = rank::episodes(&fused, extent, rank::EPISODE_TOLERANCE_MS);
    diag.episodes_before_filters = eps.len();

    let eps = if q.require_conjunction && intent.conjunction.len() > 1 {
        rank::require_modalities(eps, &intent.conjunction)
    } else {
        eps
    };
    diag.episodes_after_conjunction = eps.len();

    // an episode that cannot be played is not a result
    let eps: Vec<Episode> = eps
        .into_iter()
        .filter(|e| {
            timelines
                .get(&e.stream_id)
                .and_then(|t| t.resolve_ms(e.best.start_ms))
                .is_some_and(|r| !r.in_gap)
        })
        .collect();

    let episodes = rank::diversify(eps, q.limit, q.diversity);
    SearchResults { episodes, diagnostics: diag }
}

fn passes_filters(record: &Record, q: &Query) -> bool {
    if let Some(sid) = q.stream_id {
        if record.stream_id() != sid {
            return false;
        }
    }
    match record {
        Record::Bird(b) => {
            if b.confidence < q.min_confidence {
                return false;
            }
            if let Some(s) = &q.species {
                let s = s.to_lowercase();
                if !b.common_name.to_lowercase().contains(&s)
                    && !b.scientific_name.to_lowercase().contains(&s)
                {
                    return false;
                }
            }
            true
        }
        Record::Speech(s) => s.transcript_confidence >= q.min_confidence,
        _ => true,
    }
}

/// Build the playback span for an episode's chosen jump point.
pub fn playback_for(timeline: &Timeline, e: &Episode) -> Option<PlaybackSpan> {
    let (kind, precision_ms) =
        rank::precision_of(&e.best.key, e.best.precision_kind == crate::domain::PrecisionKind::AlignedWord);
    let mut span = timeline.playback_span(e.best.start_ms, e.best.end_ms, kind, precision_ms)?;
    span.preroll_ms = preroll_for(span.precision_kind);
    Some(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_queries_require_both_modalities_to_co_occur() {
        let i = analyze("a cardinal while it is raining");
        assert!(i.route.contains(&Modality::Bird), "cardinal is a bird");
        assert!(i.route.contains(&Modality::Sound), "rain is a sound");
        // both words unambiguously name a track, so this is a conjunction
        assert_eq!(i.conjunction.len(), 2);
        assert!(i.conjunction.contains(&Modality::Bird));
        assert!(i.conjunction.contains(&Modality::Sound));
    }

    #[test]
    fn single_topic_queries_require_no_conjunction() {
        // Routing is generous — the acoustic track is always included — but a
        // single-topic query must not demand co-occurrence, or it returns
        // nothing.
        let rain = analyze("heavy rain");
        assert!(rain.route.contains(&Modality::Sound));
        assert!(rain.conjunction.is_empty(), "nothing to require");

        let bird = analyze("Northern Cardinal");
        assert!(bird.route.contains(&Modality::Bird));
        assert!(bird.route.contains(&Modality::Sound), "CLAP can hear it too");
        assert!(
            bird.conjunction.is_empty(),
            "routing to two tracks is not the same as requiring both"
        );
    }

    #[test]
    fn ambiguous_words_route_widely_but_demand_nothing() {
        // "singing" is in both the bird and sound banks. It should search both
        // tracks, but requiring bird AND sound evidence to co-occur would make
        // the query return nothing at all.
        let i = analyze("singing");
        assert!(i.route.contains(&Modality::Bird));
        assert!(i.route.contains(&Modality::Sound));
        assert!(
            i.conjunction.is_empty(),
            "an ambiguous word must not demand co-occurrence, got {:?}",
            i.conjunction
        );

        let j = analyze("someone singing");
        assert!(j.conjunction.is_empty());
    }

    #[test]
    fn speech_queries_route_to_the_speech_track() {
        for q in [
            "what did they say about the bicycle",
            "someone talking",
            "the phrase they used",
            "a conversation",
        ] {
            assert!(
                analyze(q).route.contains(&Modality::Speech),
                "{q:?} should reach the speech track"
            );
        }
    }

    #[test]
    fn speech_plus_sound_is_a_conjunction() {
        // "speech during a siren" from the handoff's compound examples
        let i = analyze("speech during a siren");
        assert_eq!(i.conjunction.len(), 2);
        assert!(i.conjunction.contains(&Modality::Speech));
        assert!(i.conjunction.contains(&Modality::Sound));
    }

    #[test]
    fn the_acoustic_track_is_always_searched() {
        // a speech-flavoured query must still reach CLAP: routing it only to
        // the transcript index returns nothing whenever speech is unindexed,
        // even though CLAP can hear talking without a transcript
        for q in ["a person talking", "what did they say", "Northern Cardinal", "birdsong"] {
            assert!(
                analyze(q).route.contains(&Modality::Sound),
                "{q:?} should still reach the acoustic track"
            );
        }
        // and the extra track is still routed to
        assert!(analyze("a person talking").route.contains(&Modality::Speech));
        assert!(analyze("birdsong").route.contains(&Modality::Bird));
    }

    #[test]
    fn an_unrecognised_query_searches_every_track() {
        // free text with no keyword must not produce an empty ranker set:
        // CLAP and the semantic transcript index can still answer it
        let i = analyze("something strange and unfamiliar");
        assert_eq!(i.route.len(), 3, "search everything rather than nothing");
        assert!(i.conjunction.is_empty());
    }

    #[test]
    fn a_missing_sidecar_is_reported_not_silently_ignored() {
        // pointing at a closed port must surface an error rather than
        // pretending CLAP returned nothing
        let err = clap_embed_text("127.0.0.1:1", "heavy rain").unwrap_err();
        assert!(err.contains("connect"), "got {err}");
    }

    #[test]
    fn bird_confidence_filter_applies() {
        use crate::domain::*;
        let low = Record::Bird(BirdDetectionRecord {
            stream_id: 9561,
            start_ms: 0,
            end_ms: 3_000,
            species_id: "cardinalis_cardinalis".into(),
            scientific_name: "Cardinalis cardinalis".into(),
            common_name: "Northern Cardinal".into(),
            confidence: 0.30,
            birdnet_embedding: None,
            location_prior_used: true,
            week_prior_used: true,
            model: ModelStamp::default(),
            source_span: SourceSpan {
                first_media_sequence: 0,
                last_media_sequence: 1,
                asset_id: None,
                asset_offset_ms: None,
            },
        });
        let strict = Query { min_confidence: 0.5, ..Default::default() };
        assert!(!passes_filters(&low, &strict));
        let lenient = Query { min_confidence: 0.2, ..Default::default() };
        assert!(passes_filters(&low, &lenient));

        // and the species filter matches common or scientific name
        let by_common = Query {
            species: Some("cardinal".into()),
            min_confidence: 0.0,
            ..Default::default()
        };
        assert!(passes_filters(&low, &by_common));
        let wrong = Query {
            species: Some("sparrow".into()),
            min_confidence: 0.0,
            ..Default::default()
        };
        assert!(!passes_filters(&low, &wrong));
    }
}
