//! HTTP: search, playback resolution, and asset serving.
//!
//! The store runs on its own thread behind a channel. That is partly forced —
//! `anny`'s HNSW graph is held in an `Rc<RefCell<_>>`, so `Store` is not `Send`
//! — and partly the right shape anyway: `fold` is single-writer, and the
//! handoff asks that inference never hold the write lock. One owning thread
//! serialises reads and commits, and everything else talks to it by message.
//!
//! Routes:
//!
//! ```text
//! GET  /                        the search UI
//! GET  /listen/{stream}?t=      the same UI, opened at a moment
//! GET  /api/search?q=…          hybrid search
//! GET  /api/resolve/{stream}?t= playback span for an arbitrary position
//! GET  /api/vocabulary          species list with counts
//! GET  /api/stats               corpus and index status
//! GET  /assets/{file}           compacted audio, with range support
//! ```
//!
//! Asset files are served by `tower-http`'s `ServeDir`, which implements
//! `Range` properly — without it a browser cannot seek without downloading the
//! whole asset.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query as AxQuery, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use tokio::sync::{mpsc, oneshot};

use crate::domain::{Modality, Ms, PlaybackSpan, Record, RecordKey, StreamId};
use crate::manifest::CorpusManifest;
use crate::master::{MasterTimeline, Placement};
use crate::rank::Episode;
use crate::search::{self, Query, SearchResults};
use crate::store::Store;
use crate::timeline::Timeline;

// ---------------------------------------------------------------------------
// store actor
// ---------------------------------------------------------------------------

enum Cmd {
    Search(Query, oneshot::Sender<(SearchResults, Vec<Option<PlaybackSpan>>, Vec<Option<(Ms, Ms)>>)>),
    Episode(Query, oneshot::Sender<Option<(Episode, Option<PlaybackSpan>)>>),
    Resolve(StreamId, Ms, oneshot::Sender<Option<PlaybackSpan>>),
    Record(RecordKey, oneshot::Sender<Option<Record>>),
    Vocabulary(oneshot::Sender<Vec<(String, i64)>>),
    Stats(oneshot::Sender<StoreStats>),
    Commit(Vec<Record>, oneshot::Sender<usize>),
    /// The master axis itself, for drawing the scrubber.
    Master(oneshot::Sender<MasterView>),
    /// A global position -> something playable, plus the other sources that
    /// were recording at that same instant.
    ResolveGlobal(Ms, oneshot::Sender<GlobalResolution>),
    /// Commit whatever prepared batches are complete, without stopping.
    Ingest(StreamId, oneshot::Sender<IngestReport>),
}

/// Result of an incremental ingest.
///
/// The store is single-writer, so watching an index build used to mean
/// stopping the server, committing, and starting it again. The owning thread
/// can do it in place instead: workers stage checksummed batches as they
/// finish, and this picks up whichever are complete. Re-running is free —
/// commits are idempotent by stable key, so an already-ingested batch simply
/// makes no change.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct IngestReport {
    pub stream_id: StreamId,
    pub batches_found: usize,
    pub records_committed: usize,
    pub records_refused: usize,
    pub refusals: Vec<String>,
    pub records_in_store: i64,
    pub coverage: Vec<String>,
}

/// The master axis as the client needs it.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MasterView {
    pub epoch_utc: String,
    pub total_ms: Ms,
    pub recorded_ms: Ms,
    pub indexed_ms: Ms,
    pub lane_count: usize,
    pub placements: Vec<Placement>,
    /// Contiguous playable runs of the axis.
    pub coverage: Vec<(Ms, Ms)>,
    /// Streams that could not be placed on the axis at all.
    pub unplaceable: Vec<StreamId>,
    /// True for this corpus: no position on the axis is exact wall clock.
    pub wall_clock_approximate: bool,
}

/// What a click on the master timeline resolves to.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct GlobalResolution {
    pub global_ms: Ms,
    pub utc: String,
    pub playback: Option<PlaybackSpan>,
    pub stream_id: Option<StreamId>,
    pub stream_ms: Option<Ms>,
    /// Every source recording at this instant, playable first.
    pub candidates: Vec<Placement>,
    /// When the position has no audio, where the next audio starts.
    pub next_audio_global_ms: Option<Ms>,
    pub in_gap: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct StoreStats {
    pub records: i64,
    pub streams: Vec<StreamStats>,
    pub species: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct StreamStats {
    pub stream_id: StreamId,
    pub stream_name: String,
    pub duration_ms: Ms,
    pub recording_time_quality: &'static str,
    pub tompkins_link: String,
    pub modalities: Vec<String>,
    pub timeline_ms: Ms,
    pub gaps: usize,
}

/// Handle to the single store owner.
#[derive(Clone)]
pub struct StoreHandle(mpsc::UnboundedSender<Cmd>);

impl StoreHandle {
    async fn ask<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Cmd) -> Option<T> {
        let (tx, rx) = oneshot::channel();
        self.0.send(make(tx)).ok()?;
        rx.await.ok()
    }

    pub async fn search(
        &self,
        q: Query,
    ) -> Option<(
        SearchResults,
        Vec<Option<PlaybackSpan>>,
        Vec<Option<(Ms, Ms)>>,
    )> {
        self.ask(|tx| Cmd::Search(q, tx)).await
    }
    pub async fn resolve(&self, s: StreamId, t: Ms) -> Option<PlaybackSpan> {
        self.ask(|tx| Cmd::Resolve(s, t, tx)).await.flatten()
    }
    pub async fn record(&self, k: RecordKey) -> Option<Record> {
        self.ask(|tx| Cmd::Record(k, tx)).await.flatten()
    }
    pub async fn vocabulary(&self) -> Vec<(String, i64)> {
        self.ask(|tx| Cmd::Vocabulary(tx)).await.unwrap_or_default()
    }
    pub async fn stats(&self) -> StoreStats {
        self.ask(|tx| Cmd::Stats(tx)).await.unwrap_or_default()
    }
    pub async fn commit(&self, records: Vec<Record>) -> usize {
        self.ask(|tx| Cmd::Commit(records, tx)).await.unwrap_or(0)
    }
    pub async fn master(&self) -> MasterView {
        self.ask(|tx| Cmd::Master(tx)).await.unwrap_or_default()
    }
    pub async fn resolve_global(&self, t: Ms) -> GlobalResolution {
        self.ask(|tx| Cmd::ResolveGlobal(t, tx)).await.unwrap_or_default()
    }
    pub async fn ingest(&self, stream_id: StreamId) -> IngestReport {
        self.ask(|tx| Cmd::Ingest(stream_id, tx)).await.unwrap_or_default()
    }
}

/// Spawn the owning thread. Everything that touches the store happens here.
///
/// The store is *opened inside* the thread rather than passed in: `anny` keeps
/// its HNSW graph in an `Rc<RefCell<_>>`, so a `Store` cannot cross a thread
/// boundary at all. Only the path crosses, which also means there is exactly
/// one moment at which the store is opened, and one thread that can write.
pub fn spawn_store(
    db_path: PathBuf,
    timelines: BTreeMap<StreamId, Timeline>,
    manifest: Option<CorpusManifest>,
    clap_addr: Option<String>,
    prepared_dir: PathBuf,
    master: MasterTimeline,
    unplaceable: Vec<StreamId>,
    ready: oneshot::Sender<i64>,
) -> StoreHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    std::thread::Builder::new()
        .name("bog-store".into())
        .spawn(move || {
            let mut store = Store::open(&db_path);
            let _ = ready.send(store.record_count());
            while let Some(cmd) = rx.blocking_recv() {
                match cmd {
                    Cmd::Search(q, reply) => {
                        let r = search::search(&store, &timelines, clap_addr.as_deref(), &q);
                        // built here, where the timelines are, so each span
                        // carries the precision of the evidence that earned it
                        // rather than a generic stream position
                        let spans = r
                            .episodes
                            .iter()
                            .map(|e| {
                                timelines
                                    .get(&e.stream_id)
                                    .and_then(|t| search::playback_for(t, e))
                            })
                            .collect();
                        // the same episodes as positions on the master axis,
                        // so the client can highlight them on the scrubber
                        let globals = r
                            .episodes
                            .iter()
                            .map(|e| {
                                let p = master.placement(e.stream_id)?;
                                Some((p.to_global_ms(e.best.start_ms), p.to_global_ms(e.end_ms)))
                            })
                            .collect();
                        let _ = reply.send((r, spans, globals));
                    }
                    Cmd::Episode(q, reply) => {
                        let r = search::search(&store, &timelines, clap_addr.as_deref(), &q);
                        let out = r.episodes.into_iter().next().map(|e| {
                            let span = timelines
                                .get(&e.stream_id)
                                .and_then(|t| search::playback_for(t, &e));
                            (e, span)
                        });
                        let _ = reply.send(out);
                    }
                    Cmd::Resolve(s, t, reply) => {
                        let span = timelines.get(&s).and_then(|tl| {
                            tl.playback_span(
                                t,
                                t + 10_000,
                                crate::domain::PrecisionKind::StreamPosition,
                                0,
                            )
                        });
                        let _ = reply.send(span);
                    }
                    Cmd::Record(k, reply) => {
                        let _ = reply.send(store.get(&k));
                    }
                    Cmd::Vocabulary(reply) => {
                        let _ = reply.send(store.species_vocabulary());
                    }
                    Cmd::Stats(reply) => {
                        let mut streams = Vec::new();
                        for (id, tl) in &timelines {
                            let m = manifest.as_ref().and_then(|m| m.get(*id));
                            streams.push(StreamStats {
                                stream_id: *id,
                                stream_name: m
                                    .map(|m| m.stream_name.clone())
                                    .unwrap_or_else(|| format!("stream {id}")),
                                duration_ms: m.map(|m| m.duration_ms).unwrap_or(0),
                                recording_time_quality: m
                                    .map(|m| m.recording_time_quality.label())
                                    .unwrap_or("unknown"),
                                tompkins_link: m
                                    .map(|m| format!("{:?}", m.tompkins_link_type))
                                    .unwrap_or_else(|| "unknown".into()),
                                modalities: store.coverage(*id),
                                timeline_ms: tl.total_ms(),
                                gaps: tl.gap_count(),
                            });
                        }
                        let _ = reply.send(StoreStats {
                            records: store.record_count(),
                            species: store.species_vocabulary().len(),
                            streams,
                        });
                    }
                    Cmd::Master(reply) => {
                        let _ = reply.send(MasterView {
                            epoch_utc: master.epoch_utc.clone(),
                            total_ms: master.total_ms,
                            recorded_ms: master.recorded_ms,
                            indexed_ms: master.indexed_ms,
                            lane_count: master.lane_count,
                            placements: master.placements.clone(),
                            coverage: master.indexed_coverage(),
                            unplaceable: unplaceable.clone(),
                            // every start on this axis is S3-derived, so the
                            // client must not present positions as exact
                            wall_clock_approximate: true,
                        });
                    }
                    Cmd::ResolveGlobal(t, reply) => {
                        let candidates: Vec<Placement> =
                            master.at(t).into_iter().cloned().collect();
                        let mut out = GlobalResolution {
                            global_ms: t,
                            utc: master.to_utc(t),
                            candidates: candidates.clone(),
                            ..Default::default()
                        };
                        // the first playable candidate wins; the rest are
                        // offered so a listener can switch microphone
                        for p in candidates.iter().filter(|p| p.indexed) {
                            let Some(stream_ms) = p.to_stream_ms(t) else { continue };
                            if let Some(tl) = timelines.get(&p.stream_id) {
                                if let Some(span) = tl.playback_span(
                                    stream_ms,
                                    stream_ms + 10_000,
                                    crate::domain::PrecisionKind::StreamPosition,
                                    0,
                                ) {
                                    out.in_gap =
                                        span.precision_kind == crate::domain::PrecisionKind::AcrossGap;
                                    out.stream_id = Some(p.stream_id);
                                    out.stream_ms = Some(stream_ms);
                                    out.playback = Some(span);
                                    break;
                                }
                            }
                        }
                        if out.playback.is_none() {
                            out.next_audio_global_ms = master.next_indexed_after(t);
                        }
                        let _ = reply.send(out);
                    }
                    Cmd::Ingest(stream_id, reply) => {
                        let mut report = IngestReport { stream_id, ..Default::default() };
                        let dir = prepared_dir.join(stream_id.to_string());
                        let Some(tl) = timelines.get(&stream_id) else {
                            report.refusals.push(format!("no timeline for stream {stream_id}"));
                            let _ = reply.send(report);
                            continue;
                        };
                        match crate::prepared::discover(&dir) {
                            Ok((batches, bad)) => {
                                report.batches_found = batches.len();
                                for r in bad {
                                    report.refusals.push(format!("{}: {}", r.what, r.reason));
                                }
                                // one short transaction per batch, as with the
                                // offline committer
                                for b in &batches {
                                    let (records, refused) =
                                        crate::prepared::load_batch(b, tl, stream_id);
                                    let rejected = store.commit_batch(&records);
                                    report.records_committed +=
                                        records.len().saturating_sub(rejected.len());
                                    report.records_refused += refused.len() + rejected.len();
                                    for r in refused.iter().take(3) {
                                        report.refusals.push(format!("{}: {}", r.what, r.reason));
                                    }
                                }
                                store.checkpoint();
                            }
                            Err(e) => report.refusals.push(e),
                        }
                        report.records_in_store = store.record_count();
                        report.coverage = store.coverage(stream_id);
                        let _ = reply.send(report);
                    }
                    Cmd::Commit(records, reply) => {
                        let rejected = store.commit_batch(&records);
                        store.checkpoint();
                        let _ = reply.send(rejected.len());
                    }
                }
            }
        })
        .expect("spawn store thread");
    StoreHandle(tx)
}

// ---------------------------------------------------------------------------
// app
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AppState {
    pub store: StoreHandle,
    pub asset_dir: PathBuf,
    pub index_html: Arc<String>,
}

pub fn router(state: AppState) -> Router {
    let assets = tower_http::services::ServeDir::new(state.asset_dir.clone());
    Router::new()
        .route("/", get(page))
        .route("/listen/{stream_id}", get(page))
        .route("/api/search", get(api_search))
        .route("/api/resolve/{stream_id}", get(api_resolve))
        .route("/api/vocabulary", get(api_vocabulary))
        .route("/api/stats", get(api_stats))
        .route("/api/master", get(api_master))
        .route("/api/master/resolve", get(api_master_resolve))
        .route("/api/ingest/{stream_id}", get(api_ingest))
        .nest_service("/assets", assets)
        .with_state(state)
}

async fn page(State(s): State<AppState>) -> Html<String> {
    Html((*s.index_html).clone())
}

#[derive(serde::Deserialize)]
pub struct SearchParams {
    q: Option<String>,
    limit: Option<usize>,
    modality: Option<String>,
    species: Option<String>,
    stream: Option<StreamId>,
    min_confidence: Option<f32>,
    min_similarity: Option<f32>,
    conjunction: Option<bool>,
    max_per_stream: Option<usize>,
    min_separation_s: Option<u64>,
}

async fn api_search(
    State(s): State<AppState>,
    AxQuery(p): AxQuery<SearchParams>,
) -> impl IntoResponse {
    let text = p.q.unwrap_or_default();
    if text.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "empty query" })))
            .into_response();
    }
    let modalities = p
        .modality
        .as_deref()
        .map(|m| m.split(',').filter_map(Modality::parse).collect())
        .unwrap_or_default();

    let mut diversity = crate::rank::Diversity::default();
    if let Some(n) = p.max_per_stream {
        diversity.max_per_stream = n.max(1);
    }
    if let Some(sec) = p.min_separation_s {
        diversity.min_separation_ms = sec * 1000;
    }

    let q = Query {
        text: text.clone(),
        modalities,
        require_conjunction: p.conjunction.unwrap_or(true),
        species: p.species,
        stream_id: p.stream,
        min_confidence: p.min_confidence.unwrap_or(0.0),
        min_similarity: p.min_similarity.unwrap_or(0.15),
        limit: p.limit.unwrap_or(20).min(100),
        diversity,
        confident_only: false,
    };

    let started = std::time::Instant::now();
    let Some((results, spans, globals)) = s.store.search(q).await else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": "store unavailable" })))
            .into_response();
    };
    let elapsed_ms = started.elapsed().as_millis();

    // hydrate each episode with its evidence and playback span
    let mut out = Vec::new();
    for (i, e) in results.episodes.iter().enumerate() {
        let span = spans.get(i).cloned().flatten();
        let global = globals.get(i).cloned().flatten();
        let mut evidence = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for ev in &e.evidence {
            let enc = format!("{:?}", ev.key);
            if !seen.insert(enc) {
                continue; // one entry per record, listing its rankers
            }
            let rankers: Vec<&str> = e
                .evidence
                .iter()
                .filter(|o| o.key == ev.key)
                .map(|o| o.ranker.as_str())
                .collect();
            let detail = s.store.record(ev.key.clone()).await.map(describe);
            let similarity = rankers
                .contains(&"clap-text")
                .then(|| ev.score + 1.0);
            evidence.push(serde_json::json!({
                "similarity": similarity,
                "modality": ev.key.modality().map(|m| m.as_str()),
                "start_ms": ev.start_ms,
                "end_ms": ev.end_ms,
                "rankers": rankers,
                "score": ev.score,
                "precision": ev.precision_kind.label(),
                "detail": detail,
            }));
        }
        out.push(serde_json::json!({
            "stream_id": e.stream_id,
            "start_ms": e.start_ms,
            "end_ms": e.end_ms,
            "duration_ms": e.duration_ms(),
            "fused_score": e.fused_score,
            "rankers": e.rankers,
            "modalities": e.modalities.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
            "best": {
                "modality": e.best.key.modality().map(|m| m.as_str()),
                "start_ms": e.best.start_ms,
                "precision": e.best.precision_kind.label(),
            },
            "playback": span,
            // position on the master axis, for highlighting the scrubber
            "global_start_ms": global.map(|g| g.0),
            "global_end_ms": global.map(|g| g.1),
            "evidence": evidence,
        }));
    }

    let d = &results.diagnostics;
    Json(serde_json::json!({
        "query": text,
        "elapsed_ms": elapsed_ms,
        "results": out,
        "diagnostics": {
            "searched_tracks": d.inferred_modalities,
            "required_together": d.required_modalities,
            "rankers_run": d.rankers_run.iter().map(|(n, c)| serde_json::json!({"ranker": n, "hits": c})).collect::<Vec<_>>(),
            "rankers_skipped": d.rankers_skipped.iter().map(|(n, e)| serde_json::json!({"ranker": n, "reason": e})).collect::<Vec<_>>(),
            "candidates": d.candidates,
            "weak_hits_dropped": d.weak_hits_dropped,
            "episodes_before_filters": d.episodes_before_filters,
            "episodes_after_conjunction": d.episodes_after_conjunction,
        }
    }))
    .into_response()
}

/// Render a record as the evidence the UI shows.
fn describe(r: Record) -> serde_json::Value {
    match r {
        Record::Speech(s) => serde_json::json!({
            "kind": "speech",
            "text": s.text,
            "language": s.language,
            "transcript_confidence": s.transcript_confidence,
            "no_speech_probability": s.no_speech_probability,
            "machine_generated": true,
            "word_count": s.words.len(),
            "model": s.model.model_name,
        }),
        Record::Bird(b) => serde_json::json!({
            "kind": "bird",
            "common_name": b.common_name,
            "scientific_name": b.scientific_name,
            "confidence": b.confidence,
            "location_prior_used": b.location_prior_used,
            "week_prior_used": b.week_prior_used,
            "model": b.model.model_name,
        }),
        Record::Acoustic(a) => serde_json::json!({
            "kind": "sound",
            "tags": a.zero_shot_tags.iter().map(|t| serde_json::json!({"label": t.label, "score": t.score})).collect::<Vec<_>>(),
            "rms_dbfs": a.rms_dbfs,
            "speech_probability": a.speech_probability,
            "model": a.model.model_name,
        }),
        Record::Segment(_) | Record::Stream(_) => serde_json::json!({ "kind": "other" }),
    }
}

#[derive(serde::Deserialize)]
pub struct ResolveParams {
    t: Option<f64>,
}

async fn api_resolve(
    State(s): State<AppState>,
    AxPath(stream_id): AxPath<StreamId>,
    AxQuery(p): AxQuery<ResolveParams>,
) -> impl IntoResponse {
    let seconds = p.t.unwrap_or(0.0).max(0.0);
    let ms = (seconds * 1000.0) as Ms;
    match s.store.resolve(stream_id, ms).await {
        Some(span) => Json(serde_json::json!({ "playback": span })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "no playable media at that position",
                "stream_id": stream_id,
                "t": seconds,
            })),
        )
            .into_response(),
    }
}

async fn api_vocabulary(State(s): State<AppState>) -> impl IntoResponse {
    let v = s.store.vocabulary().await;
    Json(serde_json::json!({
        "species": v.iter().map(|(n, c)| serde_json::json!({"common_name": n, "detections": c})).collect::<Vec<_>>()
    }))
}

async fn api_stats(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.store.stats().await)
}

/// Pick up whatever the inference workers have finished, without a restart.
async fn api_ingest(
    State(s): State<AppState>,
    AxPath(stream_id): AxPath<StreamId>,
) -> impl IntoResponse {
    Json(s.store.ingest(stream_id).await)
}

async fn api_master(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.store.master().await)
}

async fn api_master_resolve(
    State(s): State<AppState>,
    AxQuery(p): AxQuery<ResolveParams>,
) -> impl IntoResponse {
    let ms = (p.t.unwrap_or(0.0).max(0.0) * 1000.0) as Ms;
    Json(s.store.resolve_global(ms).await)
}
