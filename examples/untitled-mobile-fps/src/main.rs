use std::collections::{BTreeMap, HashMap};
use std::path::{Path as FilePath, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anny::metric::{Cosine, L2};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use fjall::Readable;
use fold::pipeline::{Aggregate, FilterMap, FlatMap, Keyed, Map, Scored, terminal};
use fold::stream::{KeyedStream, Stream};
use futures_util::{SinkExt, StreamExt};
use rand::distributions::{Alphanumeric, DistString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, oneshot, watch};
use untitled_mobile_fps::{
    Account, AccountRegistration, AppearanceProfile, AppearanceStatus, ClientMessage, Friend,
    FriendRequest, FriendRequestStatus, INVITE_LIFETIME_MS, InspectorSnapshot, MATCH_RADIUS_METERS,
    MAX_HEALTH, MatchDetail, MatchEvent, MatchHistoryEntry, MatchHistoryEvent, MatchHistoryPage,
    MatchHistoryParticipant, MatchId, MatchInvitation, MatchInvitationStatus, MatchSnapshot,
    MatchStatus, PROTOCOL_VERSION, PlayerId, Presence, ServerInfo, ServerMessage, SocialEvent,
    VISUAL_DIMENSIONS,
};
use uuid::Uuid;

const LOCATION_DIMENSIONS: usize = 3;
const MAX_SEARCH_RESULTS: usize = 10;
const WS_TICKET_LIFETIME_MS: u64 = 60_000;
const DEFAULT_PAGE_LIMIT: usize = 25;
const MAX_PAGE_LIMIT: usize = 100;
const MAX_OUTSTANDING_WS_TICKETS_PER_PLAYER: usize = 8;
const REQUIRED_CALIBRATION_MODEL: &str = "vision-hand-pose-2d-v7";
const APPEARANCE_V2_STORE: &str = "appearances-v2.db";
const APPEARANCE_V3_STORE: &str = "appearances-v3.db";
const APPEARANCE_V3_MIGRATION_MARKER: &str = "appearances-v3.migrated-from-v2";

#[derive(Clone)]
struct AppState {
    store: StoreHandle,
    directed: broadcast::Sender<DirectedMessage>,
    server_info: ServerInfo,
    ws_tickets: Arc<Mutex<HashMap<String, WsTicketRecord>>>,
    nearby_tokens: Arc<Mutex<HashMap<(MatchId, PlayerId), String>>>,
}

#[derive(Debug, Clone)]
struct DirectedMessage {
    player_id: PlayerId,
    message: ServerMessage,
}

#[derive(Clone)]
struct StoreHandle {
    tx: mpsc::Sender<StoreCommand>,
    snapshot_rx: watch::Receiver<InspectorSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccountRecord {
    account: Account,
    token_hash: String,
}

/// Positional Postcard layout used by `appearances-v2.db` before silhouette
/// skins were added. JSON's `#[serde(default)]` does not make a changed struct
/// layout compatible with Postcard, so startup decodes this exact legacy shape
/// and writes the current profile into a new Fold store.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyAppearanceProfileV2 {
    player_id: PlayerId,
    display_name: String,
    generated_description: String,
    embedding_model: String,
    descriptor_model: String,
    whole_body_embedding: Vec<f32>,
    face_embeddings: Vec<Vec<f32>>,
    upper_body_embeddings: Vec<Vec<f32>>,
    lower_body_embeddings: Vec<Vec<f32>>,
    head_accessory_embeddings: Vec<Vec<f32>>,
    silhouette_descriptor: Vec<f32>,
    briefing_thumbnail: Option<String>,
    updated_at_ms: u64,
}

impl From<LegacyAppearanceProfileV2> for AppearanceProfile {
    fn from(profile: LegacyAppearanceProfileV2) -> Self {
        Self {
            player_id: profile.player_id,
            display_name: profile.display_name,
            generated_description: profile.generated_description,
            embedding_model: profile.embedding_model,
            descriptor_model: profile.descriptor_model,
            whole_body_embedding: profile.whole_body_embedding,
            face_embeddings: profile.face_embeddings,
            upper_body_embeddings: profile.upper_body_embeddings,
            lower_body_embeddings: profile.lower_body_embeddings,
            head_accessory_embeddings: profile.head_accessory_embeddings,
            silhouette_descriptor: profile.silhouette_descriptor,
            briefing_thumbnail: profile.briefing_thumbnail,
            skin: None,
            updated_at_ms: profile.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Friendship {
    player_a: PlayerId,
    player_b: PlayerId,
    since_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompletedMatchRecord {
    snapshot: MatchSnapshot,
    hit_totals: BTreeMap<PlayerId, u32>,
}

#[derive(Debug, Clone)]
struct WsTicketRecord {
    player_id: PlayerId,
    match_id: Option<MatchId>,
    expires_at_ms: u64,
}

fn cache_nearby_token(
    tokens: &Mutex<HashMap<(MatchId, PlayerId), String>>,
    match_id: &str,
    player_id: &str,
    peer_id: &str,
    token: String,
) -> Option<String> {
    let mut tokens = tokens.lock().unwrap();
    tokens.insert((match_id.to_string(), player_id.to_string()), token);
    tokens
        .get(&(match_id.to_string(), peer_id.to_string()))
        .cloned()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchHit {
    player_id: PlayerId,
    score: f64,
    source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NearbyHit {
    player_id: PlayerId,
    distance_meters: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    #[serde(flatten)]
    server: ServerInfo,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectorPresenceResponse {
    foreground: bool,
    updated_at_ms: u64,
    has_usable_location: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectorMatchResponse {
    status: untitled_mobile_fps::MatchStatus,
    player_count: usize,
    ready_count: usize,
    briefing_acknowledged_count: usize,
    revision: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectorSnapshotResponse {
    revision: u64,
    presences: Vec<InspectorPresenceResponse>,
    matches: Vec<InspectorMatchResponse>,
    processed_commands: usize,
    search_stats: BTreeMap<String, usize>,
    materialization_counts: BTreeMap<String, usize>,
    index_sizes: BTreeMap<String, usize>,
    search_latency_micros: BTreeMap<String, u64>,
}

impl From<InspectorSnapshot> for InspectorSnapshotResponse {
    fn from(snapshot: InspectorSnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            presences: snapshot
                .presences
                .into_iter()
                .map(|presence| InspectorPresenceResponse {
                    foreground: presence.foreground,
                    updated_at_ms: presence.updated_at_ms,
                    has_usable_location: presence.has_usable_location(),
                })
                .collect(),
            matches: snapshot
                .matches
                .into_iter()
                .map(|match_snapshot| InspectorMatchResponse {
                    status: match_snapshot.status,
                    player_count: match_snapshot.players.len(),
                    ready_count: match_snapshot
                        .players
                        .iter()
                        .filter(|player| player.ready)
                        .count(),
                    briefing_acknowledged_count: match_snapshot
                        .players
                        .iter()
                        .filter(|player| player.briefing_acknowledged)
                        .count(),
                    revision: match_snapshot.revision,
                })
                .collect(),
            processed_commands: snapshot.processed_commands,
            search_stats: snapshot.search_stats,
            materialization_counts: snapshot.materialization_counts,
            index_sizes: snapshot.index_sizes,
            search_latency_micros: snapshot.search_latency_micros,
        }
    }
}

#[derive(Debug, Clone)]
struct ProximityReport {
    distance_meters: Option<f32>,
    received_at_ms: u64,
}

enum StoreCommand {
    CreateAccount {
        handle: String,
        display_name: String,
        token: String,
        reply: oneshot::Sender<Result<Account, String>>,
    },
    Authenticate {
        token_hash: String,
        reply: oneshot::Sender<Option<PlayerId>>,
    },
    GetAccount {
        player_id: PlayerId,
        reply: oneshot::Sender<Option<Account>>,
    },
    UpdateAccount {
        player_id: PlayerId,
        handle: Option<String>,
        display_name: Option<String>,
        reply: oneshot::Sender<Result<Account, String>>,
    },
    FindAccount {
        handle: String,
        reply: oneshot::Sender<Option<Account>>,
    },
    UpsertAppearance {
        profile: AppearanceProfile,
        reply: oneshot::Sender<Result<AppearanceProfile, String>>,
    },
    UpsertPresence {
        presence: Presence,
    },
    ClearPresence {
        player_id: PlayerId,
    },
    SearchAppearance {
        query: String,
        reply: oneshot::Sender<Vec<SearchHit>>,
    },
    SearchNearby {
        player_id: PlayerId,
        reply: oneshot::Sender<Vec<NearbyHit>>,
    },
    MatchNearby {
        player_id: PlayerId,
        reply: oneshot::Sender<Result<MatchSnapshot, String>>,
    },
    ListFriendRequests {
        player_id: PlayerId,
        reply: oneshot::Sender<Vec<FriendRequest>>,
    },
    CreateFriendRequest {
        from_id: PlayerId,
        to_id: PlayerId,
        reply: oneshot::Sender<Result<FriendRequest, String>>,
    },
    ResolveFriendRequest {
        actor_id: PlayerId,
        request_id: String,
        accept: bool,
        reply: oneshot::Sender<Result<FriendRequest, String>>,
    },
    ListFriends {
        player_id: PlayerId,
        reply: oneshot::Sender<Vec<Friend>>,
    },
    RemoveFriend {
        player_id: PlayerId,
        friend_id: PlayerId,
        reply: oneshot::Sender<Result<(), String>>,
    },
    CreateMatch {
        host_id: PlayerId,
        reply: oneshot::Sender<MatchSnapshot>,
    },
    JoinMatch {
        invite_code: String,
        player_id: PlayerId,
        reply: oneshot::Sender<Result<MatchSnapshot, String>>,
    },
    CreateTargetInvitation {
        from_id: PlayerId,
        to_id: PlayerId,
        reply: oneshot::Sender<Result<(MatchInvitation, MatchSnapshot), String>>,
    },
    ListTargetInvitations {
        player_id: PlayerId,
        reply: oneshot::Sender<Vec<MatchInvitation>>,
    },
    ResolveTargetInvitation {
        actor_id: PlayerId,
        invitation_id: String,
        action: InvitationAction,
        reply: oneshot::Sender<Result<(MatchInvitation, Option<MatchSnapshot>), String>>,
    },
    Ready {
        command_id: Uuid,
        match_id: MatchId,
        player_id: PlayerId,
        calibration_model_version: String,
        reply: oneshot::Sender<Result<MatchSnapshot, String>>,
    },
    AcknowledgeBriefing {
        command_id: Uuid,
        match_id: MatchId,
        player_id: PlayerId,
        reply: oneshot::Sender<Result<MatchSnapshot, String>>,
    },
    Proximity {
        command_id: Uuid,
        match_id: MatchId,
        player_id: PlayerId,
        peer_id: PlayerId,
        report: ProximityReport,
    },
    Shot {
        command_id: Uuid,
        match_id: MatchId,
        shooter_id: PlayerId,
        target_id: PlayerId,
        mask_contains_reticle: bool,
        target_score: f32,
        reply: oneshot::Sender<ServerMessage>,
    },
    GetMatch {
        match_id: MatchId,
        requester_id: PlayerId,
        reply: oneshot::Sender<Result<MatchSnapshot, String>>,
    },
    GetMatchDetail {
        match_id: MatchId,
        requester_id: PlayerId,
        reply: oneshot::Sender<Result<MatchDetail, String>>,
    },
    ListHistory {
        player_id: PlayerId,
        cursor: Option<String>,
        limit: usize,
        reply: oneshot::Sender<Result<MatchHistoryPage, String>>,
    },
    GetMatchAppearance {
        requester_id: PlayerId,
        player_id: PlayerId,
        reply: oneshot::Sender<Result<AppearanceProfile, String>>,
    },
}

#[derive(Debug, Clone, Copy)]
enum InvitationAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateAccountRequest {
    handle: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateAccountRequest {
    handle: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DemoSessionRequest {
    display_name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DemoSessionResponse {
    player_id: PlayerId,
    token: String,
    display_name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InviteResponse {
    snapshot: MatchSnapshot,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct JoinRequest {
    #[serde(default, rename = "playerId")]
    _player_id: Option<PlayerId>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchQuery {
    q: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountSearchQuery {
    handle: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateFriendRequestBody {
    #[serde(alias = "targetPlayerId")]
    player_id: Option<PlayerId>,
    handle: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateTargetInvitationBody {
    #[serde(alias = "targetPlayerId")]
    friend_id: PlayerId,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CreateMatchInviteBody {
    target_player_id: Option<PlayerId>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FriendSummaryResponse {
    player_id: PlayerId,
    handle: String,
    display_name: String,
    available: bool,
    last_seen_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FriendRequestSummaryResponse {
    request_id: String,
    sender: FriendSummaryResponse,
    status: FriendRequestStatus,
    created_at_ms: u64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct HistoryQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TicketRequest {
    match_id: Option<MatchId>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TicketResponse {
    ticket: String,
    expires_at_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RealtimeQuery {
    ticket: String,
    match_id: Option<MatchId>,
}

fn main() {
    let data_dir = std::env::var_os("FPS_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data"));
    std::fs::create_dir_all(&data_dir).expect("create FPS_DATA_DIR");
    let server_info = load_server_info(&data_dir).expect("load server identity");

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async move {
        let port = std::env::var("FPS_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(3000);
        let address = format!("0.0.0.0:{port}");
        let listener = tokio::net::TcpListener::bind(&address)
            .await
            .expect("bind server");
        let state = new_state(data_dir, server_info.clone());
        let app = build_router(state);
        println!(
            "{} ({}) · http://{address}/inspector",
            server_info.display_name, server_info.server_id
        );
        axum::serve(listener, app).await.expect("serve");
    });
}

fn new_state(data_dir: PathBuf, server_info: ServerInfo) -> AppState {
    let store = StoreHandle::spawn(data_dir);
    let (directed, _) = broadcast::channel(256);
    AppState {
        store,
        directed,
        server_info,
        ws_tickets: Arc::new(Mutex::new(HashMap::new())),
        nearby_tokens: Arc::new(Mutex::new(HashMap::new())),
    }
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INSPECTOR_HTML) }))
        .route("/health", get(health))
        .route("/inspector", get(|| async { Html(INSPECTOR_HTML) }))
        .route("/v1/accounts", post(create_account))
        .route("/v1/demo/session", post(create_demo_session))
        .route("/v1/me", get(get_me).patch(update_me))
        .route("/v1/players", get(find_account))
        .route("/v1/me/appearance", put(upsert_appearance))
        .route(
            "/v1/me/presence",
            put(upsert_presence).delete(clear_presence),
        )
        .route(
            "/v1/me/friend-requests",
            get(list_friend_requests).post(create_friend_request),
        )
        .route(
            "/v1/friend-requests",
            get(list_friend_requests).post(create_friend_request),
        )
        .route(
            "/v1/me/friend-requests/{id}/accept",
            post(accept_friend_request),
        )
        .route(
            "/v1/friend-requests/{id}/accept",
            post(accept_friend_request),
        )
        .route(
            "/v1/me/friend-requests/{id}/decline",
            post(decline_friend_request),
        )
        .route(
            "/v1/friend-requests/{id}/decline",
            post(decline_friend_request),
        )
        .route("/v1/me/friends", get(list_friends))
        .route("/v1/friends", get(list_friends))
        .route("/v1/me/friends/{id}", axum::routing::delete(remove_friend))
        .route("/v1/friends/{id}", axum::routing::delete(remove_friend))
        .route("/v1/match-invitations", post(create_target_invitation))
        .route("/v1/me/match-invitations", get(list_target_invitations))
        .route(
            "/v1/match-invitations/{id}/accept",
            post(accept_target_invitation),
        )
        .route(
            "/v1/match-invitations/{id}/decline",
            post(decline_target_invitation),
        )
        .route(
            "/v1/match-invitations/{id}",
            axum::routing::delete(cancel_target_invitation),
        )
        .route("/v1/invites", post(create_invite))
        .route("/v1/invites/{code}/join", post(join_invite))
        .route("/v1/match-invites", post(create_match_invite))
        .route("/v1/match-invites/{code}/join", post(join_invite))
        .route("/v1/matches/{id}", get(get_match))
        .route("/v1/me/matches", get(list_history))
        .route("/v1/me/matches/{id}", get(get_match_detail))
        .route("/v1/players/{id}/appearance", get(get_match_appearance))
        .route("/v1/search", get(search_appearance))
        .route("/v1/nearby", get(search_nearby))
        .route("/v1/match/nearby", post(match_nearby))
        .route("/v1/inspector/snapshot", get(inspector_snapshot))
        .route("/v1/realtime/tickets", post(create_realtime_ticket))
        .route("/v1/realtime-ticket", post(create_realtime_ticket))
        .route("/v1/realtime", get(realtime_upgrade))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        server: state.server_info,
    })
}

fn load_server_info(data_dir: &FilePath) -> Result<ServerInfo, std::io::Error> {
    let path = data_dir.join("server-id");
    let server_id = match std::fs::read_to_string(&path) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => {
            let id = Uuid::new_v4().to_string();
            std::fs::write(&path, format!("{id}\n"))?;
            id
        }
    };
    Ok(ServerInfo {
        server_id,
        display_name: std::env::var("FPS_SERVER_NAME")
            .unwrap_or_else(|_| "Untitled FPS Dev Server".into()),
        environment: std::env::var("FPS_ENVIRONMENT").unwrap_or_else(|_| "development".into()),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![
            "accounts".into(),
            "appearance".into(),
            "friends".into(),
            "targetedInvitations".into(),
            "matchHistory".into(),
            "briefing".into(),
            "realtimeTickets".into(),
            format!("calibrationModel:{REQUIRED_CALIBRATION_MODEL}"),
            "bogkitFold".into(),
            "bogkitESE".into(),
            "bogkitANNy".into(),
        ],
        minimum_client_version: std::env::var("FPS_MINIMUM_CLIENT_VERSION")
            .unwrap_or_else(|_| "0.1.0".into()),
    })
}

fn decode_v2_appearance(bytes: &[u8]) -> Result<AppearanceProfile, String> {
    match postcard::from_bytes::<AppearanceProfile>(bytes) {
        Ok(profile) => Ok(profile),
        Err(current_error) => postcard::from_bytes::<LegacyAppearanceProfileV2>(bytes)
            .map(Into::into)
            .map_err(|legacy_error| {
                format!(
                    "appearance row matches neither v2 layout: current={current_error:?}, \
                     legacy={legacy_error:?}"
                )
            }),
    }
}

fn load_v2_appearances(path: &FilePath) -> Result<Vec<(PlayerId, AppearanceProfile)>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let store = fjall::SingleWriterTxDatabase::builder(path)
        .open()
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    let keyed_root = store
        .keyspace("keyed_root", fjall::KeyspaceCreateOptions::default)
        .map_err(|error| format!("open {} keyed_root: {error}", path.display()))?;
    let snapshot = store.read_tx();

    snapshot
        .iter(&keyed_root)
        .map(|entry| {
            let (key, value) = entry
                .into_inner()
                .map_err(|error| format!("read {} row: {error}", path.display()))?;
            let player_id: PlayerId = postcard::from_bytes(&key)
                .map_err(|error| format!("decode {} player key: {error:?}", path.display()))?;
            let profile = decode_v2_appearance(&value).map_err(|error| {
                format!("decode {} player {player_id}: {error}", path.display())
            })?;
            if profile.player_id != player_id {
                return Err(format!(
                    "{} player key {player_id} does not match profile {}",
                    path.display(),
                    profile.player_id
                ));
            }
            Ok((player_id, profile))
        })
        .collect()
}

impl StoreHandle {
    fn spawn(data_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let initial = InspectorSnapshot {
            revision: 0,
            appearances: Vec::new(),
            presences: Vec::new(),
            matches: Vec::new(),
            processed_commands: 0,
            search_stats: BTreeMap::new(),
            materialization_counts: BTreeMap::new(),
            index_sizes: BTreeMap::new(),
            search_latency_micros: BTreeMap::new(),
        };
        let (snapshot_tx, snapshot_rx) = watch::channel(initial);
        std::thread::spawn(move || run_store(data_dir, rx, snapshot_tx));
        Self { tx, snapshot_rx }
    }

    async fn send(&self, command: StoreCommand) -> Result<(), String> {
        self.tx
            .send(command)
            .map_err(|_| "store thread is unavailable".to_string())
    }
}

macro_rules! publish_snapshot {
    ($appearances:expr, $presences:expr, $matches:expr, $processed:expr,
     $accounts:expr, $requests:expr, $friends:expr, $invitations:expr, $history:expr,
     $sender:expr, $revision:expr, $stats:expr, $latency:expr) => {{
        let appearances = $appearances.rtx(|(table, _, _, _)| {
            table
                .iter()
                .map(|(_, value)| value.redacted_for_global_view())
                .collect()
        });
        let presences = $presences.rtx(|(table, _)| {
            table
                .iter()
                .map(|(_, mut value)| {
                    value.latitude = 0.0;
                    value.longitude = 0.0;
                    value.horizontal_accuracy = 0.0;
                    value
                })
                .collect()
        });
        let matches = $matches.rtx(|table| table.iter().map(|(_, value)| value).collect());
        let processed_commands = $processed.rtx(|table| table.iter().count());
        let mut materialization_counts = BTreeMap::new();
        materialization_counts.insert(
            "accounts".into(),
            $accounts.rtx(|(table, _, _)| table.iter().count()),
        );
        materialization_counts.insert(
            "friendRequests".into(),
            $requests.rtx(|table| table.iter().count()),
        );
        materialization_counts.insert(
            "friendships".into(),
            $friends.rtx(|table| table.iter().count()),
        );
        materialization_counts.insert(
            "targetedInvitations".into(),
            $invitations.rtx(|(table, _)| table.iter().count()),
        );
        materialization_counts.insert(
            "completedMatches".into(),
            $history.rtx(|(table, _)| table.iter().count()),
        );
        // Read the live size of each Bog search index straight off its reader so the
        // inspector shows the ANNy HNSWs and BM25 corpus growing (and, after a
        // re-enrollment, holding steady — proof the retraction landed).
        let (bm25_docs, semantic_len, visual_len) = $appearances
            .rtx(|(_, bm25, semantic, visual)| (bm25.doc_count(), semantic.len(), visual.len()));
        let presence_len = $presences.rtx(|(_, hnsw)| hnsw.len());
        let mut index_sizes = BTreeMap::new();
        index_sizes.insert("appearanceBm25Docs".into(), bm25_docs.max(0) as usize);
        index_sizes.insert("appearanceSemanticHnsw".into(), semantic_len);
        index_sizes.insert("appearanceVisualHnsw".into(), visual_len);
        index_sizes.insert("presenceHnsw".into(), presence_len);
        let _ = $sender.send(InspectorSnapshot {
            revision: $revision,
            appearances,
            presences,
            matches,
            processed_commands,
            search_stats: $stats.clone(),
            materialization_counts,
            index_sizes,
            search_latency_micros: $latency.clone(),
        });
    }};
}

fn run_store(
    data_dir: PathBuf,
    rx: mpsc::Receiver<StoreCommand>,
    snapshot_tx: watch::Sender<InspectorSnapshot>,
) {
    let mut accounts = KeyedStream::new(
        data_dir.join("accounts.db"),
        (
            terminal::Table::new("account_table"),
            Map::new(
                |record: &Keyed<PlayerId, AccountRecord>| {
                    Keyed::new(
                        normalize_handle(&record.val.account.handle),
                        record.val.account.player_id.clone(),
                    )
                },
                terminal::Table::new("account_handle_index"),
            ),
            Map::new(
                |record: &Keyed<PlayerId, AccountRecord>| {
                    Keyed::new(
                        record.val.token_hash.clone(),
                        record.val.account.player_id.clone(),
                    )
                },
                terminal::Table::new("account_token_index"),
            ),
        ),
    );
    let appearance_v2_path = data_dir.join(APPEARANCE_V2_STORE);
    let appearance_v3_path = data_dir.join(APPEARANCE_V3_STORE);
    let appearance_migration_marker = data_dir.join(APPEARANCE_V3_MIGRATION_MARKER);
    let v2_appearances = (!appearance_migration_marker.exists())
        .then(|| load_v2_appearances(&appearance_v2_path))
        .transpose()
        .unwrap_or_else(|error| panic!("migrate appearance storage: {error}"))
        .unwrap_or_default();

    let mut appearances = KeyedStream::new(
        &appearance_v3_path,
        (
            terminal::Table::new("appearance_table"),
            Map::new(
                |record: &Keyed<PlayerId, AppearanceProfile>| {
                    Keyed::new(record.key.clone(), record.val.generated_description.clone())
                },
                terminal::search::Bm25::new("appearance_bm25"),
            ),
            Map::new(
                |record: &Keyed<PlayerId, AppearanceProfile>| {
                    Keyed::new(
                        record.key.clone(),
                        ese::encode_single(&record.val.generated_description),
                    )
                },
                terminal::search::Hnsw::<PlayerId, f32, Cosine, { ese::DIMENSIONS }>::new(
                    "appearance_semantic_hnsw",
                    Cosine,
                    42,
                ),
            ),
            Map::new(
                |record: &Keyed<PlayerId, AppearanceProfile>| {
                    Keyed::new(record.key.clone(), record.val.searchable_embedding())
                },
                terminal::search::Hnsw::<PlayerId, f32, Cosine, VISUAL_DIMENSIONS>::new(
                    "appearance_visual_hnsw",
                    Cosine,
                    43,
                ),
            ),
        ),
    );
    if !appearance_migration_marker.exists() {
        // A prior attempt may have committed v3 before it could write the
        // marker. Never overwrite a newer v3 profile with its stale v2 copy.
        let pending: Vec<_> = v2_appearances
            .into_iter()
            .filter(|(player_id, _)| !appearances.contains(player_id))
            .collect();
        let migrated_count = pending.len();
        if !pending.is_empty() {
            appearances.wtx(|tx| {
                for (player_id, profile) in &pending {
                    tx.upsert(player_id, profile);
                }
            });
            appearances.checkpoint();
        }
        std::fs::write(
            &appearance_migration_marker,
            format!("migrated={migrated_count}\n"),
        )
        .unwrap_or_else(|error| {
            panic!(
                "write appearance migration marker {}: {error}",
                appearance_migration_marker.display()
            )
        });
        if migrated_count > 0 {
            println!(
                "Migrated {migrated_count} appearance profile(s) from {} to {}",
                appearance_v2_path.display(),
                appearance_v3_path.display()
            );
        }
    }
    let mut presences = KeyedStream::new(
        data_dir.join("presence-v2.db"),
        (
            terminal::Table::new("presence_table"),
            Map::new(
                |record: &Keyed<PlayerId, Presence>| {
                    Keyed::new(record.key.clone(), record.val.earth_vector())
                },
                terminal::search::Hnsw::<PlayerId, f32, L2, LOCATION_DIMENSIONS>::new(
                    "presence_hnsw",
                    L2,
                    44,
                ),
            ),
        ),
    );
    let mut matches = KeyedStream::new(
        // Protocol v1 snapshots used a different postcard shape. Keep the
        // v2 materialization separate so pre-release data never crashes a
        // protocol-v2 server during deserialization.
        data_dir.join("matches-v2.db"),
        terminal::Table::new("match_table"),
    );
    let mut processed = KeyedStream::new(
        data_dir.join("commands-v2.db"),
        terminal::Table::new("processed_commands"),
    );
    let mut events = Stream::new(
        data_dir.join("events-v2.db"),
        (
            terminal::Bag::<MatchEvent>::new("match_events"),
            FilterMap::new(
                |event: &MatchEvent| match event {
                    MatchEvent::Hit { target_id, .. } => Some(Keyed::new(target_id.clone(), 1i64)),
                    _ => None,
                },
                Aggregate::new(
                    "damage_aggregate",
                    |acc: &mut i64, damage: &i64, delta| {
                        *acc += *damage * delta as i64;
                    },
                    terminal::Table::new("damage_table"),
                ),
            ),
        ),
    );
    let mut social_events = Stream::new(
        data_dir.join("social-events.db"),
        terminal::Bag::<SocialEvent>::new("social_event_log"),
    );
    let mut friend_requests = KeyedStream::new(
        data_dir.join("friend-requests.db"),
        terminal::Table::<String, FriendRequest>::new("friend_request_table"),
    );
    let mut friendships = KeyedStream::new(
        data_dir.join("friendships.db"),
        terminal::Table::<String, Friendship>::new("friendship_table"),
    );
    let mut invitations = KeyedStream::new(
        data_dir.join("targeted-invitations.db"),
        (
            terminal::Table::new("targeted_invitation_table"),
            Map::new(
                |record: &Keyed<String, MatchInvitation>| {
                    Keyed::new(
                        format!("{}:{}", record.val.to_player_id, record.key),
                        record.key.clone(),
                    )
                },
                terminal::Table::new("invitation_recipient_index"),
            ),
        ),
    );
    let mut history = KeyedStream::new(
        data_dir.join("history.db"),
        (
            terminal::Table::new("completed_match_table"),
            FlatMap::new(
                |record: &Keyed<MatchId, CompletedMatchRecord>| {
                    record
                        .val
                        .snapshot
                        .players
                        .iter()
                        .map(|player| {
                            Keyed::new(
                                player.player_id.clone(),
                                Scored::new(
                                    record.val.snapshot.completed_at_ms.unwrap_or_default(),
                                    record.key.clone(),
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                },
                terminal::KeyedRanked::<PlayerId, u64, MatchId>::new("completed_match_player_rank"),
            ),
        ),
    );

    let mut revision = 0u64;
    let mut search_stats = BTreeMap::new();
    let mut search_latency = BTreeMap::<String, u64>::new();
    let mut proximity: HashMap<(MatchId, PlayerId, PlayerId), ProximityReport> = HashMap::new();
    // Players waiting to be matched with a random nearby opponent, each holding the
    // one-player Lobby match they created while they wait. Pairing consults the
    // presence_hnsw for the nearest waiting player within the match radius.
    let mut nearby_queue: Vec<(PlayerId, MatchId)> = Vec::new();

    // Each Fold stream is durable independently. Repair the small number of
    // cross-stream invariants on startup so a process exit between writes is
    // recoverable and retries remain idempotent.
    let accepted_requests = friend_requests.rtx(|table| {
        table
            .iter()
            .map(|(_, request)| request)
            .filter(|request| request.status == FriendRequestStatus::Accepted)
            .collect::<Vec<_>>()
    });
    for request in accepted_requests {
        let key = friendship_key(&request.from_player_id, &request.to_player_id);
        if !friendships.contains(&key) {
            friendships.wtx(|tx| {
                tx.upsert(
                    &key,
                    &Friendship {
                        player_a: request.from_player_id,
                        player_b: request.to_player_id,
                        since_ms: request.updated_at_ms,
                    },
                )
            });
        }
    }
    let pending_invitations = invitations.rtx(|(table, _)| {
        table
            .iter()
            .map(|(_, invitation)| invitation)
            .filter(|invitation| invitation.status == MatchInvitationStatus::Pending)
            .collect::<Vec<_>>()
    });
    for mut invitation in pending_invitations {
        if matches
            .get(&invitation.match_id)
            .is_some_and(|snapshot: MatchSnapshot| {
                snapshot
                    .players
                    .iter()
                    .any(|player| player.player_id == invitation.to_player_id)
            })
        {
            invitation.status = MatchInvitationStatus::Accepted;
            invitations.wtx(|tx| tx.upsert(&invitation.invitation_id, &invitation));
        }
    }
    let accepted_command_ids = events.rtx(|(bag, _)| {
        bag.iter()
            .filter_map(|(event, count)| match event {
                MatchEvent::Hit { command_id, .. } if count > 0 => Some(command_id.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
    });
    for command_id in accepted_command_ids {
        if !processed.contains(&command_id) {
            processed.wtx(|tx| tx.upsert(&command_id, &true));
        }
    }
    let completed_snapshots = matches.rtx(|table| {
        table
            .iter()
            .map(|(_, snapshot)| snapshot)
            .filter(|snapshot| snapshot.status == untitled_mobile_fps::MatchStatus::Completed)
            .collect::<Vec<_>>()
    });
    for snapshot in completed_snapshots {
        if history.contains(&snapshot.match_id) {
            continue;
        }
        let mut hit_totals = BTreeMap::new();
        for shooter in &snapshot.players {
            let hits = snapshot
                .players
                .iter()
                .filter(|target| target.player_id != shooter.player_id)
                .map(|target| u32::from(MAX_HEALTH.saturating_sub(target.health)))
                .sum();
            hit_totals.insert(shooter.player_id.clone(), hits);
        }
        history.wtx(|tx| {
            tx.upsert(
                &snapshot.match_id,
                &CompletedMatchRecord {
                    snapshot: snapshot.clone(),
                    hit_totals,
                },
            )
        });
    }

    publish_snapshot!(
        appearances,
        presences,
        matches,
        processed,
        accounts,
        friend_requests,
        friendships,
        invitations,
        history,
        snapshot_tx,
        revision,
        search_stats,
        search_latency
    );

    for command in rx {
        let mut changed = false;
        match command {
            StoreCommand::CreateAccount {
                handle,
                display_name,
                token,
                reply,
            } => {
                let result = validate_account_fields(&handle, &display_name).and_then(|_| {
                    let normalized = normalize_handle(&handle);
                    if accounts.rtx(|(_, handles, _)| handles.contains(&normalized)) {
                        return Err("handle is already registered".into());
                    }
                    let now = now_ms();
                    let account = Account {
                        player_id: Uuid::new_v4().to_string(),
                        handle: normalized,
                        display_name: display_name.trim().to_string(),
                        appearance_status: AppearanceStatus::Missing,
                        created_at_ms: now,
                        updated_at_ms: now,
                    };
                    let record = AccountRecord {
                        account: account.clone(),
                        token_hash: hash_token(&token),
                    };
                    accounts.wtx(|tx| tx.upsert(&account.player_id, &record));
                    changed = true;
                    Ok(account)
                });
                let _ = reply.send(result);
            }
            StoreCommand::Authenticate { token_hash, reply } => {
                let player_id = accounts.rtx(|(_, _, tokens)| tokens.get(&token_hash));
                let _ = reply.send(player_id);
            }
            StoreCommand::GetAccount { player_id, reply } => {
                let _ = reply.send(
                    accounts
                        .rtx(|(table, _, _)| table.get(&player_id).map(|record| record.account)),
                );
            }
            StoreCommand::UpdateAccount {
                player_id,
                handle,
                display_name,
                reply,
            } => {
                let result = accounts
                    .rtx(|(table, _, _)| table.get(&player_id))
                    .ok_or_else(|| "account not found".to_string())
                    .and_then(|mut record| {
                        let next_handle = handle.unwrap_or_else(|| record.account.handle.clone());
                        let next_name =
                            display_name.unwrap_or_else(|| record.account.display_name.clone());
                        validate_account_fields(&next_handle, &next_name)?;
                        let normalized = normalize_handle(&next_handle);
                        let owner = accounts.rtx(|(_, handles, _)| handles.get(&normalized));
                        if owner.as_deref().is_some_and(|owner| owner != player_id) {
                            return Err("handle is already registered".into());
                        }
                        record.account.handle = normalized;
                        record.account.display_name = next_name.trim().to_string();
                        record.account.updated_at_ms = now_ms();
                        accounts.wtx(|tx| tx.upsert(&player_id, &record));
                        changed = true;
                        Ok(record.account)
                    });
                let _ = reply.send(result);
            }
            StoreCommand::FindAccount { handle, reply } => {
                let normalized = normalize_handle(&handle);
                let player_id = accounts.rtx(|(_, handles, _)| handles.get(&normalized));
                let account = player_id.and_then(|id| {
                    accounts.rtx(|(table, _, _)| table.get(&id).map(|record| record.account))
                });
                let _ = reply.send(account);
            }
            StoreCommand::UpsertAppearance { mut profile, reply } => {
                let result = accounts
                    .rtx(|(table, _, _)| table.get(&profile.player_id))
                    .ok_or_else(|| "account not found".to_string())
                    .and_then(|mut record| {
                        profile.display_name = record.account.display_name.clone();
                        profile.updated_at_ms = now_ms();
                        profile.validate().map_err(str::to_string)?;
                        // A repeat upsert for an existing player is a re-enrollment: Fold
                        // retracts the prior profile's contributions from every appearance
                        // sink (BM25, both HNSWs) and inserts the new one, with no rebuild
                        // and no index growth. Count it so the inspector can show the
                        // retraction happening live during the re-enrollment demo.
                        if appearances.contains(&profile.player_id) {
                            *search_stats
                                .entry("appearanceReenrollments".into())
                                .or_insert(0) += 1;
                        }
                        appearances.wtx(|tx| tx.upsert(&profile.player_id, &profile));
                        record.account.appearance_status = AppearanceStatus::Registered;
                        record.account.updated_at_ms = profile.updated_at_ms;
                        accounts.wtx(|tx| tx.upsert(&profile.player_id, &record));
                        changed = true;
                        Ok(profile)
                    });
                let _ = reply.send(result);
            }
            StoreCommand::UpsertPresence { presence } => {
                presences.wtx(|tx| tx.upsert(&presence.player_id, &presence));
                changed = true;
            }
            StoreCommand::ClearPresence { player_id } => {
                presences.wtx(|tx| tx.remove(&player_id));
                changed = true;
            }
            StoreCommand::SearchAppearance { query, reply } => {
                let started = Instant::now();
                let hits = appearances.rtx(|(_, bm25, semantic, _)| {
                    let mut combined: HashMap<PlayerId, f64> = HashMap::new();
                    for (rank, hit) in bm25.search(&query, MAX_SEARCH_RESULTS).iter().enumerate() {
                        *combined.entry(hit.val.clone()).or_default() += 1.0 / (61.0 + rank as f64);
                    }
                    let encoded = ese::encode_single(&query);
                    for (rank, hit) in semantic.search(&encoded).iter().enumerate() {
                        *combined.entry(hit.val.clone()).or_default() += 1.0 / (61.0 + rank as f64);
                    }
                    let mut hits: Vec<_> = combined
                        .into_iter()
                        .map(|(player_id, score)| SearchHit {
                            player_id,
                            score,
                            source: "bm25+ese",
                        })
                        .collect();
                    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
                    hits.truncate(MAX_SEARCH_RESULTS);
                    hits
                });
                search_latency.insert(
                    "appearanceSearchMicros".into(),
                    started.elapsed().as_micros() as u64,
                );
                *search_stats.entry("appearanceQueries".into()).or_insert(0) += 1;
                changed = true;
                let _ = reply.send(hits);
            }
            StoreCommand::SearchNearby { player_id, reply } => {
                let started = Instant::now();
                let now = now_ms();
                let origin = presences.get(&player_id).filter(|presence| {
                    presence.is_available_at(now) && presence.has_usable_location()
                });
                let hits = origin.map_or_else(Vec::new, |origin| {
                    presences.rtx(|(table, index)| {
                        index
                            .search(&origin.earth_vector())
                            .into_iter()
                            .filter(|hit| hit.val != player_id)
                            .filter_map(|hit| {
                                let presence = table.get(&hit.val)?;
                                if !presence.is_available_at(now) || !presence.has_usable_location()
                                {
                                    return None;
                                }
                                let distance = origin.distance_meters(&presence);
                                (distance <= MATCH_RADIUS_METERS).then_some(NearbyHit {
                                    player_id: hit.val,
                                    distance_meters: distance,
                                })
                            })
                            .collect()
                    })
                });
                search_latency.insert(
                    "nearbySearchMicros".into(),
                    started.elapsed().as_micros() as u64,
                );
                *search_stats.entry("nearbyQueries".into()).or_insert(0) += 1;
                changed = true;
                let _ = reply.send(hits);
            }
            StoreCommand::MatchNearby { player_id, reply } => {
                let started = Instant::now();
                let now = now_ms();
                // A still-waiting, one-player Lobby match is a live matchmaking slot.
                let is_open_slot = |match_id: &MatchId| -> bool {
                    matches
                        .get(match_id)
                        .is_some_and(|snapshot: MatchSnapshot| {
                            snapshot.status == MatchStatus::Lobby && snapshot.players.len() == 1
                        })
                };
                let available_here = |pid: &PlayerId| -> Option<Presence> {
                    presences
                        .get(pid)
                        .filter(|p| p.is_available_at(now) && p.has_usable_location())
                };

                let result = if available_here(&player_id).is_none() {
                    Err(
                        "enable location sharing and set yourself available to match nearby"
                            .to_string(),
                    )
                } else if let Some((_, existing)) = nearby_queue
                    .iter()
                    .find(|(pid, mid)| pid == &player_id && is_open_slot(mid))
                    .cloned()
                {
                    // Idempotent: the caller is already waiting; hand back their slot.
                    matches
                        .get(&existing)
                        .ok_or_else(|| "waiting match vanished".to_string())
                } else {
                    // Drop our own stale slot plus any queued player who left, went
                    // unavailable, or whose match already filled or started.
                    nearby_queue.retain(|(pid, mid)| {
                        pid != &player_id && is_open_slot(mid) && available_here(pid).is_some()
                    });
                    let origin = available_here(&player_id).expect("checked above");
                    // Rank every usable waiting player by ANNy presence proximity, then
                    // take the nearest one that is actually holding an open slot.
                    let opponent = presences.rtx(|(table, index)| {
                        index
                            .search(&origin.earth_vector())
                            .into_iter()
                            .filter(|hit| hit.val != player_id)
                            .filter_map(|hit| {
                                let candidate = table.get(&hit.val)?;
                                if !candidate.is_available_at(now)
                                    || !candidate.has_usable_location()
                                    || origin.distance_meters(&candidate) > MATCH_RADIUS_METERS
                                {
                                    return None;
                                }
                                let idx =
                                    nearby_queue.iter().position(|(pid, _)| pid == &hit.val)?;
                                Some((idx, nearby_queue[idx].1.clone()))
                            })
                            .next()
                    });

                    if let Some((idx, match_id)) = opponent {
                        nearby_queue.remove(idx);
                        matches
                            .get(&match_id)
                            .ok_or_else(|| "waiting match vanished".to_string())
                            .and_then(|mut snapshot: MatchSnapshot| {
                                if !snapshot.add_player(player_id.clone(), now) {
                                    return Err("nearby match is no longer joinable".into());
                                }
                                matches.wtx(|tx| tx.upsert(&snapshot.match_id, &snapshot));
                                events.wtx(|tx| {
                                    tx.insert(&MatchEvent::Joined {
                                        event_id: Uuid::new_v4(),
                                        match_id: snapshot.match_id.clone(),
                                        player_id: player_id.clone(),
                                        at_ms: now,
                                    })
                                });
                                Ok(snapshot)
                            })
                    } else {
                        // No one waiting nearby: open a slot and join the queue.
                        let match_id = Uuid::new_v4().to_string();
                        let invite_code = Alphanumeric
                            .sample_string(&mut rand::thread_rng(), 6)
                            .to_uppercase();
                        let snapshot = MatchSnapshot::new(
                            match_id.clone(),
                            invite_code,
                            player_id.clone(),
                            now,
                        );
                        matches.wtx(|tx| tx.upsert(&match_id, &snapshot));
                        events.wtx(|tx| {
                            tx.insert(&MatchEvent::Created {
                                event_id: Uuid::new_v4(),
                                match_id: match_id.clone(),
                                host_id: player_id.clone(),
                                at_ms: now,
                            })
                        });
                        nearby_queue.push((player_id.clone(), match_id));
                        Ok(snapshot)
                    }
                };

                search_latency.insert(
                    "nearbyMatchMicros".into(),
                    started.elapsed().as_micros() as u64,
                );
                *search_stats.entry("nearbyMatchmakes".into()).or_insert(0) += 1;
                changed = result.is_ok();
                let _ = reply.send(result);
            }
            StoreCommand::ListFriendRequests { player_id, reply } => {
                let mut list = friend_requests.rtx(|table| {
                    table
                        .iter()
                        .map(|(_, request)| request)
                        .filter(|request| {
                            request.from_player_id == player_id || request.to_player_id == player_id
                        })
                        .collect::<Vec<_>>()
                });
                list.sort_by_key(|request| std::cmp::Reverse(request.updated_at_ms));
                let _ = reply.send(list);
            }
            StoreCommand::CreateFriendRequest {
                from_id,
                to_id,
                reply,
            } => {
                let result = if from_id == to_id {
                    Err("cannot friend yourself".into())
                } else if !accounts.contains(&to_id) {
                    Err("account not found".into())
                } else if friendships.contains(&friendship_key(&from_id, &to_id)) {
                    Err("players are already friends".into())
                } else if let Some(existing) = friend_requests.rtx(|table| {
                    table.iter().map(|(_, request)| request).find(|request| {
                        request.from_player_id == from_id
                            && request.to_player_id == to_id
                            && request.status == FriendRequestStatus::Pending
                    })
                }) {
                    Ok(existing)
                } else if friend_requests.rtx(|table| {
                    table.iter().map(|(_, request)| request).any(|request| {
                        request.from_player_id == to_id
                            && request.to_player_id == from_id
                            && request.status == FriendRequestStatus::Pending
                    })
                }) {
                    Err("a reverse friend request is already pending".into())
                } else {
                    let now = now_ms();
                    let request = FriendRequest {
                        request_id: Uuid::new_v4().to_string(),
                        from_player_id: from_id,
                        to_player_id: to_id,
                        status: FriendRequestStatus::Pending,
                        created_at_ms: now,
                        updated_at_ms: now,
                    };
                    friend_requests.wtx(|tx| tx.upsert(&request.request_id, &request));
                    social_events.wtx(|tx| {
                        tx.insert(&SocialEvent::Requested {
                            event_id: Uuid::new_v4(),
                            request: request.clone(),
                        })
                    });
                    Ok(request)
                };
                changed = result.is_ok();
                let _ = reply.send(result);
            }
            StoreCommand::ResolveFriendRequest {
                actor_id,
                request_id,
                accept,
                reply,
            } => {
                let result = friend_requests
                    .get(&request_id)
                    .ok_or_else(|| "friend request not found".to_string())
                    .and_then(|mut request| {
                        if request.to_player_id != actor_id {
                            return Err("friend request is not actionable".into());
                        }
                        let desired = if accept {
                            FriendRequestStatus::Accepted
                        } else {
                            FriendRequestStatus::Declined
                        };
                        if request.status == desired {
                            if accept {
                                let key =
                                    friendship_key(&request.from_player_id, &request.to_player_id);
                                if !friendships.contains(&key) {
                                    friendships.wtx(|tx| {
                                        tx.upsert(
                                            &key,
                                            &Friendship {
                                                player_a: request.from_player_id.clone(),
                                                player_b: request.to_player_id.clone(),
                                                since_ms: request.updated_at_ms,
                                            },
                                        )
                                    });
                                    changed = true;
                                }
                            }
                            return Ok(request);
                        }
                        if request.status != FriendRequestStatus::Pending {
                            return Err("friend request already has a different outcome".into());
                        }
                        let now = now_ms();
                        request.status = desired;
                        request.updated_at_ms = now;
                        friend_requests.wtx(|tx| tx.upsert(&request_id, &request));
                        let social_event = if accept {
                            let friendship = Friendship {
                                player_a: request.from_player_id.clone(),
                                player_b: request.to_player_id.clone(),
                                since_ms: now,
                            };
                            friendships.wtx(|tx| {
                                tx.upsert(
                                    &friendship_key(&friendship.player_a, &friendship.player_b),
                                    &friendship,
                                )
                            });
                            SocialEvent::Accepted {
                                event_id: Uuid::new_v4(),
                                request_id: request_id.clone(),
                                actor_id,
                                at_ms: now,
                            }
                        } else {
                            SocialEvent::Declined {
                                event_id: Uuid::new_v4(),
                                request_id: request_id.clone(),
                                actor_id,
                                at_ms: now,
                            }
                        };
                        social_events.wtx(|tx| tx.insert(&social_event));
                        changed = true;
                        Ok(request)
                    });
                let _ = reply.send(result);
            }
            StoreCommand::ListFriends { player_id, reply } => {
                let now = now_ms();
                let ids = friendships.rtx(|table| {
                    table
                        .iter()
                        .map(|(_, friendship)| friendship)
                        .filter_map(|friendship| {
                            if friendship.player_a == player_id {
                                Some(friendship.player_b)
                            } else if friendship.player_b == player_id {
                                Some(friendship.player_a)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                });
                let mut list = ids
                    .into_iter()
                    .filter_map(|id| {
                        let account = accounts
                            .rtx(|(table, _, _)| table.get(&id).map(|record| record.account))?;
                        let available = presences
                            .get(&id)
                            .is_some_and(|presence| presence.is_available_at(now));
                        Some(Friend { account, available })
                    })
                    .collect::<Vec<_>>();
                list.sort_by(|a, b| a.account.handle.cmp(&b.account.handle));
                let _ = reply.send(list);
            }
            StoreCommand::RemoveFriend {
                player_id,
                friend_id,
                reply,
            } => {
                let key = friendship_key(&player_id, &friend_id);
                let result = if friendships.contains(&key) {
                    friendships.wtx(|tx| tx.remove(&key));
                    social_events.wtx(|tx| {
                        tx.insert(&SocialEvent::Removed {
                            event_id: Uuid::new_v4(),
                            actor_id: player_id,
                            friend_id,
                            at_ms: now_ms(),
                        })
                    });
                    changed = true;
                    Ok(())
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            StoreCommand::CreateMatch { host_id, reply } => {
                let match_id = Uuid::new_v4().to_string();
                let invite_code = Alphanumeric
                    .sample_string(&mut rand::thread_rng(), 6)
                    .to_uppercase();
                let now = now_ms();
                let snapshot =
                    MatchSnapshot::new(match_id.clone(), invite_code, host_id.clone(), now);
                matches.wtx(|tx| tx.upsert(&match_id, &snapshot));
                events.wtx(|tx| {
                    tx.insert(&MatchEvent::Created {
                        event_id: Uuid::new_v4(),
                        match_id,
                        host_id,
                        at_ms: now,
                    })
                });
                changed = true;
                let _ = reply.send(snapshot);
            }
            StoreCommand::JoinMatch {
                invite_code,
                player_id,
                reply,
            } => {
                let found = matches.rtx(|table| {
                    table
                        .iter()
                        .map(|(_, snapshot)| snapshot)
                        .find(|snapshot| snapshot.invite_code.eq_ignore_ascii_case(&invite_code))
                });
                let result = found
                    .ok_or_else(|| "invite code not found".to_string())
                    .and_then(|mut snapshot| {
                        let now = now_ms();
                        if snapshot.invite_expires_at_ms < now {
                            return Err("invite code expired".into());
                        }
                        if !snapshot.add_player(player_id.clone(), now) {
                            return Err("match is full or no longer joinable".into());
                        }
                        matches.wtx(|tx| tx.upsert(&snapshot.match_id, &snapshot));
                        events.wtx(|tx| {
                            tx.insert(&MatchEvent::Joined {
                                event_id: Uuid::new_v4(),
                                match_id: snapshot.match_id.clone(),
                                player_id,
                                at_ms: now,
                            })
                        });
                        Ok(snapshot)
                    });
                changed = result.is_ok();
                let _ = reply.send(result);
            }
            StoreCommand::CreateTargetInvitation {
                from_id,
                to_id,
                reply,
            } => {
                let result = if !friendships.contains(&friendship_key(&from_id, &to_id)) {
                    Err("targeted invitations require an accepted friendship".into())
                } else {
                    let existing = invitations.rtx(|(table, _)| {
                        table
                            .iter()
                            .map(|(_, invitation)| invitation)
                            .find(|invitation| {
                                invitation.from_player_id == from_id
                                    && invitation.to_player_id == to_id
                                    && invitation.status == MatchInvitationStatus::Pending
                                    && invitation.expires_at_ms >= now_ms()
                            })
                    });
                    if let Some(existing) = existing {
                        matches
                            .get(&existing.match_id)
                            .map(|snapshot| (existing, snapshot))
                            .ok_or_else(|| "match not found".into())
                    } else {
                        let now = now_ms();
                        let match_id = Uuid::new_v4().to_string();
                        let invite_code = Alphanumeric
                            .sample_string(&mut rand::thread_rng(), 6)
                            .to_uppercase();
                        let snapshot =
                            MatchSnapshot::new(match_id.clone(), invite_code, from_id.clone(), now);
                        matches.wtx(|tx| tx.upsert(&match_id, &snapshot));
                        events.wtx(|tx| {
                            tx.insert(&MatchEvent::Created {
                                event_id: Uuid::new_v4(),
                                match_id,
                                host_id: from_id.clone(),
                                at_ms: now,
                            })
                        });
                        let invitation = MatchInvitation {
                            invitation_id: Uuid::new_v4().to_string(),
                            from_player_id: from_id,
                            to_player_id: to_id,
                            match_id: snapshot.match_id.clone(),
                            status: MatchInvitationStatus::Pending,
                            created_at_ms: now,
                            expires_at_ms: now + INVITE_LIFETIME_MS,
                            updated_at_ms: now,
                        };
                        invitations.wtx(|tx| tx.upsert(&invitation.invitation_id, &invitation));
                        changed = true;
                        Ok((invitation, snapshot))
                    }
                };
                let _ = reply.send(result);
            }
            StoreCommand::ListTargetInvitations { player_id, reply } => {
                let now = now_ms();
                let mut list = invitations.rtx(|(table, recipient_index)| {
                    let prefix = format!("{player_id}:");
                    recipient_index
                        .iter()
                        .filter(|(key, _)| key.starts_with(&prefix))
                        .filter_map(|(_, invitation_id)| table.get(&invitation_id))
                        .chain(
                            table
                                .iter()
                                .map(|(_, invitation)| invitation)
                                .filter(|invitation| invitation.from_player_id == player_id),
                        )
                        .collect::<Vec<_>>()
                });
                list.sort_by_key(|invitation| std::cmp::Reverse(invitation.updated_at_ms));
                list.dedup_by(|a, b| a.invitation_id == b.invitation_id);
                for invitation in &mut list {
                    if invitation.status == MatchInvitationStatus::Pending
                        && invitation.expires_at_ms < now
                    {
                        invitation.status = MatchInvitationStatus::Expired;
                        invitation.updated_at_ms = now;
                        invitations.wtx(|tx| tx.upsert(&invitation.invitation_id, invitation));
                        changed = true;
                    }
                }
                let _ = reply.send(list);
            }
            StoreCommand::ResolveTargetInvitation {
                actor_id,
                invitation_id,
                action,
                reply,
            } => {
                let result = invitations
                    .get(&invitation_id)
                    .ok_or_else(|| "match invitation not found".to_string())
                    .and_then(|mut invitation| {
                        let now = now_ms();
                        let desired = match action {
                            InvitationAction::Accept if actor_id == invitation.to_player_id => {
                                MatchInvitationStatus::Accepted
                            }
                            InvitationAction::Decline if actor_id == invitation.to_player_id => {
                                MatchInvitationStatus::Declined
                            }
                            InvitationAction::Cancel if actor_id == invitation.from_player_id => {
                                MatchInvitationStatus::Cancelled
                            }
                            _ => {
                                return Err("not authorized for that invitation action".into());
                            }
                        };
                        if invitation.status == desired {
                            let snapshot = (desired == MatchInvitationStatus::Accepted)
                                .then(|| matches.get(&invitation.match_id))
                                .flatten();
                            return Ok((invitation, snapshot));
                        }
                        if invitation.status != MatchInvitationStatus::Pending {
                            return Err("match invitation already has a different outcome".into());
                        }
                        if invitation.expires_at_ms < now {
                            invitation.status = MatchInvitationStatus::Expired;
                            invitation.updated_at_ms = now;
                            invitations.wtx(|tx| tx.upsert(&invitation_id, &invitation));
                            changed = true;
                            return Err("match invitation expired".into());
                        }
                        let snapshot = match action {
                            InvitationAction::Accept if actor_id == invitation.to_player_id => {
                                if !friendships.contains(&friendship_key(
                                    &invitation.from_player_id,
                                    &invitation.to_player_id,
                                )) {
                                    return Err(
                                        "targeted invitations require an accepted friendship"
                                            .into(),
                                    );
                                }
                                let mut snapshot = matches
                                    .get(&invitation.match_id)
                                    .ok_or_else(|| "match not found".to_string())?;
                                let already_joined = snapshot
                                    .players
                                    .iter()
                                    .any(|player| player.player_id == actor_id);
                                if !already_joined && !snapshot.add_player(actor_id.clone(), now) {
                                    return Err("match is no longer joinable".into());
                                }
                                matches.wtx(|tx| tx.upsert(&snapshot.match_id, &snapshot));
                                if !already_joined {
                                    events.wtx(|tx| {
                                        tx.insert(&MatchEvent::Joined {
                                            event_id: Uuid::new_v4(),
                                            match_id: snapshot.match_id.clone(),
                                            player_id: actor_id.clone(),
                                            at_ms: now,
                                        })
                                    });
                                }
                                invitation.status = MatchInvitationStatus::Accepted;
                                Some(snapshot)
                            }
                            InvitationAction::Decline if actor_id == invitation.to_player_id => {
                                invitation.status = MatchInvitationStatus::Declined;
                                None
                            }
                            InvitationAction::Cancel if actor_id == invitation.from_player_id => {
                                invitation.status = MatchInvitationStatus::Cancelled;
                                None
                            }
                            _ => return Err("not authorized for that invitation action".into()),
                        };
                        invitation.updated_at_ms = now;
                        invitations.wtx(|tx| tx.upsert(&invitation_id, &invitation));
                        changed = true;
                        Ok((invitation, snapshot))
                    });
                let _ = reply.send(result);
            }
            StoreCommand::Ready {
                command_id,
                match_id,
                player_id,
                calibration_model_version,
                reply,
            } => {
                let command_key = command_id.to_string();
                if processed.contains(&command_key) {
                    let _ = reply.send(
                        matches
                            .get(&match_id)
                            .ok_or_else(|| "match not found".into()),
                    );
                    continue;
                }
                let has_appearance = appearances.contains(&player_id);
                let result = if calibration_model_version != REQUIRED_CALIBRATION_MODEL {
                    Err(format!(
                        "calibration model {REQUIRED_CALIBRATION_MODEL} is required"
                    ))
                } else if !has_appearance {
                    Err("appearance registration is required".into())
                } else {
                    matches
                        .get(&match_id)
                        .and_then(|mut snapshot| {
                            let now = now_ms();
                            snapshot
                                .set_ready_with_calibration(
                                    &player_id,
                                    true,
                                    calibration_model_version.clone(),
                                    now,
                                )
                                .then(|| {
                                    processed.wtx(|tx| tx.upsert(&command_key, &true));
                                    matches.wtx(|tx| tx.upsert(&match_id, &snapshot));
                                    events.wtx(|tx| {
                                        tx.insert(&MatchEvent::Ready {
                                            event_id: Uuid::new_v4(),
                                            match_id: match_id.clone(),
                                            player_id,
                                            calibration_model_version,
                                            at_ms: now,
                                        })
                                    });
                                    snapshot
                                })
                        })
                        .ok_or_else(|| "player or lobby not found".into())
                };
                changed = result.is_ok();
                let _ = reply.send(result);
            }
            StoreCommand::AcknowledgeBriefing {
                command_id,
                match_id,
                player_id,
                reply,
            } => {
                let command_key = command_id.to_string();
                if processed.contains(&command_key) {
                    let _ = reply.send(
                        matches
                            .get(&match_id)
                            .ok_or_else(|| "match not found".into()),
                    );
                    continue;
                }
                let result = matches
                    .get(&match_id)
                    .and_then(|mut snapshot| {
                        let now = now_ms();
                        snapshot.acknowledge_briefing(&player_id, now).then(|| {
                            processed.wtx(|tx| tx.upsert(&command_key, &true));
                            matches.wtx(|tx| tx.upsert(&match_id, &snapshot));
                            events.wtx(|tx| {
                                tx.insert(&MatchEvent::BriefingAcknowledged {
                                    event_id: Uuid::new_v4(),
                                    match_id: match_id.clone(),
                                    player_id,
                                    at_ms: now,
                                })
                            });
                            snapshot
                        })
                    })
                    .ok_or_else(|| "briefing or player not found".into());
                changed = result.is_ok();
                let _ = reply.send(result);
            }
            StoreCommand::Proximity {
                command_id,
                match_id,
                player_id,
                peer_id,
                report,
            } => {
                let command_key = command_id.to_string();
                let valid_pair = matches.get(&match_id).is_some_and(|snapshot| {
                    snapshot
                        .players
                        .iter()
                        .any(|player| player.player_id == player_id)
                        && snapshot
                            .players
                            .iter()
                            .any(|player| player.player_id == peer_id)
                });
                if valid_pair && !processed.contains(&command_key) {
                    processed.wtx(|tx| tx.upsert(&command_key, &true));
                    proximity.insert((match_id, player_id, peer_id), report);
                    changed = true;
                }
            }
            StoreCommand::Shot {
                command_id,
                match_id,
                shooter_id,
                target_id,
                mask_contains_reticle,
                target_score,
                reply,
            } => {
                let command_key = command_id.to_string();
                let was_accepted = events.rtx(|(bag, _)| {
                    bag.iter().any(|(event, count)| {
                        count > 0
                            && matches!(
                                event,
                                MatchEvent::Hit {
                                    command_id: event_command_id,
                                    ..
                                } if event_command_id == command_id
                            )
                    })
                });
                let response = if was_accepted {
                    ServerMessage::ShotResolution {
                        command_id,
                        accepted: true,
                        reason: "accepted".into(),
                        snapshot: matches.get(&match_id),
                    }
                } else if processed.contains(&command_key) {
                    ServerMessage::ShotResolution {
                        command_id,
                        accepted: false,
                        reason: "duplicate_command".into(),
                        snapshot: matches.get(&match_id),
                    }
                } else {
                    let forward =
                        proximity.get(&(match_id.clone(), shooter_id.clone(), target_id.clone()));
                    let reverse =
                        proximity.get(&(match_id.clone(), target_id.clone(), shooter_id.clone()));
                    let reason = if !mask_contains_reticle {
                        Some("reticle_outside_target")
                    } else if target_score < 0.5 {
                        Some("target_lock_too_weak")
                    } else if !reciprocal_proximity(forward, reverse, now_ms()) {
                        Some("missing_reciprocal_proximity")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        ServerMessage::ShotResolution {
                            command_id,
                            accepted: false,
                            reason: reason.into(),
                            snapshot: matches.get(&match_id),
                        }
                    } else if let Some(mut snapshot) = matches.get(&match_id) {
                        let accepted_at_ms = now_ms();
                        let accepted = snapshot.apply_hit(&shooter_id, &target_id, accepted_at_ms);
                        if accepted {
                            matches.wtx(|tx| tx.upsert(&match_id, &snapshot));
                            events.wtx(|tx| {
                                tx.insert(&MatchEvent::Hit {
                                    event_id: Uuid::new_v4(),
                                    command_id,
                                    match_id: match_id.clone(),
                                    shooter_id: shooter_id.clone(),
                                    target_id: target_id.clone(),
                                    at_ms: accepted_at_ms,
                                });
                                if let Some(winner_id) = &snapshot.winner {
                                    tx.insert(&MatchEvent::Completed {
                                        event_id: Uuid::new_v4(),
                                        match_id: match_id.clone(),
                                        winner_id: winner_id.clone(),
                                        at_ms: accepted_at_ms,
                                    });
                                }
                            });
                            if snapshot.completed_at_ms.is_some() && !history.contains(&match_id) {
                                let mut hit_totals = BTreeMap::new();
                                for shooter in &snapshot.players {
                                    let hits = snapshot
                                        .players
                                        .iter()
                                        .filter(|target| target.player_id != shooter.player_id)
                                        .map(|target| {
                                            u32::from(MAX_HEALTH.saturating_sub(target.health))
                                        })
                                        .sum();
                                    hit_totals.insert(shooter.player_id.clone(), hits);
                                }
                                let record = CompletedMatchRecord {
                                    snapshot: snapshot.clone(),
                                    hit_totals,
                                };
                                history.wtx(|tx| tx.upsert(&match_id, &record));
                            }
                        }
                        ServerMessage::ShotResolution {
                            command_id,
                            accepted,
                            reason: if accepted {
                                "accepted"
                            } else {
                                "invalid_match_state"
                            }
                            .into(),
                            snapshot: Some(snapshot),
                        }
                    } else {
                        ServerMessage::ShotResolution {
                            command_id,
                            accepted: false,
                            reason: "match_not_found".into(),
                            snapshot: None,
                        }
                    }
                };
                if !processed.contains(&command_key) {
                    processed.wtx(|tx| tx.upsert(&command_key, &true));
                }
                changed = true;
                let _ = reply.send(response);
            }
            StoreCommand::GetMatch {
                match_id,
                requester_id,
                reply,
            } => {
                let result = matches
                    .get(&match_id)
                    .filter(|snapshot| {
                        snapshot
                            .players
                            .iter()
                            .any(|player| player.player_id == requester_id)
                    })
                    .ok_or_else(|| "match not found".into());
                let _ = reply.send(result);
            }
            StoreCommand::GetMatchDetail {
                match_id,
                requester_id,
                reply,
            } => {
                let result = history
                    .rtx(|(table, _)| table.get(&match_id))
                    .filter(|record| {
                        record
                            .snapshot
                            .players
                            .iter()
                            .any(|player| player.player_id == requester_id)
                    })
                    .map(|record| {
                        let participants = record
                            .snapshot
                            .players
                            .iter()
                            .filter_map(|player| {
                                let account = accounts.rtx(|(table, _, _)| {
                                    table.get(&player.player_id).map(|record| record.account)
                                })?;
                                Some(MatchHistoryParticipant {
                                    player_id: account.player_id.clone(),
                                    handle: Some(account.handle),
                                    display_name: account.display_name,
                                    hit_total: record
                                        .hit_totals
                                        .get(&account.player_id)
                                        .copied()
                                        .unwrap_or_default(),
                                    winner: record.snapshot.winner.as_ref()
                                        == Some(&account.player_id),
                                })
                            })
                            .collect::<Vec<_>>();
                        let mut timeline = events.rtx(|(bag, _)| {
                            bag.iter()
                                .filter_map(|(event, count)| {
                                    (count > 0 && event.match_id() == match_id).then_some(event)
                                })
                                .collect::<Vec<_>>()
                        });
                        timeline.sort_by_key(match_event_time);
                        let started_at_ms = record
                            .snapshot
                            .started_at_ms
                            .unwrap_or(record.snapshot.created_at_ms);
                        let completed_at_ms = record
                            .snapshot
                            .completed_at_ms
                            .unwrap_or(record.snapshot.updated_at_ms);
                        MatchDetail {
                            match_id: record.snapshot.match_id,
                            result: if record.snapshot.winner.as_deref()
                                == Some(requester_id.as_str())
                            {
                                "won".into()
                            } else {
                                "lost".into()
                            },
                            participants,
                            started_at_ms,
                            completed_at_ms,
                            events: timeline
                                .into_iter()
                                .map(history_event_from_match_event)
                                .collect(),
                        }
                    })
                    .ok_or_else(|| "completed match not found".into());
                let _ = reply.send(result);
            }
            StoreCommand::ListHistory {
                player_id,
                cursor,
                limit,
                reply,
            } => {
                let result = parse_cursor(cursor.as_deref()).map(|cursor| {
                    history.rtx(|(table, ranked)| {
                        let mut records = ranked
                            .iter(&player_id)
                            .rev()
                            .map(|(scored, _)| (scored.score, scored.val))
                            .filter(|item| {
                                cursor
                                    .as_ref()
                                    .is_none_or(|cursor| item < &(cursor.0, cursor.1.clone()))
                            })
                            .take(limit + 1)
                            .collect::<Vec<_>>();
                        let has_more = records.len() > limit;
                        records.truncate(limit);
                        let next_cursor = has_more
                            .then(|| records.last().map(|(time, id)| encode_cursor(*time, id)))
                            .flatten();
                        let matches = records
                            .into_iter()
                            .filter_map(|(_, match_id)| {
                                let record = table.get(&match_id)?;
                                let opponent_id = record
                                    .snapshot
                                    .players
                                    .iter()
                                    .find(|player| player.player_id != player_id)?
                                    .player_id
                                    .clone();
                                let opponent = accounts.rtx(|(account_table, _, _)| {
                                    account_table.get(&opponent_id).map(|record| record.account)
                                })?;
                                let completed_at_ms = record.snapshot.completed_at_ms?;
                                let started_at_ms = record
                                    .snapshot
                                    .started_at_ms
                                    .unwrap_or(record.snapshot.created_at_ms);
                                Some(MatchHistoryEntry {
                                    match_id: record.snapshot.match_id.clone(),
                                    result: if record.snapshot.winner.as_deref()
                                        == Some(player_id.as_str())
                                    {
                                        "won".into()
                                    } else {
                                        "lost".into()
                                    },
                                    opponent: MatchHistoryParticipant {
                                        player_id: opponent.player_id.clone(),
                                        handle: Some(opponent.handle),
                                        display_name: opponent.display_name,
                                        hit_total: record
                                            .hit_totals
                                            .get(&opponent.player_id)
                                            .copied()
                                            .unwrap_or_default(),
                                        winner: record.snapshot.winner.as_ref()
                                            == Some(&opponent.player_id),
                                    },
                                    started_at_ms,
                                    completed_at_ms,
                                    my_hit_total: record
                                        .hit_totals
                                        .get(&player_id)
                                        .copied()
                                        .unwrap_or_default(),
                                })
                            })
                            .collect();
                        MatchHistoryPage {
                            matches,
                            next_cursor,
                        }
                    })
                });
                let _ = reply.send(result);
            }
            StoreCommand::GetMatchAppearance {
                requester_id,
                player_id,
                reply,
            } => {
                let allowed = matches.rtx(|table| {
                    table.iter().any(|(_, snapshot)| {
                        matches!(
                            snapshot.status,
                            untitled_mobile_fps::MatchStatus::Briefing
                                | untitled_mobile_fps::MatchStatus::Active
                        ) && snapshot.players.iter().any(|p| p.player_id == requester_id)
                            && snapshot.players.iter().any(|p| p.player_id == player_id)
                    })
                });
                let result = if allowed {
                    appearances
                        .rtx(|(table, _, _, _)| table.get(&player_id))
                        .ok_or_else(|| "opponent has not registered an appearance".into())
                } else {
                    Err("appearance is only available to a briefing or active match peer".into())
                };
                let _ = reply.send(result);
            }
        }

        if changed {
            revision += 1;
            publish_snapshot!(
                appearances,
                presences,
                matches,
                processed,
                accounts,
                friend_requests,
                friendships,
                invitations,
                history,
                snapshot_tx,
                revision,
                search_stats,
                search_latency
            );
        }
    }
}

fn match_event_time(event: &MatchEvent) -> u64 {
    match event {
        MatchEvent::Created { at_ms, .. }
        | MatchEvent::Joined { at_ms, .. }
        | MatchEvent::Ready { at_ms, .. }
        | MatchEvent::BriefingAcknowledged { at_ms, .. }
        | MatchEvent::Hit { at_ms, .. }
        | MatchEvent::Completed { at_ms, .. } => *at_ms,
    }
}

fn history_event_from_match_event(event: MatchEvent) -> MatchHistoryEvent {
    match event {
        MatchEvent::Created {
            event_id,
            host_id,
            at_ms,
            ..
        } => MatchHistoryEvent {
            event_id: event_id.to_string(),
            event_type: "created".into(),
            player_id: Some(host_id),
            timestamp_ms: at_ms,
            detail: None,
        },
        MatchEvent::Joined {
            event_id,
            player_id,
            at_ms,
            ..
        } => MatchHistoryEvent {
            event_id: event_id.to_string(),
            event_type: "joined".into(),
            player_id: Some(player_id),
            timestamp_ms: at_ms,
            detail: None,
        },
        MatchEvent::Ready {
            event_id,
            player_id,
            calibration_model_version,
            at_ms,
            ..
        } => MatchHistoryEvent {
            event_id: event_id.to_string(),
            event_type: "ready".into(),
            player_id: Some(player_id),
            timestamp_ms: at_ms,
            detail: Some(format!("calibration {calibration_model_version}")),
        },
        MatchEvent::BriefingAcknowledged {
            event_id,
            player_id,
            at_ms,
            ..
        } => MatchHistoryEvent {
            event_id: event_id.to_string(),
            event_type: "briefing_acknowledged".into(),
            player_id: Some(player_id),
            timestamp_ms: at_ms,
            detail: None,
        },
        MatchEvent::Hit {
            event_id,
            shooter_id,
            target_id,
            at_ms,
            ..
        } => MatchHistoryEvent {
            event_id: event_id.to_string(),
            event_type: "hit".into(),
            player_id: Some(shooter_id),
            timestamp_ms: at_ms,
            detail: Some(format!("hit {target_id}")),
        },
        MatchEvent::Completed {
            event_id,
            winner_id,
            at_ms,
            ..
        } => MatchHistoryEvent {
            event_id: event_id.to_string(),
            event_type: "completed".into(),
            player_id: Some(winner_id),
            timestamp_ms: at_ms,
            detail: None,
        },
    }
}

fn reciprocal_proximity(
    forward: Option<&ProximityReport>,
    reverse: Option<&ProximityReport>,
    evaluated_at_ms: u64,
) -> bool {
    let (Some(forward), Some(reverse)) = (forward, reverse) else {
        return false;
    };
    let recent = evaluated_at_ms.saturating_sub(forward.received_at_ms) <= 1_500
        && evaluated_at_ms.saturating_sub(reverse.received_at_ms) <= 1_500;
    let distance_agrees = match (forward.distance_meters, reverse.distance_meters) {
        (Some(a), Some(b)) => a <= 15.0 && b <= 15.0 && (a - b).abs() <= 2.0,
        _ => false,
    };
    recent && distance_agrees
}

async fn create_account(
    State(state): State<AppState>,
    Json(request): Json<CreateAccountRequest>,
) -> Response {
    let token = new_token();
    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::CreateAccount {
            handle: request.handle,
            display_name: request.display_name,
            token: token.clone(),
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match reply_rx.await {
        Ok(Ok(account)) => (
            StatusCode::CREATED,
            Json(AccountRegistration { account, token }),
        )
            .into_response(),
        Ok(Err(message)) => (StatusCode::CONFLICT, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn create_demo_session(
    State(state): State<AppState>,
    Json(request): Json<DemoSessionRequest>,
) -> Response {
    let token = new_token();
    let suffix = &Uuid::new_v4().simple().to_string()[..10];
    let display_name = request
        .display_name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("Player {}", &suffix[..4]));
    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::CreateAccount {
            handle: format!("player_{suffix}"),
            display_name: display_name.clone(),
            token: token.clone(),
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match reply_rx.await {
        Ok(Ok(account)) => Json(DemoSessionResponse {
            player_id: account.player_id,
            token,
            display_name,
        })
        .into_response(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn get_me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::GetAccount {
            player_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Some(account)) => Json(account).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn update_me(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateAccountRequest>,
) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::UpdateAccount {
            player_id,
            handle: request.handle,
            display_name: request.display_name,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    result_response(rx.await)
}

async fn find_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AccountSearchQuery>,
) -> Response {
    if authenticate(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::FindAccount {
            handle: query.handle,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Some(account)) => Json(serde_json::json!({ "player": account })).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn upsert_appearance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut profile): Json<AppearanceProfile>,
) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    profile.player_id = player_id;
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::UpsertAppearance { profile, reply: tx })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    result_response(rx.await)
}

async fn upsert_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut presence): Json<Presence>,
) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    presence.player_id = player_id;
    presence.updated_at_ms = now_ms();
    match state
        .store
        .send(StoreCommand::UpsertPresence { presence })
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn clear_presence(State(state): State<AppState>, headers: HeaderMap) -> StatusCode {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED;
    };
    match state
        .store
        .send(StoreCommand::ClearPresence { player_id })
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn list_friend_requests(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::ListFriendRequests {
            player_id: player_id.clone(),
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(list) => {
            let mut requests = Vec::new();
            for request in list.into_iter().filter(|request| {
                request.to_player_id == player_id && request.status == FriendRequestStatus::Pending
            }) {
                let (account_tx, account_rx) = oneshot::channel();
                if state
                    .store
                    .send(StoreCommand::GetAccount {
                        player_id: request.from_player_id.clone(),
                        reply: account_tx,
                    })
                    .await
                    .is_err()
                {
                    continue;
                }
                if let Ok(Some(account)) = account_rx.await {
                    requests.push(FriendRequestSummaryResponse {
                        request_id: request.request_id,
                        sender: FriendSummaryResponse {
                            player_id: account.player_id,
                            handle: account.handle,
                            display_name: account.display_name,
                            available: false,
                            last_seen_at_ms: None,
                        },
                        status: request.status,
                        created_at_ms: request.created_at_ms,
                    });
                }
            }
            Json(serde_json::json!({ "requests": requests })).into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn create_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateFriendRequestBody>,
) -> Response {
    let Ok(from_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let to_id = if let Some(id) = body.player_id {
        Some(id)
    } else if let Some(handle) = body.handle {
        let (tx, rx) = oneshot::channel();
        if state
            .store
            .send(StoreCommand::FindAccount { handle, reply: tx })
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        rx.await.ok().flatten().map(|account| account.player_id)
    } else {
        None
    };
    let Some(to_id) = to_id else {
        return (StatusCode::BAD_REQUEST, "playerId or handle is required").into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::CreateFriendRequest {
            from_id: from_id.clone(),
            to_id: to_id.clone(),
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Ok(request)) => {
            notify_revision(&state, [&from_id, &to_id], true);
            (StatusCode::CREATED, Json(request)).into_response()
        }
        Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn accept_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    resolve_friend_request(state, headers, id, true).await
}

async fn decline_friend_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    resolve_friend_request(state, headers, id, false).await
}

async fn resolve_friend_request(
    state: AppState,
    headers: HeaderMap,
    request_id: String,
    accept: bool,
) -> Response {
    let Ok(actor_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::ResolveFriendRequest {
            actor_id,
            request_id,
            accept,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Ok(request)) => {
            notify_revision(
                &state,
                [&request.from_player_id, &request.to_player_id],
                true,
            );
            Json(request).into_response()
        }
        Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn list_friends(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::ListFriends {
            player_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(list) => {
            let friends = list
                .into_iter()
                .map(|friend| FriendSummaryResponse {
                    player_id: friend.account.player_id,
                    handle: friend.account.handle,
                    display_name: friend.account.display_name,
                    available: friend.available,
                    last_seen_at_ms: None,
                })
                .collect::<Vec<_>>();
            Json(serde_json::json!({ "friends": friends })).into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn remove_friend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(friend_id): Path<String>,
) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::RemoveFriend {
            player_id: player_id.clone(),
            friend_id: friend_id.clone(),
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Ok(())) => {
            notify_revision(&state, [&player_id, &friend_id], true);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(message)) => (StatusCode::NOT_FOUND, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn create_invite(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(host_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::CreateMatch { host_id, reply: tx })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(snapshot) => Json(InviteResponse { snapshot }).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn create_match_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateMatchInviteBody>,
) -> Response {
    let Ok(host_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if let Some(to_id) = body.target_player_id {
        let (tx, rx) = oneshot::channel();
        if state
            .store
            .send(StoreCommand::CreateTargetInvitation {
                from_id: host_id.clone(),
                to_id: to_id.clone(),
                reply: tx,
            })
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return match rx.await {
            Ok(Ok((invitation, snapshot))) => {
                notify_revision(&state, [&host_id, &to_id], false);
                Json(serde_json::json!({
                    "invitation": invitation,
                    "snapshot": snapshot
                }))
                .into_response()
            }
            Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::CreateMatch { host_id, reply: tx })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(snapshot) => Json(InviteResponse { snapshot }).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn join_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Json(_request): Json<JoinRequest>,
) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::JoinMatch {
            invite_code: code,
            player_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Ok(snapshot)) => Json(InviteResponse { snapshot }).into_response(),
        Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn create_target_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateTargetInvitationBody>,
) -> Response {
    let Ok(from_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let to_id = body.friend_id;
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::CreateTargetInvitation {
            from_id: from_id.clone(),
            to_id: to_id.clone(),
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Ok((invitation, snapshot))) => {
            notify_revision(&state, [&from_id, &to_id], false);
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "invitation": invitation,
                    "snapshot": snapshot
                })),
            )
                .into_response()
        }
        Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn list_target_invitations(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::ListTargetInvitations {
            player_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(list) => Json(list).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn accept_target_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    resolve_target_invitation(state, headers, id, InvitationAction::Accept).await
}

async fn decline_target_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    resolve_target_invitation(state, headers, id, InvitationAction::Decline).await
}

async fn cancel_target_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    resolve_target_invitation(state, headers, id, InvitationAction::Cancel).await
}

async fn resolve_target_invitation(
    state: AppState,
    headers: HeaderMap,
    invitation_id: String,
    action: InvitationAction,
) -> Response {
    let Ok(actor_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::ResolveTargetInvitation {
            actor_id,
            invitation_id,
            action,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Ok((invitation, snapshot))) => {
            notify_revision(
                &state,
                [&invitation.from_player_id, &invitation.to_player_id],
                false,
            );
            Json(serde_json::json!({
                "invitation": invitation,
                "snapshot": snapshot
            }))
            .into_response()
        }
        Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn get_match(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(requester_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::GetMatch {
            match_id: id,
            requester_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    result_response(rx.await)
}

async fn list_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::ListHistory {
            player_id,
            cursor: query.cursor,
            limit,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    result_response(rx.await)
}

async fn get_match_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Ok(requester_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::GetMatchDetail {
            match_id: id,
            requester_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    result_response(rx.await)
}

async fn get_match_appearance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(player_id): Path<String>,
) -> Response {
    let Ok(requester_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::GetMatchAppearance {
            requester_id,
            player_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    result_response(rx.await)
}

async fn search_appearance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Response {
    if authenticate(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::SearchAppearance {
            query: query.q,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(hits) => Json(hits).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn search_nearby(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::SearchNearby {
            player_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(hits) => Json(hits).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn match_nearby(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (tx, rx) = oneshot::channel();
    if state
        .store
        .send(StoreCommand::MatchNearby {
            player_id,
            reply: tx,
        })
        .await
        .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match rx.await {
        Ok(Ok(snapshot)) => Json(InviteResponse { snapshot }).into_response(),
        Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn inspector_snapshot(State(state): State<AppState>) -> Json<InspectorSnapshotResponse> {
    let snapshot = state.store.snapshot_rx.borrow().clone();
    Json(snapshot.into())
}

async fn create_realtime_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<TicketRequest>,
) -> Response {
    let Ok(player_id) = authenticate(&state, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if let Some(match_id) = &request.match_id {
        let (tx, rx) = oneshot::channel();
        if state
            .store
            .send(StoreCommand::GetMatch {
                match_id: match_id.clone(),
                requester_id: player_id.clone(),
                reply: tx,
            })
            .await
            .is_err()
            || !matches!(rx.await, Ok(Ok(_)))
        {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let ticket = new_token();
    let issued_at_ms = now_ms();
    let expires_at_ms = issued_at_ms + WS_TICKET_LIFETIME_MS;
    let mut tickets = state.ws_tickets.lock().unwrap();
    tickets.retain(|_, record| record.expires_at_ms >= issued_at_ms);
    while tickets
        .values()
        .filter(|record| record.player_id == player_id)
        .count()
        >= MAX_OUTSTANDING_WS_TICKETS_PER_PLAYER
    {
        let oldest = tickets
            .iter()
            .filter(|(_, record)| record.player_id == player_id)
            .min_by_key(|(_, record)| record.expires_at_ms)
            .map(|(hash, _)| hash.clone());
        let Some(oldest) = oldest else { break };
        tickets.remove(&oldest);
    }
    tickets.insert(
        hash_token(&ticket),
        WsTicketRecord {
            player_id,
            match_id: request.match_id,
            expires_at_ms,
        },
    );
    drop(tickets);
    Json(TicketResponse {
        ticket,
        expires_at_ms,
    })
    .into_response()
}

async fn realtime_upgrade(
    State(state): State<AppState>,
    Query(query): Query<RealtimeQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let record = state
        .ws_tickets
        .lock()
        .unwrap()
        .remove(&hash_token(&query.ticket));
    let Some(record) = record.filter(|record| record.expires_at_ms >= now_ms()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if query
        .match_id
        .as_ref()
        .is_some_and(|id| record.match_id.as_ref() != Some(id))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.on_upgrade(move |socket| {
        handle_socket(
            socket,
            state,
            record.player_id,
            query.match_id.or(record.match_id),
        )
    })
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    player_id: PlayerId,
    selected_match_id: Option<MatchId>,
) {
    let (mut outgoing, mut incoming) = socket.split();
    let mut snapshots = state.store.snapshot_rx.clone();
    let mut directed = state.directed.subscribe();
    let mut sent_match_revisions: HashMap<MatchId, u64> = HashMap::new();
    let hello = ServerMessage::Hello {
        player_id: player_id.clone(),
        revision: snapshots.borrow().revision,
    };
    if send_json(&mut outgoing, &hello).await.is_err() {
        return;
    }
    let initial_snapshot = snapshots.borrow().clone();
    for match_snapshot in initial_snapshot.matches.iter().filter(|snapshot| {
        selected_match_id
            .as_ref()
            .is_none_or(|id| snapshot.match_id == *id)
            && snapshot
                .players
                .iter()
                .any(|player| player.player_id == player_id)
    }) {
        if send_json(
            &mut outgoing,
            &ServerMessage::MatchSnapshot {
                snapshot: match_snapshot.clone(),
            },
        )
        .await
        .is_err()
        {
            return;
        }
        sent_match_revisions.insert(match_snapshot.match_id.clone(), match_snapshot.revision);
    }
    loop {
        tokio::select! {
            changed = snapshots.changed() => {
                if changed.is_err() { return; }
                let snapshot = snapshots.borrow_and_update().clone();
                for match_snapshot in snapshot.matches.iter().filter(|snapshot| {
                    selected_match_id.as_ref().is_none_or(|id| snapshot.match_id == *id)
                        && snapshot.players.iter().any(|player| player.player_id == player_id)
                }) {
                    if sent_match_revisions.get(&match_snapshot.match_id) == Some(&match_snapshot.revision) {
                        continue;
                    }
                    if send_json(&mut outgoing, &ServerMessage::MatchSnapshot { snapshot: match_snapshot.clone() }).await.is_err() {
                        return;
                    }
                    sent_match_revisions.insert(match_snapshot.match_id.clone(), match_snapshot.revision);
                }
            }
            message = directed.recv() => {
                match message {
                    Ok(message) if message.player_id == player_id => {
                        if send_json(&mut outgoing, &message.message).await.is_err() { return; }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                    _ => {}
                }
            }
            message = incoming.next() => {
                let Some(Ok(Message::Text(text))) = message else { return; };
                let parsed = serde_json::from_str::<ClientMessage>(&text);
                let Err(error) = handle_client_message(&state, &player_id, parsed).await else { continue; };
                let _ = state.directed.send(DirectedMessage {
                    player_id: player_id.clone(),
                    message: ServerMessage::Error { message: error },
                });
            }
        }
    }
}

async fn handle_client_message(
    state: &AppState,
    player_id: &str,
    parsed: Result<ClientMessage, serde_json::Error>,
) -> Result<(), String> {
    match parsed.map_err(|error| error.to_string())? {
        ClientMessage::Heartbeat { .. } => Ok(()),
        ClientMessage::Presence { mut presence, .. } => {
            presence.player_id = player_id.to_string();
            presence.updated_at_ms = now_ms();
            state
                .store
                .send(StoreCommand::UpsertPresence { presence })
                .await
        }
        ClientMessage::Ready {
            command_id: _,
            match_id: _,
        } => Err("legacy ready is not supported by protocol v2".into()),
        ClientMessage::ReadyWithMetadata {
            command_id,
            match_id,
            calibration_model_version,
        } => {
            send_ready(
                state,
                player_id,
                command_id,
                match_id,
                calibration_model_version,
            )
            .await
        }
        ClientMessage::BriefingAcknowledged {
            command_id,
            match_id,
        } => {
            let (tx, rx) = oneshot::channel();
            state
                .store
                .send(StoreCommand::AcknowledgeBriefing {
                    command_id,
                    match_id,
                    player_id: player_id.to_string(),
                    reply: tx,
                })
                .await?;
            rx.await.map_err(|_| "briefing reply lost".to_string())??;
            Ok(())
        }
        ClientMessage::NearbyToken {
            match_id,
            peer_id,
            token,
            ..
        } => {
            let token_match_id = match_id.clone();
            let (tx, rx) = oneshot::channel();
            state
                .store
                .send(StoreCommand::GetMatch {
                    match_id,
                    requester_id: player_id.to_string(),
                    reply: tx,
                })
                .await?;
            let snapshot = rx.await.map_err(|_| "match lookup lost".to_string())??;
            if !snapshot
                .players
                .iter()
                .any(|player| player.player_id == peer_id)
            {
                return Err("nearby token peer is not in the match".into());
            }
            let peer_token = cache_nearby_token(
                &state.nearby_tokens,
                &token_match_id,
                player_id,
                &peer_id,
                token.clone(),
            );
            let _ = state.directed.send(DirectedMessage {
                player_id: peer_id.clone(),
                message: ServerMessage::NearbyToken {
                    player_id: player_id.to_string(),
                    token,
                },
            });
            // Discovery tokens are match-scoped but the WebSocket relay is
            // ephemeral. Replaying the peer's latest token makes reconnects
            // and asymmetric startup races recover as soon as either phone's
            // one-second retry reaches the server.
            if let Some(peer_token) = peer_token {
                let _ = state.directed.send(DirectedMessage {
                    player_id: player_id.to_string(),
                    message: ServerMessage::NearbyToken {
                        player_id: peer_id,
                        token: peer_token,
                    },
                });
            }
            Ok(())
        }
        ClientMessage::Proximity {
            command_id,
            match_id,
            peer_id,
            distance_meters,
            ..
        } => {
            state
                .store
                .send(StoreCommand::Proximity {
                    command_id,
                    match_id,
                    player_id: player_id.to_string(),
                    peer_id,
                    report: ProximityReport {
                        distance_meters,
                        received_at_ms: now_ms(),
                    },
                })
                .await
        }
        ClientMessage::Shot {
            command_id,
            match_id,
            target_id,
            mask_contains_reticle,
            target_score,
            fired_at_ms: _,
            ..
        } => {
            let (tx, rx) = oneshot::channel();
            state
                .store
                .send(StoreCommand::Shot {
                    command_id,
                    match_id,
                    shooter_id: player_id.to_string(),
                    target_id,
                    mask_contains_reticle,
                    target_score,
                    reply: tx,
                })
                .await?;
            let resolution = rx.await.map_err(|_| "shot reply lost".to_string())?;
            let _ = state.directed.send(DirectedMessage {
                player_id: player_id.to_string(),
                message: resolution,
            });
            Ok(())
        }
    }
}

async fn send_ready(
    state: &AppState,
    player_id: &str,
    command_id: Uuid,
    match_id: MatchId,
    calibration_model_version: String,
) -> Result<(), String> {
    let (tx, rx) = oneshot::channel();
    state
        .store
        .send(StoreCommand::Ready {
            command_id,
            match_id,
            player_id: player_id.to_string(),
            calibration_model_version,
            reply: tx,
        })
        .await?;
    rx.await.map_err(|_| "ready reply lost".to_string())??;
    Ok(())
}

async fn send_json(
    outgoing: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: &ServerMessage,
) -> Result<(), axum::Error> {
    outgoing
        .send(Message::Text(serde_json::to_string(value).unwrap().into()))
        .await
}

async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<PlayerId, ()> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or(())?;
    let (tx, rx) = oneshot::channel();
    state
        .store
        .send(StoreCommand::Authenticate {
            token_hash: hash_token(token),
            reply: tx,
        })
        .await
        .map_err(|_| ())?;
    rx.await.map_err(|_| ())?.ok_or(())
}

fn notify_revision<'a>(
    state: &AppState,
    players: impl IntoIterator<Item = &'a String>,
    social: bool,
) {
    let revision = state.store.snapshot_rx.borrow().revision;
    for player_id in players {
        let message = if social {
            ServerMessage::SocialRevision { revision }
        } else {
            ServerMessage::InvitationRevision { revision }
        };
        let _ = state.directed.send(DirectedMessage {
            player_id: player_id.clone(),
            message,
        });
    }
}

fn result_response<T: Serialize>(
    result: Result<Result<T, String>, oneshot::error::RecvError>,
) -> Response {
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(message)) => (StatusCode::BAD_REQUEST, message).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn validate_account_fields(handle: &str, display_name: &str) -> Result<(), String> {
    let normalized = normalize_handle(handle);
    if normalized.len() < 3
        || normalized.len() > 20
        || !normalized.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
    {
        return Err("handle must be 3-20 letters, numbers, hyphens, or underscores".into());
    }
    let name = display_name.trim();
    if name.is_empty() || name.chars().count() > 40 {
        return Err("displayName must be 1-40 characters".into());
    }
    Ok(())
}

fn normalize_handle(handle: &str) -> String {
    handle.trim().to_ascii_lowercase()
}

fn friendship_key(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}:{b}")
    } else {
        format!("{b}:{a}")
    }
}

fn new_token() -> String {
    Alphanumeric.sample_string(&mut rand::thread_rng(), 48)
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn encode_cursor(completed_at_ms: u64, match_id: &str) -> String {
    format!("{completed_at_ms}:{match_id}")
}

fn parse_cursor(cursor: Option<&str>) -> Result<Option<(u64, String)>, String> {
    cursor
        .map(|cursor| {
            let (time, match_id) = cursor
                .split_once(':')
                .ok_or_else(|| "invalid cursor".to_string())?;
            let time = time
                .parse::<u64>()
                .map_err(|_| "invalid cursor".to_string())?;
            if match_id.is_empty() {
                return Err("invalid cursor".into());
            }
            Ok((time, match_id.to_string()))
        })
        .transpose()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

const INSPECTOR_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Untitled FPS · Bogkit Inspector</title><style>
body{font:14px ui-monospace,SFMono-Regular,monospace;margin:0;background:#0b0c10;color:#e8edf2}
header{padding:18px 24px;border-bottom:1px solid #29313a;display:flex;gap:20px;align-items:center}
h1{font-size:18px;margin:0}.badge{color:#74f2a7}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:14px;padding:18px}
section{border:1px solid #29313a;border-radius:10px;padding:14px;background:#11151a}h2{font-size:14px;color:#8fc7ff;margin:0 0 10px}
pre{white-space:pre-wrap;word-break:break-word;margin:0;color:#c8d1dc}</style></head>
<body><header><h1>UNTITLED FPS / BOGKIT</h1><span class="badge">FOLD · ESE · ANNY</span><span id="rev"></span></header>
<div class="grid"><section><h2>Server identity</h2><pre id="server"></pre></section>
<section><h2>ANNy / BM25 index sizes</h2><pre id="indexes"></pre></section>
<section><h2>Search latency (µs, last query)</h2><pre id="latency"></pre></section>
<section><h2>Redacted materializations</h2><pre id="counts"></pre></section>
<section><h2>Appearance search &amp; re-enrollment stats</h2><pre id="stats"></pre></section>
<section><h2>Presence diagnostics (coordinates redacted)</h2><pre id="presence"></pre></section>
<section><h2>Match snapshots</h2><pre id="matches"></pre></section></div>
<script>const fmt=v=>JSON.stringify(v,null,2);async function refresh(){const [h,s]=await Promise.all([fetch('/health').then(r=>r.json()),fetch('/v1/inspector/snapshot').then(r=>r.json())]);rev.textContent=`revision ${s.revision}`;server.textContent=fmt(h);indexes.textContent=fmt(s.indexSizes);latency.textContent=fmt(s.searchLatencyMicros);counts.textContent=fmt(s.materializationCounts);stats.textContent=fmt({processedCommands:s.processedCommands,searchStats:s.searchStats});presence.textContent=fmt(s.presences);matches.textContent=fmt(s.matches)}setInterval(refresh,1000);refresh();</script>
</body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("fps-{label}-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    async fn create_test_account(store: &StoreHandle, handle: &str, token: &str) -> Account {
        let (tx, rx) = oneshot::channel();
        store
            .send(StoreCommand::CreateAccount {
                handle: handle.into(),
                display_name: handle.into(),
                token: token.into(),
                reply: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap().unwrap()
    }

    fn test_appearance(player_id: &str) -> AppearanceProfile {
        AppearanceProfile {
            player_id: player_id.into(),
            display_name: player_id.into(),
            generated_description: "red jacket and dark jeans".into(),
            embedding_model: "test-v1".into(),
            descriptor_model: "test-v1".into(),
            whole_body_embedding: vec![0.0; VISUAL_DIMENSIONS],
            face_embeddings: Vec::new(),
            upper_body_embeddings: Vec::new(),
            lower_body_embeddings: Vec::new(),
            head_accessory_embeddings: Vec::new(),
            silhouette_descriptor: vec![0.0; 64],
            briefing_thumbnail: None,
            skin: None,
            updated_at_ms: now_ms(),
        }
    }

    fn legacy_test_appearance(player_id: &str) -> LegacyAppearanceProfileV2 {
        let profile = test_appearance(player_id);
        LegacyAppearanceProfileV2 {
            player_id: profile.player_id,
            display_name: profile.display_name,
            generated_description: profile.generated_description,
            embedding_model: profile.embedding_model,
            descriptor_model: profile.descriptor_model,
            whole_body_embedding: profile.whole_body_embedding,
            face_embeddings: profile.face_embeddings,
            upper_body_embeddings: profile.upper_body_embeddings,
            lower_body_embeddings: profile.lower_body_embeddings,
            head_accessory_embeddings: profile.head_accessory_embeddings,
            silhouette_descriptor: profile.silhouette_descriptor,
            briefing_thumbnail: profile.briefing_thumbnail,
            updated_at_ms: profile.updated_at_ms,
        }
    }

    fn located_presence(player_id: &str, latitude: f64, longitude: f64) -> Presence {
        Presence {
            player_id: player_id.into(),
            latitude,
            longitude,
            horizontal_accuracy: 10.0,
            foreground: true,
            updated_at_ms: now_ms(),
        }
    }

    async fn enroll(store: &StoreHandle, player_id: &str, description: &str) {
        let mut profile = test_appearance(player_id);
        profile.generated_description = description.into();
        let (tx, rx) = oneshot::channel();
        store
            .send(StoreCommand::UpsertAppearance { profile, reply: tx })
            .await
            .unwrap();
        rx.await.unwrap().unwrap();
    }

    async fn appearance_search(store: &StoreHandle, query: &str) -> Vec<SearchHit> {
        let (tx, rx) = oneshot::channel();
        store
            .send(StoreCommand::SearchAppearance {
                query: query.into(),
                reply: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    #[test]
    fn v2_decoder_accepts_the_skinned_postcard_layout() {
        let mut profile = test_appearance("skinned-player");
        profile.skin = Some("green_camo".into());
        let encoded = postcard::to_stdvec(&profile).unwrap();
        assert_eq!(decode_v2_appearance(&encoded).unwrap(), profile);
    }

    #[tokio::test]
    async fn startup_migrates_legacy_v2_appearances_and_rebuilds_indexes() {
        let path = test_dir("appearance-v2-migration");
        let v2_path = path.join(APPEARANCE_V2_STORE);
        let legacy = legacy_test_appearance("legacy-player");
        let expected_updated_at_ms = legacy.updated_at_ms;
        let mut v2 = KeyedStream::new(
            &v2_path,
            terminal::Table::<PlayerId, LegacyAppearanceProfileV2>::new("appearance_table"),
        );
        v2.wtx(|tx| {
            tx.upsert(&legacy.player_id.clone(), &legacy);
        });
        v2.checkpoint();
        drop(v2);

        let store = StoreHandle::spawn(path.clone());
        let mut snapshots = store.snapshot_rx.clone();
        let snapshot = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let snapshot = snapshots.borrow_and_update().clone();
                if !snapshot.appearances.is_empty() {
                    break snapshot;
                }
                snapshots.changed().await.unwrap();
            }
        })
        .await
        .expect("appearance migration should publish a snapshot");

        assert_eq!(snapshot.appearances.len(), 1);
        assert_eq!(snapshot.appearances[0].player_id, "legacy-player");
        assert_eq!(snapshot.appearances[0].skin, None);
        assert_eq!(
            snapshot.appearances[0].updated_at_ms,
            expected_updated_at_ms
        );
        assert_eq!(snapshot.index_sizes.get("appearanceBm25Docs"), Some(&1));
        assert_eq!(snapshot.index_sizes.get("appearanceSemanticHnsw"), Some(&1));
        assert_eq!(snapshot.index_sizes.get("appearanceVisualHnsw"), Some(&1));
        assert!(v2_path.exists(), "v2 remains as the migration backup");
        assert!(path.join(APPEARANCE_V3_STORE).exists());
        assert!(path.join(APPEARANCE_V3_MIGRATION_MARKER).exists());
    }

    async fn match_nearby_for(
        store: &StoreHandle,
        player_id: &str,
    ) -> Result<MatchSnapshot, String> {
        let (tx, rx) = oneshot::channel();
        store
            .send(StoreCommand::MatchNearby {
                player_id: player_id.into(),
                reply: tx,
            })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    #[tokio::test]
    async fn nearby_matchmaking_pairs_two_waiting_players() {
        let path = test_dir("nearby-match");
        let store = StoreHandle::spawn(path);
        let alpha = create_test_account(&store, "alpha", "alpha-secret").await;
        let beta = create_test_account(&store, "beta", "beta-secret").await;

        // Both available and standing a few meters apart in New York.
        store
            .send(StoreCommand::UpsertPresence {
                presence: located_presence(&alpha.player_id, 40.7128, -74.0060),
            })
            .await
            .unwrap();
        store
            .send(StoreCommand::UpsertPresence {
                presence: located_presence(&beta.player_id, 40.7128, -74.0061),
            })
            .await
            .unwrap();

        // First player has nobody to pair with, so they open a slot and wait.
        let waiting = match_nearby_for(&store, &alpha.player_id).await.unwrap();
        assert_eq!(waiting.players.len(), 1);
        assert_eq!(waiting.players[0].player_id, alpha.player_id);

        // Calling again while still waiting is idempotent: same slot, still one player.
        let again = match_nearby_for(&store, &alpha.player_id).await.unwrap();
        assert_eq!(again.match_id, waiting.match_id);
        assert_eq!(again.players.len(), 1);

        // The nearby second player joins the waiting slot rather than opening a new one.
        let paired = match_nearby_for(&store, &beta.player_id).await.unwrap();
        assert_eq!(paired.match_id, waiting.match_id);
        assert_eq!(paired.players.len(), 2);
        assert!(paired.players.iter().any(|p| p.player_id == beta.player_id));
    }

    #[tokio::test]
    async fn nearby_matchmaking_requires_usable_location() {
        let path = test_dir("nearby-noloc");
        let store = StoreHandle::spawn(path);
        let solo = create_test_account(&store, "solo", "solo-secret").await;
        // A location-free heartbeat (negative accuracy) is not usable for matchmaking.
        store
            .send(StoreCommand::UpsertPresence {
                presence: Presence {
                    player_id: solo.player_id.clone(),
                    latitude: 0.0,
                    longitude: 0.0,
                    horizontal_accuracy: -1.0,
                    foreground: true,
                    updated_at_ms: now_ms(),
                },
            })
            .await
            .unwrap();
        assert!(match_nearby_for(&store, &solo.player_id).await.is_err());
    }

    #[tokio::test]
    async fn reenrollment_retracts_prior_appearance_from_search() {
        let path = test_dir("reenroll");
        let store = StoreHandle::spawn(path);
        let player = create_test_account(&store, "chameleon", "cham-secret").await;

        // Enroll a red-jacket outfit and confirm it is searchable.
        enroll(&store, &player.player_id, "red jacket and dark jeans").await;
        assert!(
            appearance_search(&store, "red jacket")
                .await
                .iter()
                .any(|hit| hit.player_id == player.player_id)
        );

        // Re-enroll a completely different outfit for the same player.
        enroll(&store, &player.player_id, "green hoodie and grey shorts").await;

        // Source of truth: the prior profile was retracted and replaced in place —
        // exactly one profile remains for the player, carrying the new description.
        // The inspector snapshot is published on a watch channel just after the upsert
        // reply, so wait for it to reflect the re-enrollment before asserting.
        let mut snapshot_rx = store.snapshot_rx.clone();
        let snapshot = loop {
            let snapshot = snapshot_rx.borrow_and_update().clone();
            if snapshot.appearances.iter().any(|profile| {
                profile.player_id == player.player_id
                    && profile.generated_description == "green hoodie and grey shorts"
            }) {
                break snapshot;
            }
            snapshot_rx.changed().await.unwrap();
        };
        let profiles: Vec<_> = snapshot
            .appearances
            .iter()
            .filter(|profile| profile.player_id == player.player_id)
            .collect();
        assert_eq!(profiles.len(), 1);
        assert_eq!(
            profiles[0].generated_description,
            "green hoodie and grey shorts"
        );

        // Derived Bog sinks retracted+reinserted rather than appending: the BM25 corpus
        // and both HNSWs did not grow, and the re-enrollment was counted for the demo.
        assert_eq!(snapshot.index_sizes.get("appearanceBm25Docs"), Some(&1));
        assert_eq!(snapshot.index_sizes.get("appearanceSemanticHnsw"), Some(&1));
        assert_eq!(snapshot.index_sizes.get("appearanceVisualHnsw"), Some(&1));
        assert_eq!(
            snapshot.search_stats.get("appearanceReenrollments"),
            Some(&1)
        );

        // The new outfit is queryable.
        assert!(
            appearance_search(&store, "green hoodie")
                .await
                .iter()
                .any(|hit| hit.player_id == player.player_id)
        );
    }

    #[test]
    fn reciprocal_samples_must_be_recent_close_and_agree() {
        let forward = ProximityReport {
            distance_meters: Some(4.0),
            received_at_ms: 1_000,
        };
        let reverse = ProximityReport {
            distance_meters: Some(4.5),
            received_at_ms: 1_100,
        };
        assert!(reciprocal_proximity(Some(&forward), Some(&reverse), 1_500));
        assert!(!reciprocal_proximity(Some(&forward), Some(&reverse), 3_000));
    }

    #[test]
    fn nearby_token_cache_recovers_asymmetric_relay_order() {
        let tokens = Mutex::new(HashMap::new());

        assert_eq!(
            cache_nearby_token(&tokens, "match-a", "alpha", "beta", "alpha-1".into()),
            None
        );
        assert_eq!(
            cache_nearby_token(&tokens, "match-a", "beta", "alpha", "beta-1".into()),
            Some("alpha-1".into())
        );
        assert_eq!(
            cache_nearby_token(&tokens, "match-a", "alpha", "beta", "alpha-2".into()),
            Some("beta-1".into())
        );
        assert_eq!(
            cache_nearby_token(&tokens, "match-b", "alpha", "beta", "other".into()),
            None
        );
    }

    #[tokio::test]
    async fn nearby_token_handler_replays_the_cached_peer_token() {
        let path = test_dir("nearby-token-relay");
        let server_info = load_server_info(&path).unwrap();
        let state = new_state(path, server_info);
        let alpha = create_test_account(&state.store, "relay-alpha", "alpha-token").await;
        let beta = create_test_account(&state.store, "relay-beta", "beta-token").await;
        let (create_tx, create_rx) = oneshot::channel();
        state
            .store
            .send(StoreCommand::CreateMatch {
                host_id: alpha.player_id.clone(),
                reply: create_tx,
            })
            .await
            .unwrap();
        let created = create_rx.await.unwrap();
        let (join_tx, join_rx) = oneshot::channel();
        state
            .store
            .send(StoreCommand::JoinMatch {
                invite_code: created.invite_code,
                player_id: beta.player_id.clone(),
                reply: join_tx,
            })
            .await
            .unwrap();
        let joined = join_rx.await.unwrap().unwrap();
        let mut directed = state.directed.subscribe();

        handle_client_message(
            &state,
            &alpha.player_id,
            Ok(ClientMessage::NearbyToken {
                command_id: Uuid::new_v4(),
                match_id: joined.match_id.clone(),
                peer_id: beta.player_id.clone(),
                token: "alpha-discovery".into(),
            }),
        )
        .await
        .unwrap();
        let initial = directed.recv().await.unwrap();
        assert_eq!(initial.player_id, beta.player_id);

        handle_client_message(
            &state,
            &beta.player_id,
            Ok(ClientMessage::NearbyToken {
                command_id: Uuid::new_v4(),
                match_id: joined.match_id,
                peer_id: alpha.player_id.clone(),
                token: "beta-discovery".into(),
            }),
        )
        .await
        .unwrap();
        let to_alpha = directed.recv().await.unwrap();
        let replay_to_beta = directed.recv().await.unwrap();
        assert_eq!(to_alpha.player_id, alpha.player_id);
        assert_eq!(replay_to_beta.player_id, beta.player_id);
        assert!(matches!(
            replay_to_beta.message,
            ServerMessage::NearbyToken {
                player_id,
                token
            } if player_id == alpha.player_id && token == "alpha-discovery"
        ));
    }

    #[test]
    fn handles_normalize_and_validate() {
        assert_eq!(normalize_handle("  Player_42 "), "player_42");
        assert!(validate_account_fields("Player_42", "Player").is_ok());
        assert!(validate_account_fields("no spaces", "Player").is_err());
        assert!(validate_account_fields("ok_name", " ").is_err());
    }

    #[test]
    fn opaque_token_hash_is_stable_and_does_not_contain_token() {
        let token = "secret-device-token";
        let hash = hash_token(token);
        assert_eq!(hash.len(), 64);
        assert_eq!(hash, hash_token(token));
        assert!(!hash.contains(token));
    }

    #[test]
    fn friendship_keys_are_order_independent() {
        assert_eq!(friendship_key("a", "b"), friendship_key("b", "a"));
    }

    #[test]
    fn cursor_roundtrips_and_rejects_invalid_values() {
        let encoded = encode_cursor(42, "match-id");
        assert_eq!(
            parse_cursor(Some(&encoded)).unwrap(),
            Some((42, "match-id".into()))
        );
        assert!(parse_cursor(Some("bad")).is_err());
    }

    #[test]
    fn public_inspector_shape_omits_invites_players_and_appearance_payloads() {
        let snapshot = InspectorSnapshot {
            revision: 3,
            appearances: vec![test_appearance("private-player")],
            presences: Vec::new(),
            matches: vec![MatchSnapshot::new(
                "private-match".into(),
                "SECRET".into(),
                "private-player".into(),
                1,
            )],
            processed_commands: 0,
            search_stats: BTreeMap::new(),
            materialization_counts: BTreeMap::new(),
            index_sizes: BTreeMap::new(),
            search_latency_micros: BTreeMap::new(),
        };
        let json = serde_json::to_string(&InspectorSnapshotResponse::from(snapshot)).unwrap();
        assert!(!json.contains("SECRET"));
        assert!(!json.contains("private-player"));
        assert!(!json.contains("private-match"));
        assert!(!json.contains("appearance"));
    }

    #[test]
    fn server_identity_persists() {
        let path = test_dir("server-identity");
        let first = load_server_info(&path).unwrap();
        let second = load_server_info(&path).unwrap();
        assert_eq!(first.server_id, second.server_id);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn account_auth_and_social_materializations_are_enforced() {
        let path = test_dir("social");
        let store = StoreHandle::spawn(path);
        let alpha = create_test_account(&store, "Alpha", "alpha-secret").await;
        let beta = create_test_account(&store, "beta", "beta-secret").await;

        let (duplicate_tx, duplicate_rx) = oneshot::channel();
        store
            .send(StoreCommand::CreateAccount {
                handle: " ALPHA ".into(),
                display_name: "Duplicate".into(),
                token: "other".into(),
                reply: duplicate_tx,
            })
            .await
            .unwrap();
        assert!(duplicate_rx.await.unwrap().is_err());

        let (auth_tx, auth_rx) = oneshot::channel();
        store
            .send(StoreCommand::Authenticate {
                token_hash: hash_token("alpha-secret"),
                reply: auth_tx,
            })
            .await
            .unwrap();
        assert_eq!(auth_rx.await.unwrap(), Some(alpha.player_id.clone()));
        let (bad_tx, bad_rx) = oneshot::channel();
        store
            .send(StoreCommand::Authenticate {
                token_hash: hash_token(&alpha.player_id),
                reply: bad_tx,
            })
            .await
            .unwrap();
        assert_eq!(bad_rx.await.unwrap(), None);

        let (request_tx, request_rx) = oneshot::channel();
        store
            .send(StoreCommand::CreateFriendRequest {
                from_id: alpha.player_id.clone(),
                to_id: beta.player_id.clone(),
                reply: request_tx,
            })
            .await
            .unwrap();
        let request = request_rx.await.unwrap().unwrap();
        let request_id = request.request_id;
        let (accept_tx, accept_rx) = oneshot::channel();
        store
            .send(StoreCommand::ResolveFriendRequest {
                actor_id: beta.player_id.clone(),
                request_id: request_id.clone(),
                accept: true,
                reply: accept_tx,
            })
            .await
            .unwrap();
        assert_eq!(
            accept_rx.await.unwrap().unwrap().status,
            FriendRequestStatus::Accepted
        );
        let (retry_tx, retry_rx) = oneshot::channel();
        store
            .send(StoreCommand::ResolveFriendRequest {
                actor_id: beta.player_id.clone(),
                request_id,
                accept: true,
                reply: retry_tx,
            })
            .await
            .unwrap();
        assert_eq!(
            retry_rx.await.unwrap().unwrap().status,
            FriendRequestStatus::Accepted
        );
        let (invite_tx, invite_rx) = oneshot::channel();
        store
            .send(StoreCommand::CreateTargetInvitation {
                from_id: alpha.player_id.clone(),
                to_id: beta.player_id.clone(),
                reply: invite_tx,
            })
            .await
            .unwrap();
        let (invitation, snapshot) = invite_rx.await.unwrap().unwrap();
        assert_eq!(invitation.match_id, snapshot.match_id);
        assert_eq!(invitation.status, MatchInvitationStatus::Pending);
        for _ in 0..2 {
            let (resolve_tx, resolve_rx) = oneshot::channel();
            store
                .send(StoreCommand::ResolveTargetInvitation {
                    actor_id: beta.player_id.clone(),
                    invitation_id: invitation.invitation_id.clone(),
                    action: InvitationAction::Accept,
                    reply: resolve_tx,
                })
                .await
                .unwrap();
            assert_eq!(
                resolve_rx.await.unwrap().unwrap().0.status,
                MatchInvitationStatus::Accepted
            );
        }
        for _ in 0..2 {
            let (remove_tx, remove_rx) = oneshot::channel();
            store
                .send(StoreCommand::RemoveFriend {
                    player_id: alpha.player_id.clone(),
                    friend_id: beta.player_id.clone(),
                    reply: remove_tx,
                })
                .await
                .unwrap();
            assert!(remove_rx.await.unwrap().is_ok());
        }
    }

    #[tokio::test]
    async fn completed_match_is_materialized_for_both_players() {
        let store = StoreHandle::spawn(test_dir("history"));
        let alpha = create_test_account(&store, "alpha", "alpha-token").await;
        let beta = create_test_account(&store, "beta", "beta-token").await;
        for account in [&alpha, &beta] {
            let (tx, rx) = oneshot::channel();
            store
                .send(StoreCommand::UpsertAppearance {
                    profile: test_appearance(&account.player_id),
                    reply: tx,
                })
                .await
                .unwrap();
            rx.await.unwrap().unwrap();
        }
        let (create_tx, create_rx) = oneshot::channel();
        store
            .send(StoreCommand::CreateMatch {
                host_id: alpha.player_id.clone(),
                reply: create_tx,
            })
            .await
            .unwrap();
        let created = create_rx.await.unwrap();
        let (join_tx, join_rx) = oneshot::channel();
        store
            .send(StoreCommand::JoinMatch {
                invite_code: created.invite_code,
                player_id: beta.player_id.clone(),
                reply: join_tx,
            })
            .await
            .unwrap();
        let joined = join_rx.await.unwrap().unwrap();
        let (lobby_appearance_tx, lobby_appearance_rx) = oneshot::channel();
        store
            .send(StoreCommand::GetMatchAppearance {
                requester_id: alpha.player_id.clone(),
                player_id: beta.player_id.clone(),
                reply: lobby_appearance_tx,
            })
            .await
            .unwrap();
        assert!(lobby_appearance_rx.await.unwrap().is_err());

        let (invalid_ready_tx, invalid_ready_rx) = oneshot::channel();
        store
            .send(StoreCommand::Ready {
                command_id: Uuid::new_v4(),
                match_id: joined.match_id.clone(),
                player_id: alpha.player_id.clone(),
                calibration_model_version: "arbitrary".into(),
                reply: invalid_ready_tx,
            })
            .await
            .unwrap();
        assert!(invalid_ready_rx.await.unwrap().is_err());

        for player_id in [&alpha.player_id, &beta.player_id] {
            let (tx, rx) = oneshot::channel();
            store
                .send(StoreCommand::Ready {
                    command_id: Uuid::new_v4(),
                    match_id: joined.match_id.clone(),
                    player_id: player_id.clone(),
                    calibration_model_version: REQUIRED_CALIBRATION_MODEL.into(),
                    reply: tx,
                })
                .await
                .unwrap();
            rx.await.unwrap().unwrap();
        }
        let (briefing_appearance_tx, briefing_appearance_rx) = oneshot::channel();
        store
            .send(StoreCommand::GetMatchAppearance {
                requester_id: alpha.player_id.clone(),
                player_id: beta.player_id.clone(),
                reply: briefing_appearance_tx,
            })
            .await
            .unwrap();
        assert!(briefing_appearance_rx.await.unwrap().is_ok());
        for player_id in [&alpha.player_id, &beta.player_id] {
            let (tx, rx) = oneshot::channel();
            store
                .send(StoreCommand::AcknowledgeBriefing {
                    command_id: Uuid::new_v4(),
                    match_id: joined.match_id.clone(),
                    player_id: player_id.clone(),
                    reply: tx,
                })
                .await
                .unwrap();
            rx.await.unwrap().unwrap();
        }
        for (player_id, peer_id) in [
            (&alpha.player_id, &beta.player_id),
            (&beta.player_id, &alpha.player_id),
        ] {
            store
                .send(StoreCommand::Proximity {
                    command_id: Uuid::new_v4(),
                    match_id: joined.match_id.clone(),
                    player_id: player_id.clone(),
                    peer_id: peer_id.clone(),
                    report: ProximityReport {
                        distance_meters: Some(3.0),
                        received_at_ms: now_ms(),
                    },
                })
                .await
                .unwrap();
        }
        for _ in 0..3 {
            let (tx, rx) = oneshot::channel();
            store
                .send(StoreCommand::Shot {
                    command_id: Uuid::new_v4(),
                    match_id: joined.match_id.clone(),
                    shooter_id: alpha.player_id.clone(),
                    target_id: beta.player_id.clone(),
                    mask_contains_reticle: true,
                    target_score: 1.0,
                    reply: tx,
                })
                .await
                .unwrap();
            assert!(matches!(
                rx.await.unwrap(),
                ServerMessage::ShotResolution { accepted: true, .. }
            ));
        }
        for (player_id, expected_result) in [(&alpha.player_id, "won"), (&beta.player_id, "lost")] {
            let (tx, rx) = oneshot::channel();
            store
                .send(StoreCommand::ListHistory {
                    player_id: player_id.clone(),
                    cursor: None,
                    limit: 25,
                    reply: tx,
                })
                .await
                .unwrap();
            let page = rx.await.unwrap().unwrap();
            assert_eq!(page.matches.len(), 1);
            assert_eq!(page.matches[0].result, expected_result);
            assert_eq!(page.matches[0].match_id, joined.match_id);
        }
    }
}
