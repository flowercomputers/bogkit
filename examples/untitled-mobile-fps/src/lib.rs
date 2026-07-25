use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 2;
pub const VISUAL_DIMENSIONS: usize = 512;
pub const MATCH_RADIUS_METERS: f64 = 3_219.0;
pub const MAX_HEALTH: u8 = 3;
pub const PRESENCE_FRESHNESS_MS: u64 = 30_000;
pub const INVITE_LIFETIME_MS: u64 = 10 * 60 * 1_000;

pub type PlayerId = String;
pub type MatchId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub server_id: String,
    pub display_name: String,
    pub environment: String,
    pub protocol_version: u32,
    pub capabilities: Vec<String>,
    pub minimum_client_version: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceStatus {
    Missing,
    Registered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub player_id: PlayerId,
    pub handle: String,
    pub display_name: String,
    pub appearance_status: AppearanceStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountRegistration {
    pub account: Account,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceProfile {
    pub player_id: PlayerId,
    pub display_name: String,
    pub generated_description: String,
    pub embedding_model: String,
    pub descriptor_model: String,
    pub whole_body_embedding: Vec<f32>,
    #[serde(default)]
    pub face_embeddings: Vec<Vec<f32>>,
    #[serde(default)]
    pub upper_body_embeddings: Vec<Vec<f32>>,
    #[serde(default)]
    pub lower_body_embeddings: Vec<Vec<f32>>,
    #[serde(default)]
    pub head_accessory_embeddings: Vec<Vec<f32>>,
    #[serde(default)]
    pub silhouette_descriptor: Vec<f32>,
    pub briefing_thumbnail: Option<String>,
    /// Cosmetic silhouette skin the player picked. Stored and returned
    /// verbatim; the server never interprets the value, so clients can add
    /// skins without a server release.
    #[serde(default)]
    pub skin: Option<String>,
    pub updated_at_ms: u64,
}

impl AppearanceProfile {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.player_id.trim().is_empty() || self.display_name.trim().is_empty() {
            return Err("playerId and displayName are required");
        }
        if self.generated_description.trim().is_empty() {
            return Err("generatedDescription is required");
        }
        if self.whole_body_embedding.len() != VISUAL_DIMENSIONS {
            return Err("wholeBodyEmbedding must contain 512 values");
        }
        if self
            .whole_body_embedding
            .iter()
            .any(|value| !value.is_finite())
        {
            return Err("embedding values must be finite");
        }
        let regional = self
            .face_embeddings
            .iter()
            .chain(&self.upper_body_embeddings)
            .chain(&self.lower_body_embeddings)
            .chain(&self.head_accessory_embeddings);
        if regional
            .clone()
            .any(|embedding| embedding.len() != VISUAL_DIMENSIONS)
        {
            return Err("regional and face embeddings must contain 512 values");
        }
        if regional.flatten().any(|value| !value.is_finite()) {
            return Err("regional and face embedding values must be finite");
        }
        if !self.silhouette_descriptor.is_empty()
            && (self.silhouette_descriptor.len() != 64
                || self
                    .silhouette_descriptor
                    .iter()
                    .any(|value| !value.is_finite()))
        {
            return Err("silhouetteDescriptor must contain 64 finite values");
        }
        Ok(())
    }

    pub fn searchable_embedding(&self) -> [f32; VISUAL_DIMENSIONS] {
        normalize_vector(&self.whole_body_embedding)
    }

    pub fn redacted_for_global_view(&self) -> Self {
        let mut redacted = self.clone();
        redacted.face_embeddings.clear();
        redacted.upper_body_embeddings.clear();
        redacted.lower_body_embeddings.clear();
        redacted.head_accessory_embeddings.clear();
        redacted.silhouette_descriptor.clear();
        redacted.briefing_thumbnail = None;
        redacted
    }
}

pub fn normalize_vector<const N: usize>(values: &[f32]) -> [f32; N] {
    let mut result = [0.0; N];
    for (output, input) in result.iter_mut().zip(values.iter().copied()) {
        *output = if input.is_finite() { input } else { 0.0 };
    }
    let magnitude = result.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude > f32::EPSILON {
        for value in &mut result {
            *value /= magnitude;
        }
    }
    result
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Presence {
    pub player_id: PlayerId,
    pub latitude: f64,
    pub longitude: f64,
    pub horizontal_accuracy: f64,
    pub foreground: bool,
    pub updated_at_ms: u64,
}

impl Presence {
    pub fn has_usable_location(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && self.horizontal_accuracy.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && self.horizontal_accuracy >= 0.0
    }

    pub fn earth_vector(&self) -> [f32; 3] {
        let latitude = self.latitude.to_radians();
        let longitude = self.longitude.to_radians();
        [
            (latitude.cos() * longitude.cos()) as f32,
            (latitude.cos() * longitude.sin()) as f32,
            latitude.sin() as f32,
        ]
    }

    pub fn distance_meters(&self, other: &Presence) -> f64 {
        haversine_meters(
            self.latitude,
            self.longitude,
            other.latitude,
            other.longitude,
        )
    }

    pub fn is_available_at(&self, now_ms: u64) -> bool {
        self.foreground && now_ms.saturating_sub(self.updated_at_ms) <= PRESENCE_FRESHNESS_MS
    }
}

pub fn haversine_meters(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    let earth_radius = 6_371_008.8;
    let d_lat = (lat_b - lat_a).to_radians();
    let d_lon = (lon_b - lon_a).to_radians();
    let lat_a = lat_a.to_radians();
    let lat_b = lat_b.to_radians();
    let haversine =
        (d_lat / 2.0).sin().powi(2) + lat_a.cos() * lat_b.cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * earth_radius * haversine.sqrt().asin()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    Lobby,
    Briefing,
    Active,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlayerMatchState {
    pub player_id: PlayerId,
    pub health: u8,
    pub ready: bool,
    pub eliminated: bool,
    #[serde(default)]
    pub calibration_model_version: Option<String>,
    #[serde(default)]
    pub briefing_acknowledged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchSnapshot {
    pub protocol_version: u32,
    pub revision: u64,
    pub match_id: MatchId,
    pub invite_code: String,
    pub invite_expires_at_ms: u64,
    pub status: MatchStatus,
    pub players: Vec<PlayerMatchState>,
    pub winner: Option<PlayerId>,
    pub created_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    pub updated_at_ms: u64,
}

impl MatchSnapshot {
    pub fn new(match_id: MatchId, invite_code: String, host: PlayerId, now_ms: u64) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            revision: 1,
            match_id,
            invite_code,
            invite_expires_at_ms: now_ms + INVITE_LIFETIME_MS,
            status: MatchStatus::Lobby,
            players: vec![PlayerMatchState {
                player_id: host,
                health: MAX_HEALTH,
                ready: false,
                eliminated: false,
                calibration_model_version: None,
                briefing_acknowledged: false,
            }],
            winner: None,
            created_at_ms: now_ms,
            started_at_ms: None,
            completed_at_ms: None,
            updated_at_ms: now_ms,
        }
    }

    pub fn add_player(&mut self, player_id: PlayerId, now_ms: u64) -> bool {
        if self.status != MatchStatus::Lobby
            || self.players.len() >= 2
            || self
                .players
                .iter()
                .any(|player| player.player_id == player_id)
        {
            return false;
        }
        self.players.push(PlayerMatchState {
            player_id,
            health: MAX_HEALTH,
            ready: false,
            eliminated: false,
            calibration_model_version: None,
            briefing_acknowledged: false,
        });
        self.bump(now_ms);
        true
    }

    /// Compatibility helper for the pre-onboarding bot.
    pub fn set_ready(&mut self, player_id: &str, ready: bool, now_ms: u64) -> bool {
        self.set_ready_with_calibration(player_id, ready, "legacy".into(), now_ms)
    }

    pub fn set_ready_with_calibration(
        &mut self,
        player_id: &str,
        ready: bool,
        calibration_model_version: String,
        now_ms: u64,
    ) -> bool {
        if self.status != MatchStatus::Lobby || calibration_model_version.trim().is_empty() {
            return false;
        }
        let Some(player) = self
            .players
            .iter_mut()
            .find(|player| player.player_id == player_id)
        else {
            return false;
        };
        player.ready = ready;
        player.calibration_model_version = ready.then_some(calibration_model_version);
        if self.players.len() == 2 && self.players.iter().all(|player| player.ready) {
            self.status = MatchStatus::Briefing;
        }
        self.bump(now_ms);
        true
    }

    pub fn acknowledge_briefing(&mut self, player_id: &str, now_ms: u64) -> bool {
        if self.status != MatchStatus::Briefing {
            return false;
        }
        let Some(player) = self
            .players
            .iter_mut()
            .find(|player| player.player_id == player_id)
        else {
            return false;
        };
        player.briefing_acknowledged = true;
        if self
            .players
            .iter()
            .all(|player| player.briefing_acknowledged)
        {
            self.status = MatchStatus::Active;
            self.started_at_ms = Some(now_ms);
        }
        self.bump(now_ms);
        true
    }

    pub fn apply_hit(&mut self, shooter: &str, target: &str, now_ms: u64) -> bool {
        if self.status != MatchStatus::Active
            || shooter == target
            || !self
                .players
                .iter()
                .any(|player| player.player_id == shooter)
        {
            return false;
        }
        let Some(target_state) = self
            .players
            .iter_mut()
            .find(|player| player.player_id == target)
        else {
            return false;
        };
        if target_state.eliminated || target_state.health == 0 {
            return false;
        }
        target_state.health -= 1;
        if target_state.health == 0 {
            target_state.eliminated = true;
            self.status = MatchStatus::Completed;
            self.winner = Some(shooter.to_string());
            self.completed_at_ms = Some(now_ms);
        }
        self.bump(now_ms);
        true
    }

    fn bump(&mut self, now_ms: u64) {
        self.revision += 1;
        self.updated_at_ms = now_ms;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum MatchEvent {
    Created {
        event_id: Uuid,
        match_id: MatchId,
        host_id: PlayerId,
        at_ms: u64,
    },
    Joined {
        event_id: Uuid,
        match_id: MatchId,
        player_id: PlayerId,
        at_ms: u64,
    },
    Ready {
        event_id: Uuid,
        match_id: MatchId,
        player_id: PlayerId,
        calibration_model_version: String,
        at_ms: u64,
    },
    BriefingAcknowledged {
        event_id: Uuid,
        match_id: MatchId,
        player_id: PlayerId,
        at_ms: u64,
    },
    Hit {
        event_id: Uuid,
        command_id: Uuid,
        match_id: MatchId,
        shooter_id: PlayerId,
        target_id: PlayerId,
        at_ms: u64,
    },
    Completed {
        event_id: Uuid,
        match_id: MatchId,
        winner_id: PlayerId,
        at_ms: u64,
    },
}

impl MatchEvent {
    pub fn match_id(&self) -> &str {
        match self {
            MatchEvent::Created { match_id, .. }
            | MatchEvent::Joined { match_id, .. }
            | MatchEvent::Ready { match_id, .. }
            | MatchEvent::BriefingAcknowledged { match_id, .. }
            | MatchEvent::Hit { match_id, .. }
            | MatchEvent::Completed { match_id, .. } => match_id,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FriendRequestStatus {
    Pending,
    Accepted,
    Declined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FriendRequest {
    pub request_id: String,
    pub from_player_id: PlayerId,
    pub to_player_id: PlayerId,
    pub status: FriendRequestStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum SocialEvent {
    Requested {
        event_id: Uuid,
        request: FriendRequest,
    },
    Accepted {
        event_id: Uuid,
        request_id: String,
        actor_id: PlayerId,
        at_ms: u64,
    },
    Declined {
        event_id: Uuid,
        request_id: String,
        actor_id: PlayerId,
        at_ms: u64,
    },
    Removed {
        event_id: Uuid,
        actor_id: PlayerId,
        friend_id: PlayerId,
        at_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Friend {
    pub account: Account,
    pub available: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchInvitationStatus {
    Pending,
    Accepted,
    Declined,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchInvitation {
    pub invitation_id: String,
    pub from_player_id: PlayerId,
    pub to_player_id: PlayerId,
    pub match_id: MatchId,
    pub status: MatchInvitationStatus,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchHistoryEntry {
    pub match_id: MatchId,
    pub result: String,
    pub opponent: MatchHistoryParticipant,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub my_hit_total: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchHistoryPage {
    pub matches: Vec<MatchHistoryEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchHistoryParticipant {
    pub player_id: PlayerId,
    pub handle: Option<String>,
    pub display_name: String,
    pub hit_total: u32,
    pub winner: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchHistoryEvent {
    pub event_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub player_id: Option<PlayerId>,
    pub timestamp_ms: u64,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MatchDetail {
    pub match_id: MatchId,
    pub result: String,
    pub participants: Vec<MatchHistoryParticipant>,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub events: Vec<MatchHistoryEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ClientMessage {
    Heartbeat {
        command_id: Uuid,
    },
    Presence {
        command_id: Uuid,
        presence: Presence,
    },
    Ready {
        command_id: Uuid,
        match_id: MatchId,
    },
    ReadyWithMetadata {
        command_id: Uuid,
        match_id: MatchId,
        calibration_model_version: String,
    },
    BriefingAcknowledged {
        command_id: Uuid,
        match_id: MatchId,
    },
    NearbyToken {
        command_id: Uuid,
        match_id: MatchId,
        peer_id: PlayerId,
        token: String,
    },
    Proximity {
        command_id: Uuid,
        match_id: MatchId,
        peer_id: PlayerId,
        distance_meters: Option<f32>,
        direction: Option<[f32; 3]>,
        sampled_at_ms: u64,
    },
    Shot {
        command_id: Uuid,
        match_id: MatchId,
        target_id: PlayerId,
        reticle: [f32; 2],
        mask_contains_reticle: bool,
        target_score: f32,
        fired_at_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ServerMessage {
    Hello {
        player_id: PlayerId,
        revision: u64,
    },
    MatchSnapshot {
        snapshot: MatchSnapshot,
    },
    SocialRevision {
        revision: u64,
    },
    InvitationRevision {
        revision: u64,
    },
    NearbyToken {
        player_id: PlayerId,
        token: String,
    },
    ShotResolution {
        command_id: Uuid,
        accepted: bool,
        reason: String,
        snapshot: Option<MatchSnapshot>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InspectorSnapshot {
    pub revision: u64,
    pub appearances: Vec<AppearanceProfile>,
    pub presences: Vec<Presence>,
    pub matches: Vec<MatchSnapshot>,
    pub processed_commands: usize,
    pub search_stats: BTreeMap<String, usize>,
    #[serde(default)]
    pub materialization_counts: BTreeMap<String, usize>,
    /// Live size of each Bog search index (ANNy HNSWs and the BM25 doc count),
    /// so the inspector can show the vector database materializing in real time.
    #[serde(default)]
    pub index_sizes: BTreeMap<String, usize>,
    /// Wall-clock latency of the most recent query of each kind, in microseconds.
    #[serde(default)]
    pub search_latency_micros: BTreeMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_appearance(skin: Option<&str>) -> AppearanceProfile {
        AppearanceProfile {
            player_id: "p1".into(),
            display_name: "Player One".into(),
            generated_description: "red jacket, dark jeans".into(),
            embedding_model: "test-v1".into(),
            descriptor_model: "test-v1".into(),
            whole_body_embedding: vec![0.0; VISUAL_DIMENSIONS],
            face_embeddings: Vec::new(),
            upper_body_embeddings: Vec::new(),
            lower_body_embeddings: Vec::new(),
            head_accessory_embeddings: Vec::new(),
            silhouette_descriptor: vec![0.0; 64],
            briefing_thumbnail: None,
            skin: skin.map(Into::into),
            updated_at_ms: 1_700_000_000_000,
        }
    }

    /// The server stores the skin verbatim, so clients can ship a new cosmetic
    /// without a server release — but only if an absent field still decodes and
    /// an unknown value still round-trips.
    #[test]
    fn appearance_skin_is_optional_and_round_trips() {
        let without: AppearanceProfile =
            serde_json::from_str(&serde_json::to_string(&sample_appearance(None)).unwrap())
                .unwrap();
        assert_eq!(without.skin, None);
        assert!(without.validate().is_ok());

        let legacy_json = serde_json::to_value(sample_appearance(None)).unwrap();
        let mut legacy = legacy_json.as_object().unwrap().clone();
        legacy.remove("skin");
        let decoded: AppearanceProfile =
            serde_json::from_value(serde_json::Value::Object(legacy)).unwrap();
        assert_eq!(decoded.skin, None);

        let with = sample_appearance(Some("green_camo"));
        let encoded = serde_json::to_string(&with).unwrap();
        assert!(encoded.contains("\"skin\":\"green_camo\""));
        let decoded: AppearanceProfile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, with);

        let unknown: AppearanceProfile = serde_json::from_str(
            &serde_json::to_string(&sample_appearance(Some("future_skin"))).unwrap(),
        )
        .unwrap();
        assert_eq!(unknown.skin.as_deref(), Some("future_skin"));
    }

    #[test]
    fn redacted_appearance_keeps_the_skin() {
        let redacted = sample_appearance(Some("pink_camo")).redacted_for_global_view();
        assert_eq!(redacted.skin.as_deref(), Some("pink_camo"));
        assert_eq!(redacted.briefing_thumbnail, None);
    }

    #[test]
    fn haversine_rejects_points_outside_two_miles() {
        let near = haversine_meters(40.7128, -74.0060, 40.72, -74.0060);
        let far = haversine_meters(40.7128, -74.0060, 40.75, -74.0060);
        assert!(near < MATCH_RADIUS_METERS);
        assert!(far > MATCH_RADIUS_METERS);
    }

    #[test]
    fn availability_can_be_published_without_entering_nearby_search() {
        let presence = Presence {
            player_id: "player".into(),
            latitude: 0.0,
            longitude: 0.0,
            horizontal_accuracy: -1.0,
            foreground: true,
            updated_at_ms: 1_000,
        };
        assert!(presence.is_available_at(1_001));
        assert!(!presence.has_usable_location());
    }

    #[test]
    fn match_requires_briefing_before_three_hits() {
        let mut snapshot = MatchSnapshot::new("m1".into(), "CODE".into(), "a".into(), 1);
        assert!(snapshot.add_player("b".into(), 2));
        assert!(snapshot.set_ready_with_calibration("a", true, "v1".into(), 3));
        assert!(snapshot.set_ready_with_calibration("b", true, "v1".into(), 4));
        assert_eq!(snapshot.status, MatchStatus::Briefing);
        assert!(!snapshot.apply_hit("a", "b", 5));
        assert!(snapshot.acknowledge_briefing("a", 5));
        assert!(snapshot.acknowledge_briefing("b", 6));
        assert_eq!(snapshot.status, MatchStatus::Active);
        assert!(snapshot.apply_hit("a", "b", 7));
        assert!(snapshot.apply_hit("a", "b", 8));
        assert!(snapshot.apply_hit("a", "b", 9));
        assert_eq!(snapshot.status, MatchStatus::Completed);
        assert_eq!(snapshot.winner.as_deref(), Some("a"));
        assert_eq!(snapshot.completed_at_ms, Some(9));
    }

    #[test]
    fn empty_calibration_cannot_make_player_ready() {
        let mut snapshot = MatchSnapshot::new("m1".into(), "CODE".into(), "a".into(), 1);
        assert!(!snapshot.set_ready_with_calibration("a", true, " ".into(), 2));
    }

    #[test]
    fn stale_or_background_presence_is_unavailable() {
        let mut presence = Presence {
            player_id: "p".into(),
            latitude: 0.0,
            longitude: 0.0,
            horizontal_accuracy: 1.0,
            foreground: true,
            updated_at_ms: 1_000,
        };
        assert!(presence.is_available_at(1_000 + PRESENCE_FRESHNESS_MS));
        assert!(!presence.is_available_at(1_001 + PRESENCE_FRESHNESS_MS));
        presence.foreground = false;
        assert!(!presence.is_available_at(1_000));
    }

    #[test]
    fn normalization_is_finite_and_unit_length() {
        let values = vec![3.0, 4.0];
        let normalized = normalize_vector::<2>(&values);
        assert!((normalized[0] - 0.6).abs() < 0.0001);
        assert!((normalized[1] - 0.8).abs() < 0.0001);
    }

    #[test]
    fn global_view_removes_match_scoped_appearance() {
        let profile = AppearanceProfile {
            player_id: "p".into(),
            display_name: "Player".into(),
            generated_description: "blue top".into(),
            embedding_model: "test".into(),
            descriptor_model: "test".into(),
            whole_body_embedding: vec![0.0; VISUAL_DIMENSIONS],
            face_embeddings: vec![vec![1.0]],
            upper_body_embeddings: vec![vec![1.0]],
            lower_body_embeddings: vec![vec![1.0]],
            head_accessory_embeddings: vec![vec![1.0]],
            silhouette_descriptor: vec![1.0],
            briefing_thumbnail: Some("image".into()),
            skin: Some("red_tartan".into()),
            updated_at_ms: 1,
        };
        let redacted = profile.redacted_for_global_view();
        assert!(redacted.face_embeddings.is_empty());
        assert!(redacted.upper_body_embeddings.is_empty());
        assert!(redacted.briefing_thumbnail.is_none());
        assert_eq!(redacted.whole_body_embedding.len(), VISUAL_DIMENSIONS);
    }
}
