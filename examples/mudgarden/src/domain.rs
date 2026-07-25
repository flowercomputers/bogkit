use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub type WorldTime = u64;
pub const GARDEN_FILES: u8 = 8;
pub const GARDEN_RANKS: u8 = 8;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(pub u64);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(ActorId);
id_type!(GardenId);
id_type!(RoomId);
id_type!(PlantId);
id_type!(EventId);
id_type!(ItemId);
id_type!(BogCellId);
id_type!(BogOrganismId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActorKind {
    Human,
    GardenerAgent,
    Helper,
    Spirit,
    God,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    TendOwnGarden,
    TendSharedGarden,
    EnterPrivateGarden,
    ChangeWeather,
    HelpGardeners,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorState {
    pub id: ActorId,
    pub name: String,
    pub kind: ActorKind,
    pub auth_fingerprint: Option<String>,
    pub home_garden_id: GardenId,
    pub current_room_id: RoomId,
    pub capabilities: Vec<Capability>,
    pub inventory: Vec<InventoryItem>,
    pub agent: Option<AgentProfile>,
    pub last_seen_event_id: EventId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub strategy: AgentStrategy,
    pub goal: String,
    pub next_wake_at: WorldTime,
    pub action_budget: u16,
    pub enabled: bool,
    #[serde(skip)]
    pub npc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStrategy {
    Gardener,
    Helper,
    Spirit,
    WeatherGod,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTurn {
    pub actor_id: ActorId,
    pub npc_id: String,
    pub name: String,
    pub kind: ActorKind,
    pub strategy: AgentStrategy,
    pub goal: String,
    pub world_time: WorldTime,
    pub season: Season,
    pub weather: Weather,
    pub room: RoomState,
    pub visible_plants: Vec<PlantState>,
    pub visible_people: Vec<String>,
    pub inventory: Vec<InventoryItem>,
    pub capabilities: Vec<Capability>,
    pub recent_events: Vec<String>,
    pub recent_speech: Vec<String>,
    pub triggering_speech: Vec<String>,
    pub triggering_knocks: Vec<String>,
    pub available_commands: Vec<String>,
    pub ecology: Option<AgentEcologyContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentEcologyContext {
    pub edge_length: u16,
    pub moisture_p10: u64,
    pub moisture_p50: u64,
    pub moisture_p90: u64,
    pub stressed_organisms: usize,
    pub restoration_candidates: Vec<BogCellState>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActionStatus {
    Completed,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActionStepKind {
    ModelRequest,
    WorldQuery,
    Action,
    Execution,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentActionStep {
    pub kind: AgentActionStepKind,
    pub label: String,
    pub rationale: Option<String>,
    pub command: Option<String>,
    pub result: Option<String>,
    pub response_id: Option<String>,
    pub input: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentActionTrace {
    pub id: u64,
    pub actor_id: ActorId,
    pub actor_name: String,
    pub model: String,
    pub started_at_unix_ms: u128,
    pub completed_at_unix_ms: Option<u128>,
    pub status: AgentActionStatus,
    pub instructions: String,
    pub context: AgentTurn,
    pub steps: Vec<AgentActionStep>,
    pub final_command: Option<String>,
    pub final_intention: Option<String>,
    pub response_id: Option<String>,
    pub execution_output: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    Seed,
    Produce,
    Decoration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryItem {
    pub id: ItemId,
    pub kind: ItemKind,
    pub species: String,
}

impl InventoryItem {
    pub fn display_name(&self) -> String {
        let suffix = match self.kind {
            ItemKind::Seed => "seed",
            ItemKind::Produce => "fruit",
            ItemKind::Decoration => return self.species.clone(),
        };
        format!("{} {}", self.species, suffix)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GardenKind {
    Home,
    Common,
    Spirit,
    Divine,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GardenState {
    pub id: GardenId,
    pub owner_actor_id: ActorId,
    pub name: String,
    pub room_id: RoomId,
    pub kind: GardenKind,
    pub allowed_tenders: Vec<ActorId>,
    pub allowed_harvesters: Vec<ActorId>,
    #[serde(default)]
    pub decorations: Vec<DecorationState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecorationState {
    pub id: ItemId,
    pub name: String,
    pub description: String,
    pub symbol: char,
    pub position: GardenPosition,
    pub placed_by_actor_id: ActorId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GardenAccessState {
    pub garden_id: GardenId,
    pub unlocked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GardenAdmissionState {
    pub garden_id: GardenId,
    pub actor_id: ActorId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GardenPosition {
    file: u8,
    rank: u8,
}

impl GardenPosition {
    pub fn new(file: u8, rank: u8) -> Option<Self> {
        (file < GARDEN_FILES && rank < GARDEN_RANKS).then_some(Self { file, rank })
    }

    pub fn file(self) -> u8 {
        self.file
    }

    pub fn rank(self) -> u8 {
        self.rank
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (0..GARDEN_RANKS).flat_map(|rank| (0..GARDEN_FILES).map(move |file| Self { file, rank }))
    }
}

impl fmt::Display for GardenPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", char::from(b'A' + self.file), self.rank + 1)
    }
}

impl Serialize for GardenPosition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for GardenPosition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl FromStr for GardenPosition {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim().as_bytes();
        if value.len() != 2 {
            return Err("Garden coordinates run from A1 to H8.".to_string());
        }
        let file = value[0].to_ascii_uppercase();
        let rank = value[1];
        if !(b'A'..=b'H').contains(&file) || !(b'1'..=b'8').contains(&rank) {
            return Err("Garden coordinates run from A1 to H8.".to_string());
        }
        Ok(Self {
            file: file - b'A',
            rank: rank - b'1',
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomKind {
    Gate,
    CommonPath,
    Glasshouse,
    MoonBed,
    Pond,
    Compost,
    WildEdge,
    HomeGarden,
    GardenGate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomState {
    pub id: RoomId,
    pub name: String,
    pub description: String,
    pub kind: RoomKind,
    pub garden_id: Option<GardenId>,
    pub exits: BTreeMap<String, RoomId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlantStage {
    Seed,
    Sprout,
    Growing,
    Flowering,
    Fruiting,
    Dormant,
}

impl fmt::Display for PlantStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            PlantStage::Seed => "seed",
            PlantStage::Sprout => "sprout",
            PlantStage::Growing => "growing",
            PlantStage::Flowering => "flowering",
            PlantStage::Fruiting => "fruiting",
            PlantStage::Dormant => "dormant",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlantState {
    pub id: PlantId,
    pub name: String,
    pub species: String,
    pub position: GardenPosition,
    pub owner_actor_id: ActorId,
    pub garden_id: GardenId,
    pub room_id: RoomId,
    pub moisture: i16,
    pub nutrients: i16,
    pub health: i16,
    pub growth: i16,
    pub stage: PlantStage,
    pub planted_at: WorldTime,
    pub next_transition_at: WorldTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

impl fmt::Display for Season {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Season::Spring => "spring",
            Season::Summer => "summer",
            Season::Autumn => "autumn",
            Season::Winter => "winter",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Weather {
    Clear,
    Cloudy,
    LightRain,
    HeavyRain,
    Mist,
}

impl fmt::Display for Weather {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Weather::Clear => "clear",
            Weather::Cloudy => "cloudy",
            Weather::LightRain => "light rain",
            Weather::HeavyRain => "heavy rain",
            Weather::Mist => "mist",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldClock {
    pub now: WorldTime,
    pub season: Season,
    pub weather: Weather,
    pub temperature_c: i16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BogMeta {
    pub ecology_version: u16,
    pub edge_length: u16,
    pub next_organism_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BogCellState {
    pub id: BogCellId,
    pub x: u16,
    pub y: u16,
    /// Millimetres relative to the peat surface. Negative values are below it.
    pub water_table_mm: i16,
    pub moisture: u8,
    /// pH multiplied by 100 so the persisted model stays deterministic.
    pub ph_cent: u16,
    pub nutrients: u8,
    pub temperature_c: i16,
    pub light: u8,
    pub peat_depth_mm: u16,
    pub shrub_cover: u8,
    pub next_transition_at: WorldTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BogLifeStage {
    Establishing,
    Growing,
    Flowering,
    Fruiting,
    Dormant,
    Dead,
}

impl fmt::Display for BogLifeStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            BogLifeStage::Establishing => "establishing",
            BogLifeStage::Growing => "growing",
            BogLifeStage::Flowering => "flowering",
            BogLifeStage::Fruiting => "fruiting",
            BogLifeStage::Dormant => "dormant",
            BogLifeStage::Dead => "dead",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BogOrganismState {
    pub id: BogOrganismId,
    pub species: String,
    pub cell_id: BogCellId,
    pub health: i16,
    pub biomass_g: u32,
    pub age_days: u32,
    pub stage: BogLifeStage,
    pub next_transition_at: WorldTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BogSpeciesStats {
    pub count: i64,
    pub health_total: i64,
    pub biomass_total_g: i64,
    pub flowering: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    Arrival,
    Departure,
    Speech,
    Planting,
    Watering,
    Pruning,
    Harvesting,
    Trading,
    Decorating,
    Growth,
    Flowering,
    Weather,
    Permission,
    AgentAction,
    System,
    Knocking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEvent {
    pub id: EventId,
    pub at: WorldTime,
    pub kind: EventKind,
    pub actor_id: Option<ActorId>,
    pub room_id: Option<RoomId>,
    pub plant_id: Option<PlantId>,
    pub recipients: Vec<ActorId>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldMeta {
    pub next_actor_id: u64,
    pub next_garden_id: u64,
    pub next_room_id: u64,
    pub next_plant_id: u64,
    pub next_item_id: u64,
    pub next_event_id: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugScheduleEntry {
    pub entity: &'static str,
    pub id: u64,
    pub label: String,
    pub at: WorldTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugMaterializedViews {
    pub needs_water_plant_ids: Vec<PlantId>,
    pub plant_schedule: Vec<DebugScheduleEntry>,
    pub agent_schedule: Vec<DebugScheduleEntry>,
    pub cell_schedule: Vec<DebugScheduleEntry>,
    pub organism_schedule: Vec<DebugScheduleEntry>,
    pub stressed_organism_ids: Vec<BogOrganismId>,
    pub moisture_p10: Option<u64>,
    pub moisture_p50: Option<u64>,
    pub moisture_p90: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugWorldCell {
    #[serde(flatten)]
    pub cell: BogCellState,
    pub room_id: RoomId,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugRoomRegion {
    pub room_id: RoomId,
    pub center_x: u16,
    pub center_y: u16,
    pub cell_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugWorldGrid {
    pub ecology_version: u16,
    pub edge_length: u16,
    pub next_organism_id: u64,
    pub regions: Vec<DebugRoomRegion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugSnapshot {
    pub captured_at_unix_ms: u128,
    pub clock: WorldClock,
    pub meta: WorldMeta,
    pub actors: Vec<ActorState>,
    pub gardens: Vec<GardenState>,
    pub rooms: Vec<RoomState>,
    pub plants: Vec<PlantState>,
    pub world_grid: Option<DebugWorldGrid>,
    pub world_cells: Vec<DebugWorldCell>,
    pub organisms: Vec<BogOrganismState>,
    pub species: Vec<(String, BogSpeciesStats)>,
    pub events: Vec<WorldEvent>,
    pub agent_actions: Vec<AgentActionTrace>,
    pub views: DebugMaterializedViews,
}

impl Default for WorldMeta {
    fn default() -> Self {
        Self {
            next_actor_id: 100,
            next_garden_id: 100,
            next_room_id: 100,
            next_plant_id: 1,
            next_item_id: 1,
            next_event_id: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityKey {
    Meta,
    Clock,
    Actor(ActorId),
    Garden(GardenId),
    Room(RoomId),
    Plant(PlantId),
    Event(EventId),
    BogMeta,
    BogCell(BogCellId),
    BogOrganism(BogOrganismId),
    GardenAccess(GardenId),
    GardenAdmission(GardenId, ActorId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorldRecord {
    Meta(WorldMeta),
    Clock(WorldClock),
    Actor(ActorState),
    Garden(GardenState),
    Room(RoomState),
    Plant(PlantState),
    Event(WorldEvent),
    BogMeta(BogMeta),
    BogCell(BogCellState),
    BogOrganism(BogOrganismState),
    GardenAccess(GardenAccessState),
    GardenAdmission(GardenAdmissionState),
}

#[derive(Debug, Clone)]
pub struct WorldOutput {
    pub lines: Vec<String>,
    pub events: Vec<WorldEvent>,
    pub quit: bool,
}

impl WorldOutput {
    pub fn lines(lines: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            lines: lines.into_iter().map(Into::into).collect(),
            events: Vec::new(),
            quit: false,
        }
    }

    pub fn quit(message: impl Into<String>) -> Self {
        Self {
            lines: vec![message.into()],
            events: Vec::new(),
            quit: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct LegacyAgentProfile {
        strategy: AgentStrategy,
        goal: String,
        next_wake_at: WorldTime,
        action_budget: u16,
        enabled: bool,
    }

    #[test]
    fn agent_profile_decodes_records_from_before_npc_ids() {
        let encoded = postcard::to_stdvec(&LegacyAgentProfile {
            strategy: AgentStrategy::Spirit,
            goal: "encourage old green things".to_string(),
            next_wake_at: 42,
            action_budget: 2,
            enabled: true,
        })
        .unwrap();

        let profile: AgentProfile = postcard::from_bytes(&encoded).unwrap();

        assert_eq!(profile.strategy, AgentStrategy::Spirit);
        assert_eq!(profile.goal, "encourage old green things");
        assert_eq!(profile.next_wake_at, 42);
        assert_eq!(profile.action_budget, 2);
        assert!(profile.enabled);
        assert_eq!(profile.npc_id, "");
    }
}
