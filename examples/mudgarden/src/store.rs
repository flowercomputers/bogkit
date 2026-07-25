use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fold::pipeline::{Aggregate, Filter, FilterMap, Keyed, ScoreBy, Scored, terminal};
use fold::stream::KeyedStream;
use thiserror::Error;

use crate::commands::Command;
use crate::content::GameContent;
use crate::domain::*;
use crate::ecology::{self, BogConfig};

const GATE: RoomId = RoomId(1);
const COMMON_PATH: RoomId = RoomId(2);
const GLASSHOUSE: RoomId = RoomId(3);
const MOON_BED: RoomId = RoomId(4);
const POND: RoomId = RoomId(5);
const COMPOST: RoomId = RoomId(6);
const WILD_EDGE: RoomId = RoomId(7);
const GLASSHOUSE_GARDEN: GardenId = GardenId(1);
const MOON_BED_GARDEN: GardenId = GardenId(2);
const POND_GARDEN: GardenId = GardenId(3);
const COMPOST_GARDEN: GardenId = GardenId(4);
const WILD_EDGE_GARDEN: GardenId = GardenId(5);

type RootRecord = Keyed<EntityKey, WorldRecord>;
type ActorRow = Keyed<ActorId, ActorState>;
type GardenRow = Keyed<GardenId, GardenState>;
type RoomRow = Keyed<RoomId, RoomState>;
type PlantRow = Keyed<PlantId, PlantState>;
type BogCellRow = Keyed<BogCellId, BogCellState>;
type BogOrganismRow = Keyed<BogOrganismId, BogOrganismState>;
type BogOrganismByCellRow = Keyed<BogCellId, BogOrganismState>;
type BogSpeciesRow = Keyed<String, BogOrganismState>;

type ActorBranch = FilterMap<
    fn(&RootRecord) -> Option<ActorRow>,
    terminal::Table<ActorId, ActorState>,
    RootRecord,
    ActorRow,
>;
type GardenBranch = FilterMap<
    fn(&RootRecord) -> Option<GardenRow>,
    terminal::Table<GardenId, GardenState>,
    RootRecord,
    GardenRow,
>;
type RoomBranch = FilterMap<
    fn(&RootRecord) -> Option<RoomRow>,
    terminal::Table<RoomId, RoomState>,
    RootRecord,
    RoomRow,
>;
type PlantBranch = FilterMap<
    fn(&RootRecord) -> Option<PlantRow>,
    terminal::Table<PlantId, PlantState>,
    RootRecord,
    PlantRow,
>;
type EventBranch = FilterMap<
    fn(&RootRecord) -> Option<WorldEvent>,
    ScoreBy<fn(&WorldEvent) -> u64, terminal::Ranked<u64, WorldEvent>, u64, WorldEvent>,
    RootRecord,
    WorldEvent,
>;
type ScheduleBranch = FilterMap<
    fn(&RootRecord) -> Option<PlantState>,
    ScoreBy<fn(&PlantState) -> u64, terminal::Ranked<u64, PlantState>, u64, PlantState>,
    RootRecord,
    PlantState,
>;
type NeedsWaterBranch = FilterMap<
    fn(&RootRecord) -> Option<PlantRow>,
    Filter<PlantRow, fn(&PlantRow) -> bool, terminal::Table<PlantId, PlantState>>,
    RootRecord,
    PlantRow,
>;
type AgentScheduleBranch = FilterMap<
    fn(&RootRecord) -> Option<ActorState>,
    ScoreBy<fn(&ActorState) -> u64, terminal::Ranked<u64, ActorState>, u64, ActorState>,
    RootRecord,
    ActorState,
>;

type BogCellBranch = FilterMap<
    fn(&RootRecord) -> Option<BogCellRow>,
    terminal::Table<BogCellId, BogCellState>,
    RootRecord,
    BogCellRow,
>;
type BogOrganismBranch = FilterMap<
    fn(&RootRecord) -> Option<BogOrganismRow>,
    terminal::Table<BogOrganismId, BogOrganismState>,
    RootRecord,
    BogOrganismRow,
>;
type BogCellScheduleBranch = FilterMap<
    fn(&RootRecord) -> Option<BogCellState>,
    ScoreBy<fn(&BogCellState) -> u64, terminal::Ranked<u64, BogCellState>, u64, BogCellState>,
    RootRecord,
    BogCellState,
>;
type BogOrganismScheduleBranch = FilterMap<
    fn(&RootRecord) -> Option<BogOrganismState>,
    ScoreBy<
        fn(&BogOrganismState) -> u64,
        terminal::Ranked<u64, BogOrganismState>,
        u64,
        BogOrganismState,
    >,
    RootRecord,
    BogOrganismState,
>;
type StressedBogOrganismBranch = FilterMap<
    fn(&RootRecord) -> Option<BogOrganismRow>,
    Filter<
        BogOrganismRow,
        fn(&BogOrganismRow) -> bool,
        terminal::Table<BogOrganismId, BogOrganismState>,
    >,
    RootRecord,
    BogOrganismRow,
>;
type BogOrganismsByCellBranch = FilterMap<
    fn(&RootRecord) -> Option<BogOrganismByCellRow>,
    terminal::Multimap<BogCellId, BogOrganismState>,
    RootRecord,
    BogOrganismByCellRow,
>;
type BogSpeciesBranch = FilterMap<
    fn(&RootRecord) -> Option<BogSpeciesRow>,
    Aggregate<
        String,
        BogOrganismState,
        BogSpeciesStats,
        fn(&mut BogSpeciesStats, &BogOrganismState, isize),
        terminal::Table<String, BogSpeciesStats>,
    >,
    RootRecord,
    BogSpeciesRow,
>;
type BogMoistureBranch = FilterMap<
    fn(&RootRecord) -> Option<Scored<u64, ()>>,
    terminal::Histogram<u64, (), u64, fn(&u64) -> u64>,
    RootRecord,
    Scored<u64, ()>,
>;

type BogPipeline = (
    BogCellBranch,
    BogOrganismBranch,
    BogCellScheduleBranch,
    BogOrganismScheduleBranch,
    StressedBogOrganismBranch,
    BogOrganismsByCellBranch,
    BogSpeciesBranch,
    BogMoistureBranch,
);

type WorldPipeline = (
    ActorBranch,
    GardenBranch,
    RoomBranch,
    PlantBranch,
    EventBranch,
    ScheduleBranch,
    NeedsWaterBranch,
    AgentScheduleBranch,
    BogPipeline,
);

type WorldStream = KeyedStream<EntityKey, WorldRecord, WorldPipeline>;

#[derive(Debug, Error)]
pub enum WorldError {
    #[error("{0}")]
    Message(String),
}

impl From<String> for WorldError {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

pub struct World {
    stream: WorldStream,
    content: Arc<GameContent>,
    bog_config: BogConfig,
}

impl World {
    pub fn open(path: impl AsRef<Path>) -> Self {
        Self::open_with_content(path, GameContent::bundled())
    }

    pub fn open_with_content(path: impl AsRef<Path>, content: Arc<GameContent>) -> Self {
        let bog_config = BogConfig::from_env();
        let mut world = Self {
            stream: KeyedStream::new(path, pipeline()),
            content,
            bog_config,
        };
        world.bootstrap();
        world.ensure_garden_gates();
        world.bootstrap_bog();
        world
    }

    pub fn ensure_human(
        &mut self,
        requested_name: &str,
        auth_fingerprint: Option<&str>,
    ) -> Result<ActorState, WorldError> {
        let name = normalize_name(&self.content, requested_name)?;
        if let Some(mut actor) = self
            .actors()
            .into_iter()
            .find(|actor| actor.name.eq_ignore_ascii_case(&name))
        {
            if actor.kind != ActorKind::Human {
                return Err(WorldError::Message(
                    self.content
                        .render("error.name_taken", &[("name", name.clone())]),
                ));
            }
            match (actor.auth_fingerprint.as_deref(), auth_fingerprint) {
                (Some(stored), Some(offered)) if stored != offered => {
                    return Err(WorldError::Message(
                        self.content
                            .render("error.auth_key_mismatch", &[("name", actor.name)]),
                    ));
                }
                (None, Some(offered)) => {
                    actor.auth_fingerprint = Some(offered.to_string());
                    self.stream.wtx(|tx| {
                        tx.upsert(
                            &EntityKey::Actor(actor.id),
                            &WorldRecord::Actor(actor.clone()),
                        );
                    });
                }
                _ => {}
            }
            return Ok(actor);
        }

        self.create_actor(
            &name,
            ActorKind::Human,
            auth_fingerprint.map(ToOwned::to_owned),
        )
    }

    pub fn ensure_world_agents(&mut self) -> Result<Vec<ActorState>, WorldError> {
        let manifests = self.content.npcs.clone();
        let mut result = Vec::new();
        for (npc_id, manifest) in manifests {
            let mut actor = match self.actor_by_name(&manifest.name) {
                Some(actor) => actor,
                None => self.create_actor(&manifest.name, manifest.kind.clone(), None)?,
            };
            let previous_profile = actor.agent.take();
            actor.kind = manifest.kind.clone();
            actor.capabilities = capabilities_for(&manifest.kind);
            actor.agent = Some(AgentProfile {
                npc_id: npc_id.clone(),
                strategy: manifest.strategy.clone(),
                goal: manifest.goal.clone(),
                next_wake_at: previous_profile
                    .as_ref()
                    .map_or_else(|| self.clock().now + 1, |profile| profile.next_wake_at),
                action_budget: manifest.action_budget,
                enabled: previous_profile
                    .as_ref()
                    .is_none_or(|profile| profile.enabled),
            });
            self.stream.wtx(|tx| {
                tx.upsert(
                    &EntityKey::Actor(actor.id),
                    &WorldRecord::Actor(actor.clone()),
                );
            });
            self.seed_agent_garden_if_empty(&actor, &npc_id)?;
            result.push(actor);
        }
        let merchant = self.ensure_merchant()?;
        if let Some(agent) = result.iter_mut().find(|actor| actor.id == merchant.id) {
            *agent = merchant;
        }
        Ok(result)
    }

    fn seed_agent_garden_if_empty(
        &mut self,
        actor: &ActorState,
        npc_id: &str,
    ) -> Result<(), WorldError> {
        let mut garden = self.garden(actor.home_garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.home_missing").to_string())
        })?;
        if !self.plants_in_room(garden.room_id).is_empty() || !garden.decorations.is_empty() {
            return Ok(());
        }

        let species = self.content.world.species.clone();
        if species.is_empty() {
            return Ok(());
        }
        let catalog = self.content.merchant.catalog.clone();
        let seed = npc_id
            .bytes()
            .fold(0_usize, |sum, byte| sum.wrapping_add(usize::from(byte)));
        let signature_species = match npc_id {
            "ivo" => "blue cornflower",
            "wren" => "sweet pea",
            "sorrel" => "scarlet runner bean",
            "mosswife" => "hairy vetch",
            "almanac" => "white water lily",
            _ => "",
        };
        let signature_index = species
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(signature_species))
            .unwrap_or(seed % species.len());
        let clock = self.clock();
        let mut meta = self.meta();
        let mut plants = Vec::with_capacity(64);
        let mut decorations = Vec::with_capacity(8);

        for position in GardenPosition::all() {
            let file = usize::from(position.file());
            let rank = usize::from(position.rank());
            let plot = rank * usize::from(GARDEN_FILES) + file;
            let decoration_plot = !catalog.is_empty() && (file + rank * 2 + seed).is_multiple_of(8);
            if decoration_plot {
                let definition = &catalog[(rank + seed) % catalog.len()];
                let item = allocate_item(&mut meta, ItemKind::Decoration, &definition.name);
                decorations.push(DecorationState {
                    id: item.id,
                    name: definition.name.clone(),
                    description: definition.description.clone(),
                    symbol: definition.symbol,
                    position,
                    placed_by_actor_id: actor.id,
                });
                continue;
            }

            let species_index = if (file + rank + seed).is_multiple_of(3) {
                (signature_index + 1 + plot) % species.len()
            } else {
                signature_index
            };
            let growth = match (file * 3 + rank + seed) % 5 {
                0 => 94,
                1 => 78,
                _ => 58,
            };
            let stage = stage_for(growth, 90);
            let id = PlantId(meta.next_plant_id);
            meta.next_plant_id += 1;
            plants.push(PlantState {
                id,
                name: species[species_index].clone(),
                species: species[species_index].clone(),
                position,
                owner_actor_id: actor.id,
                garden_id: garden.id,
                room_id: garden.room_id,
                moisture: 62 + ((file * 7 + rank * 3 + seed) % 25) as i16,
                nutrients: 68 + ((file * 5 + rank + seed) % 22) as i16,
                health: 86 + ((file + rank * 2 + seed) % 15) as i16,
                growth,
                stage,
                planted_at: clock.now.saturating_sub(8),
                next_transition_at: clock.now + 1 + (plot % 8) as u64,
            });
        }
        garden.decorations = decorations;

        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::Garden(garden.id), &WorldRecord::Garden(garden));
            for plant in plants {
                tx.upsert(&EntityKey::Plant(plant.id), &WorldRecord::Plant(plant));
            }
        });
        Ok(())
    }

    fn ensure_merchant(&mut self) -> Result<ActorState, WorldError> {
        let definition = self.content.merchant.clone();
        let room_id = self
            .rooms()
            .into_iter()
            .find(|room| room.kind == definition.room)
            .map(|room| room.id)
            .ok_or_else(|| {
                WorldError::Message(format!(
                    "The merchant's configured room {:?} does not exist.",
                    definition.room
                ))
            })?;
        let mut merchant = if let Some(actor) = self.actor_by_name(&definition.name) {
            if actor.kind == ActorKind::Human {
                return Err(WorldError::Message(
                    self.content
                        .render("error.name_taken", &[("name", definition.name.clone())]),
                ));
            }
            actor
        } else {
            self.create_actor(&definition.name, ActorKind::Helper, None)?
        };
        merchant.kind = ActorKind::Helper;
        merchant.capabilities = Vec::new();
        merchant.current_room_id = room_id;
        merchant.inventory.clear();
        self.stream.wtx(|tx| {
            tx.upsert(
                &EntityKey::Actor(merchant.id),
                &WorldRecord::Actor(merchant.clone()),
            );
        });
        Ok(merchant)
    }

    pub fn create_actor(
        &mut self,
        requested_name: &str,
        kind: ActorKind,
        auth_fingerprint: Option<String>,
    ) -> Result<ActorState, WorldError> {
        let name = normalize_name(&self.content, requested_name)?;
        if self
            .actors()
            .iter()
            .any(|actor| actor.name.eq_ignore_ascii_case(&name))
        {
            return Err(WorldError::Message(
                self.content
                    .render("error.name_taken", &[("name", name.clone())]),
            ));
        }

        let mut meta = self.meta();
        let actor_id = ActorId(meta.next_actor_id);
        let garden_id = GardenId(meta.next_garden_id);
        let room_id = RoomId(meta.next_room_id);
        let gate_room_id = RoomId(meta.next_room_id + 1);
        meta.next_actor_id += 1;
        meta.next_garden_id += 1;
        meta.next_room_id += 2;

        let capabilities = capabilities_for(&kind);
        let mut inventory = self
            .content
            .world
            .starter_seeds
            .iter()
            .map(|species| allocate_item(&mut meta, ItemKind::Seed, species))
            .collect::<Vec<_>>();
        inventory.extend(
            self.content
                .world
                .starter_fruit
                .iter()
                .map(|species| allocate_item(&mut meta, ItemKind::Produce, species)),
        );

        let actor = ActorState {
            id: actor_id,
            name: name.clone(),
            kind: kind.clone(),
            auth_fingerprint,
            home_garden_id: garden_id,
            current_room_id: room_id,
            capabilities,
            inventory,
            agent: None,
            last_seen_event_id: EventId(meta.next_event_id.saturating_sub(1)),
        };
        let garden = GardenState {
            id: garden_id,
            owner_actor_id: actor_id,
            name: self
                .content
                .render("world.home_garden_name", &[("name", name.clone())]),
            room_id,
            kind: match kind {
                ActorKind::Spirit => GardenKind::Spirit,
                ActorKind::God => GardenKind::Divine,
                _ => GardenKind::Home,
            },
            allowed_tenders: Vec::new(),
            allowed_harvesters: Vec::new(),
            decorations: Vec::new(),
        };
        let room = RoomState {
            id: room_id,
            name: garden.name.clone(),
            description: home_description(&self.content, &kind),
            kind: RoomKind::HomeGarden,
            garden_id: Some(garden_id),
            exits: BTreeMap::from([("out".to_string(), gate_room_id)]),
        };
        let gate_room = garden_gate_room(&self.content, gate_room_id, room_id, &name);

        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Arrival,
            Some(actor_id),
            Some(room_id),
            None,
            vec![actor_id],
            self.content
                .render("event.first_arrival", &[("name", name.clone())]),
        );

        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor_id),
                &WorldRecord::Actor(actor.clone()),
            );
            tx.upsert(&EntityKey::Garden(garden_id), &WorldRecord::Garden(garden));
            tx.upsert(&EntityKey::Room(room_id), &WorldRecord::Room(room));
            tx.upsert(
                &EntityKey::Room(gate_room_id),
                &WorldRecord::Room(gate_room),
            );
            tx.upsert(&EntityKey::Event(event.id), &WorldRecord::Event(event));
        });

        Ok(actor)
    }

    pub fn execute(
        &mut self,
        actor_id: ActorId,
        command: Command,
    ) -> Result<WorldOutput, WorldError> {
        match command {
            Command::Look(target) => self.look(actor_id, target.as_deref()),
            Command::Garden => self.garden_view(actor_id),
            Command::Gardens => self.list_gardens(actor_id),
            Command::Go(direction) => self.go(actor_id, &direction),
            Command::WalkTo(destination) => self.walk_to(actor_id, &destination),
            Command::Enter => self.enter_garden(actor_id),
            Command::Knock => self.knock(actor_id),
            Command::LockGarden => self.set_home_garden_lock(actor_id, true),
            Command::UnlockGarden => self.set_home_garden_lock(actor_id, false),
            Command::Admit(target) => self.admit_at_home_gate(actor_id, &target),
            Command::Home => self.go_home(actor_id),
            Command::Plant {
                species,
                position,
                name,
            } => self.plant(actor_id, &species, position, name.as_deref()),
            Command::Water(target) => self.water(actor_id, &target),
            Command::Prune(target) => self.prune(actor_id, &target),
            Command::Harvest(target) => self.harvest(actor_id, &target),
            Command::Inspect(target) => self.inspect(actor_id, &target),
            Command::Say(body) => self.say(actor_id, &body),
            Command::Inventory => self.inventory(actor_id),
            Command::Shop => self.shop(actor_id),
            Command::Buy(decoration) => self.buy_decoration(actor_id, &decoration),
            Command::Place {
                decoration,
                position,
            } => self.place_decoration(actor_id, &decoration, position),
            Command::TakeDecoration(target) => self.take_decoration(actor_id, &target),
            Command::Offer { item, recipient } => self.offer(actor_id, &item, &recipient),
            Command::Allow { actor, action } => {
                self.set_permission(actor_id, &actor, &action, true)
            }
            Command::Forbid { actor, action } => {
                self.set_permission(actor_id, &actor, &action, false)
            }
            Command::Visit(target) => self.visit(actor_id, &target),
            Command::ChangeWeather(weather) => self.change_weather(actor_id, &weather),
            Command::Weather => Ok(self.weather()),
            Command::Bog => self.bog_overview(),
            Command::Survey(position) => self.survey_bog(actor_id, position),
            Command::Restore(x, y) => self.restore_bog_cell(actor_id, x, y),
            Command::Who => Ok(self.who(actor_id)),
            Command::Changes => self.changes(actor_id),
            Command::Help => Ok(help(&self.content)),
            Command::Quit => Ok(WorldOutput::quit(self.content.text("output.quit"))),
        }
    }

    pub fn query(&self, actor_id: ActorId, command: Command) -> Result<WorldOutput, WorldError> {
        match command {
            Command::Look(target) => self.look(actor_id, target.as_deref()),
            Command::Garden => self.garden_view(actor_id),
            Command::Gardens => self.list_gardens(actor_id),
            Command::Inspect(target) => self.inspect(actor_id, &target),
            Command::Inventory => self.inventory(actor_id),
            Command::Shop => self.shop(actor_id),
            Command::Weather => Ok(self.weather()),
            Command::Bog => self.bog_overview(),
            Command::Survey(position) => self.survey_bog(actor_id, position),
            Command::Who => Ok(self.who(actor_id)),
            _ => Err(WorldError::Message(
                "agent world queries must be read-only observation commands".to_string(),
            )),
        }
    }

    pub fn tick(&mut self) -> Result<Vec<WorldEvent>, WorldError> {
        let mut clock = self.clock();
        clock.now += 1;
        apply_weather_cycle(&mut clock);

        let due = self.due_plants(clock.now);
        let mut meta = self.meta();
        let mut updates = Vec::new();
        let mut events = Vec::new();

        for mut plant in due {
            let old_stage = plant.stage.clone();
            let rain = match clock.weather {
                Weather::LightRain => 8,
                Weather::HeavyRain => 18,
                _ => 0,
            };
            plant.moisture = (plant.moisture - 8 + rain).clamp(0, 100);
            plant.health = if plant.moisture < 15 {
                (plant.health - 8).max(0)
            } else if plant.moisture > 85 {
                (plant.health - 2).max(0)
            } else {
                (plant.health + 2).min(100)
            };
            if plant.health > 20 && plant.moisture >= 20 {
                plant.growth = (plant.growth + 18).min(100);
            }
            plant.stage = stage_for(plant.growth, plant.health);
            plant.next_transition_at = clock.now + 1;

            if plant.stage != old_stage {
                let kind = if plant.stage == PlantStage::Flowering {
                    EventKind::Flowering
                } else {
                    EventKind::Growth
                };
                let message = self.content.render(
                    "event.plant_stage",
                    &[
                        ("plant", plant.name.clone()),
                        ("stage", plant.stage.to_string()),
                    ],
                );
                events.push(allocate_event(
                    &mut meta,
                    clock.now,
                    kind,
                    None,
                    Some(plant.room_id),
                    Some(plant.id),
                    vec![plant.owner_actor_id],
                    message,
                ));
            }
            updates.push(plant);
        }

        let (bog_cells, bog_organisms, newly_flowering, newly_dead) =
            self.calculate_bog_updates(&clock);
        if clock.now.is_multiple_of(24) && (!bog_cells.is_empty() || !bog_organisms.is_empty()) {
            let stressed = self.stream.rtx(
                |(_, _, _, _, _, _, _, _, (_, _, _, _, stressed, _, _, _))| stressed.iter().count(),
            );
            events.push(allocate_event(
                &mut meta,
                clock.now,
                EventKind::System,
                None,
                Some(WILD_EDGE),
                None,
                self.room_recipients(WILD_EDGE, &[]),
                self.content.render(
                    "event.bog_daily",
                    &[
                        ("stressed", stressed.to_string()),
                        ("flowering", newly_flowering.to_string()),
                        ("dead", newly_dead.to_string()),
                    ],
                ),
            ));
        }

        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Clock, &WorldRecord::Clock(clock));
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            for plant in updates {
                tx.upsert(&EntityKey::Plant(plant.id), &WorldRecord::Plant(plant));
            }
            for event in &events {
                tx.upsert(
                    &EntityKey::Event(event.id),
                    &WorldRecord::Event(event.clone()),
                );
            }
            for cell in bog_cells {
                tx.upsert(&EntityKey::BogCell(cell.id), &WorldRecord::BogCell(cell));
            }
            for organism in bog_organisms {
                tx.upsert(
                    &EntityKey::BogOrganism(organism.id),
                    &WorldRecord::BogOrganism(organism),
                );
            }
        });

        Ok(events)
    }

    fn calculate_bog_updates(
        &self,
        clock: &WorldClock,
    ) -> (Vec<BogCellState>, Vec<BogOrganismState>, usize, usize) {
        let Some(meta) = self.bog_meta() else {
            return (Vec::new(), Vec::new(), 0, 0);
        };
        let due_cells = self.due_bog_cells(clock.now, self.bog_config.work_budget);
        let mut cell_updates = BTreeMap::new();
        for cell in due_cells {
            let neighbors = self
                .bog_neighbor_ids(&meta, &cell)
                .into_iter()
                .filter_map(|id| self.bog_cell(id))
                .map(|neighbor| neighbor.water_table_mm)
                .collect::<Vec<_>>();
            let organisms = self.bog_organisms_in_cell(cell.id);
            let total_biomass_g = organisms
                .iter()
                .map(|organism| u64::from(organism.biomass_g))
                .sum();
            let peat_builders = organisms
                .iter()
                .filter(|organism| ecology::profile(&organism.species).peat_builder)
                .count();
            let updated =
                ecology::update_cell(cell, clock, &neighbors, total_biomass_g, peat_builders);
            cell_updates.insert(updated.id, updated);
        }

        let remaining_budget = self
            .bog_config
            .work_budget
            .saturating_sub(cell_updates.len());
        let due_organisms = self.due_bog_organisms(clock.now, remaining_budget);
        let mut newly_flowering = 0;
        let mut newly_dead = 0;
        let mut organism_updates = Vec::with_capacity(due_organisms.len());
        for organism in due_organisms {
            let Some(cell) = cell_updates
                .get(&organism.cell_id)
                .cloned()
                .or_else(|| self.bog_cell(organism.cell_id))
            else {
                continue;
            };
            let previous_stage = organism.stage.clone();
            let updated = ecology::update_organism(organism, &cell, clock);
            newly_flowering += usize::from(
                previous_stage != BogLifeStage::Flowering
                    && updated.stage == BogLifeStage::Flowering,
            );
            newly_dead += usize::from(
                previous_stage != BogLifeStage::Dead && updated.stage == BogLifeStage::Dead,
            );
            organism_updates.push(updated);
        }

        (
            cell_updates.into_values().collect(),
            organism_updates,
            newly_flowering,
            newly_dead,
        )
    }

    fn bog_neighbor_ids(&self, meta: &BogMeta, cell: &BogCellState) -> Vec<BogCellId> {
        let mut ids = Vec::with_capacity(4);
        for (dx, dy) in [(-1_i32, 0_i32), (1, 0), (0, -1), (0, 1)] {
            let x = i32::from(cell.x) + dx;
            let y = i32::from(cell.y) + dy;
            if x >= 0
                && y >= 0
                && let Some(id) = ecology::cell_id(meta.edge_length, x as u16, y as u16)
            {
                ids.push(id);
            }
        }
        ids
    }

    pub fn prepare_due_agent_turns(&mut self) -> Result<Vec<AgentTurn>, WorldError> {
        let now = self.clock().now;
        let due = self.stream.rtx(|(_, _, _, _, _, _, _, agents, _)| {
            agents
                .range(..=now)
                .flat_map(|(scored, count)| std::iter::repeat_n(scored.val, count.max(0) as usize))
                .collect::<Vec<_>>()
        });
        let mut turns = Vec::new();
        for due_actor in due {
            let mut actor = match self.actor(due_actor.id) {
                Some(actor) if actor.agent.as_ref().is_some_and(|profile| profile.enabled) => actor,
                _ => continue,
            };
            let may_act = actor
                .agent
                .as_ref()
                .is_some_and(|profile| profile.action_budget > 0);
            if may_act {
                turns.push(self.agent_turn(&actor)?);
            }
            if let Some(profile) = &mut actor.agent {
                let wake_interval = self
                    .content
                    .npc_for_actor(&profile.npc_id, &actor.name)
                    .map_or(5, |npc| npc.wake_interval);
                profile.next_wake_at = now + wake_interval;
            }
            self.stream.wtx(|tx| {
                tx.upsert(&EntityKey::Actor(actor.id), &WorldRecord::Actor(actor));
            });
        }
        Ok(turns)
    }

    pub fn prepare_reactive_agent_turns(
        &mut self,
        events: &[WorldEvent],
    ) -> Result<Vec<AgentTurn>, WorldError> {
        let mut triggers_by_agent = BTreeMap::<ActorId, (Vec<String>, Vec<String>)>::new();
        for event in events
            .iter()
            .filter(|event| event.kind == EventKind::Speech)
        {
            let (Some(speaker_id), Some(room_id)) = (event.actor_id, event.room_id) else {
                continue;
            };
            let Some(speaker) = self.actor(speaker_id) else {
                continue;
            };
            if speaker.kind != ActorKind::Human || speaker.current_room_id != room_id {
                continue;
            }
            for listener in self.actors_in_room(room_id).into_iter().filter(|actor| {
                actor.id != speaker_id
                    && actor
                        .agent
                        .as_ref()
                        .is_some_and(|profile| profile.enabled && profile.action_budget > 0)
            }) {
                triggers_by_agent
                    .entry(listener.id)
                    .or_default()
                    .0
                    .push(event.message.clone());
            }
        }
        for event in events
            .iter()
            .filter(|event| event.kind == EventKind::Knocking)
        {
            let (Some(knocker_id), Some(gate_id)) = (event.actor_id, event.room_id) else {
                continue;
            };
            let Some(knocker) = self.actor(knocker_id) else {
                continue;
            };
            if knocker.kind != ActorKind::Human || knocker.current_room_id != gate_id {
                continue;
            }
            let Some(gate) = self.room(gate_id) else {
                continue;
            };
            let Some((garden, _)) = self.garden_at_gate(&gate) else {
                continue;
            };
            let Some(owner) = self.actor(garden.owner_actor_id).filter(|actor| {
                actor
                    .agent
                    .as_ref()
                    .is_some_and(|profile| profile.enabled && profile.action_budget > 0)
            }) else {
                continue;
            };
            triggers_by_agent
                .entry(owner.id)
                .or_default()
                .1
                .push(event.message.clone());
        }

        let now = self.clock().now;
        let mut turns = Vec::with_capacity(triggers_by_agent.len());
        for (actor_id, (speech, knocks)) in triggers_by_agent {
            let mut actor = self.require_actor(actor_id)?;
            turns.push(self.agent_turn_with_triggers(&actor, speech, knocks)?);
            if let Some(profile) = &mut actor.agent {
                let wake_interval = self
                    .content
                    .npc_for_actor(&profile.npc_id, &actor.name)
                    .map_or(5, |npc| npc.wake_interval);
                profile.next_wake_at = now + wake_interval;
            }
            self.stream.wtx(|tx| {
                tx.upsert(&EntityKey::Actor(actor.id), &WorldRecord::Actor(actor));
            });
        }
        Ok(turns)
    }

    pub fn execute_agent_plan(
        &mut self,
        actor_id: ActorId,
        command: Command,
        intention: &str,
    ) -> Result<WorldOutput, WorldError> {
        let mut output = self.execute(actor_id, command)?;
        let actor = self.require_actor(actor_id)?;
        let audit = self.record_agent_audit(
            actor_id,
            self.content.render(
                "event.agent_intention",
                &[
                    ("name", actor.name.clone()),
                    ("intention", intention.trim().to_string()),
                ],
            ),
        );
        output.events.push(audit);
        Ok(output)
    }

    pub fn actors(&self) -> Vec<ActorState> {
        self.stream
            .rtx(|(actors, ..)| actors.iter().map(|(_, actor)| actor).collect())
    }

    fn gardens(&self) -> Vec<GardenState> {
        self.stream
            .rtx(|(_, gardens, ..)| gardens.iter().map(|(_, garden)| garden).collect())
    }

    fn rooms(&self) -> Vec<RoomState> {
        self.stream
            .rtx(|(_, _, rooms, ..)| rooms.iter().map(|(_, room)| room).collect())
    }

    pub fn actor(&self, id: ActorId) -> Option<ActorState> {
        self.stream
            .get(&EntityKey::Actor(id))
            .and_then(record_actor)
    }

    pub fn clock(&self) -> WorldClock {
        self.stream
            .get(&EntityKey::Clock)
            .and_then(record_clock)
            .expect("world clock must exist after bootstrap")
    }

    pub fn debug_snapshot(&self, event_limit: usize) -> DebugSnapshot {
        let clock = self.clock();
        let meta = self.meta();
        let bog_meta = self.bog_meta();
        self.stream.rtx(
            |(
                actors,
                gardens,
                rooms,
                plants,
                events,
                schedule,
                needs_water,
                agent_schedule,
                bog,
            )| {
                let mut actors = actors
                    .iter()
                    .map(|(_, mut actor)| {
                        actor.auth_fingerprint = None;
                        actor
                    })
                    .collect::<Vec<_>>();
                let mut gardens = gardens.iter().map(|(_, garden)| garden).collect::<Vec<_>>();
                let mut rooms = rooms.iter().map(|(_, room)| room).collect::<Vec<_>>();
                let mut plants = plants.iter().map(|(_, plant)| plant).collect::<Vec<_>>();
                let (
                    bog_cells,
                    bog_organisms,
                    bog_cell_schedule,
                    bog_organism_schedule,
                    stressed_bog_organisms,
                    _,
                    bog_species,
                    bog_moisture,
                ) = bog;
                let mut world_cells = bog_cells.iter().map(|(_, cell)| cell).collect::<Vec<_>>();
                let mut organisms = bog_organisms
                    .iter()
                    .map(|(_, organism)| organism)
                    .collect::<Vec<_>>();
                let mut species = bog_species.iter().collect::<Vec<_>>();
                actors.sort_by_key(|actor| actor.id);
                gardens.sort_by_key(|garden| garden.id);
                rooms.sort_by_key(|room| room.id);
                plants.sort_by_key(|plant| plant.id);
                world_cells.sort_by_key(|cell| cell.id);
                organisms.sort_by_key(|organism| organism.id);
                species.sort_by(|left, right| left.0.cmp(&right.0));

                let room_positions = bog_meta
                    .as_ref()
                    .map(|meta| ecology::room_grid_positions(meta.edge_length, &rooms))
                    .unwrap_or_default();
                let mut region_counts = BTreeMap::<RoomId, usize>::new();
                let world_cells = world_cells
                    .into_iter()
                    .filter_map(|cell| {
                        let room_id = room_positions
                            .iter()
                            .min_by_key(|(room_id, (room_x, room_y))| {
                                let dx = i64::from(*room_x) - i64::from(cell.x);
                                let dy = i64::from(*room_y) - i64::from(cell.y);
                                (dx * dx + dy * dy, **room_id)
                            })
                            .map(|(room_id, _)| *room_id)?;
                        *region_counts.entry(room_id).or_default() += 1;
                        Some(DebugWorldCell { cell, room_id })
                    })
                    .collect();
                let world_grid = bog_meta.as_ref().map(|meta| DebugWorldGrid {
                    ecology_version: meta.ecology_version,
                    edge_length: meta.edge_length,
                    next_organism_id: meta.next_organism_id,
                    regions: room_positions
                        .iter()
                        .map(|(room_id, (center_x, center_y))| DebugRoomRegion {
                            room_id: *room_id,
                            center_x: *center_x,
                            center_y: *center_y,
                            cell_count: region_counts.get(room_id).copied().unwrap_or_default(),
                        })
                        .collect(),
                });

                let events = events
                    .top(event_limit)
                    .into_iter()
                    .map(|scored| scored.val)
                    .collect();
                let mut needs_water_plant_ids = needs_water
                    .iter()
                    .map(|(plant_id, _)| plant_id)
                    .collect::<Vec<_>>();
                needs_water_plant_ids.sort();
                let plant_schedule = schedule
                    .iter()
                    .filter(|(_, count)| *count > 0)
                    .map(|(scored, _)| DebugScheduleEntry {
                        entity: "plant",
                        id: scored.val.id.0,
                        label: scored.val.name,
                        at: scored.score,
                    })
                    .collect();
                let agent_schedule = agent_schedule
                    .iter()
                    .filter(|(_, count)| *count > 0)
                    .map(|(scored, _)| DebugScheduleEntry {
                        entity: "actor",
                        id: scored.val.id.0,
                        label: scored.val.name,
                        at: scored.score,
                    })
                    .collect();
                let cell_schedule = bog_cell_schedule
                    .iter()
                    .filter(|(_, count)| *count > 0)
                    .take(250)
                    .map(|(scored, _)| DebugScheduleEntry {
                        entity: "world_cell",
                        id: scored.val.id.0,
                        label: format!("{},{}", scored.val.x, scored.val.y),
                        at: scored.score,
                    })
                    .collect();
                let organism_schedule = bog_organism_schedule
                    .iter()
                    .filter(|(_, count)| *count > 0)
                    .take(250)
                    .map(|(scored, _)| DebugScheduleEntry {
                        entity: "organism",
                        id: scored.val.id.0,
                        label: scored.val.species,
                        at: scored.score,
                    })
                    .collect();
                let mut stressed_organism_ids = stressed_bog_organisms
                    .iter()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>();
                stressed_organism_ids.sort();

                DebugSnapshot {
                    captured_at_unix_ms: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                    clock,
                    meta,
                    actors,
                    gardens,
                    rooms,
                    plants,
                    world_grid,
                    world_cells,
                    organisms,
                    species,
                    events,
                    agent_actions: Vec::new(),
                    views: DebugMaterializedViews {
                        needs_water_plant_ids,
                        plant_schedule,
                        agent_schedule,
                        cell_schedule,
                        organism_schedule,
                        stressed_organism_ids,
                        moisture_p10: bog_moisture.quantile(0.1),
                        moisture_p50: bog_moisture.quantile(0.5),
                        moisture_p90: bog_moisture.quantile(0.9),
                    },
                }
            },
        )
    }

    pub fn checkpoint(&mut self) {
        self.stream.checkpoint();
    }

    fn bootstrap(&mut self) {
        if self.stream.contains(&EntityKey::Clock) {
            return;
        }

        let mut rooms = shared_rooms(&self.content);
        for (room_id, garden_id) in [
            (GLASSHOUSE, GLASSHOUSE_GARDEN),
            (MOON_BED, MOON_BED_GARDEN),
            (POND, POND_GARDEN),
            (COMPOST, COMPOST_GARDEN),
            (WILD_EDGE, WILD_EDGE_GARDEN),
        ] {
            rooms
                .iter_mut()
                .find(|room| room.id == room_id)
                .expect("shared garden room exists")
                .garden_id = Some(garden_id);
        }
        connect(&mut rooms, GATE, "north", COMMON_PATH);
        connect(&mut rooms, COMMON_PATH, "south", GATE);
        connect(&mut rooms, COMMON_PATH, "north", GLASSHOUSE);
        connect(&mut rooms, GLASSHOUSE, "south", COMMON_PATH);
        connect(&mut rooms, COMMON_PATH, "east", MOON_BED);
        connect(&mut rooms, MOON_BED, "west", COMMON_PATH);
        connect(&mut rooms, COMMON_PATH, "west", POND);
        connect(&mut rooms, POND, "east", COMMON_PATH);
        connect(&mut rooms, GLASSHOUSE, "east", COMPOST);
        connect(&mut rooms, COMPOST, "west", GLASSHOUSE);
        connect(&mut rooms, POND, "north", WILD_EDGE);
        connect(&mut rooms, WILD_EDGE, "south", POND);

        let meta = WorldMeta::default();
        let clock = WorldClock {
            now: 0,
            season: Season::Spring,
            weather: Weather::LightRain,
            temperature_c: 14,
        };
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::Clock, &WorldRecord::Clock(clock));
            for room in rooms {
                tx.upsert(&EntityKey::Room(room.id), &WorldRecord::Room(room));
            }
            for garden in shared_gardens(&self.content) {
                tx.upsert(&EntityKey::Garden(garden.id), &WorldRecord::Garden(garden));
            }
        });
    }

    fn ensure_garden_gates(&mut self) {
        let actors = self.actors();
        let gardens = self
            .gardens()
            .into_iter()
            .filter(|garden| garden.owner_actor_id.0 != 0)
            .collect::<Vec<_>>();
        let rooms = self.rooms();
        let mut meta = self.meta();
        let mut room_updates = Vec::new();

        for garden in gardens {
            let Some(mut home_room) = rooms.iter().find(|room| room.id == garden.room_id).cloned()
            else {
                continue;
            };
            let existing_gate = rooms
                .iter()
                .find(|room| {
                    room.kind == RoomKind::GardenGate
                        && room.exits.get("in") == Some(&garden.room_id)
                })
                .cloned();
            let gate_room = existing_gate.unwrap_or_else(|| {
                let gate_room_id = RoomId(meta.next_room_id);
                meta.next_room_id += 1;
                let owner_name = actors
                    .iter()
                    .find(|actor| actor.id == garden.owner_actor_id)
                    .map_or(garden.name.as_str(), |actor| actor.name.as_str());
                garden_gate_room(&self.content, gate_room_id, garden.room_id, owner_name)
            });
            if home_room.exits.get("out") != Some(&gate_room.id) {
                home_room.exits.insert("out".to_string(), gate_room.id);
                room_updates.push(home_room);
            }
            if !rooms.iter().any(|room| room.id == gate_room.id) {
                room_updates.push(gate_room);
            }
        }

        if room_updates.is_empty() {
            return;
        }
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            for room in room_updates {
                tx.upsert(&EntityKey::Room(room.id), &WorldRecord::Room(room));
            }
        });
    }

    fn bootstrap_bog(&mut self) {
        if self.stream.contains(&EntityKey::BogMeta) {
            return;
        }

        let edge_length = self.bog_config.edge_length;
        let cells = (0..edge_length)
            .flat_map(|y| (0..edge_length).map(move |x| ecology::seed_cell(edge_length, x, y)))
            .collect::<Vec<_>>();
        let total_cells = cells.len() as u64;
        let organisms = (1..=self.bog_config.initial_organisms)
            .map(|id| {
                ecology::seed_organism(BogOrganismId(id), total_cells, |cell_id| {
                    cells[(cell_id.0 - 1) as usize].clone()
                })
            })
            .collect::<Vec<_>>();
        let bog_meta = BogMeta {
            ecology_version: ecology::ECOLOGY_VERSION,
            edge_length,
            next_organism_id: self.bog_config.initial_organisms + 1,
        };

        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::BogMeta, &WorldRecord::BogMeta(bog_meta));
            for cell in cells {
                tx.upsert(&EntityKey::BogCell(cell.id), &WorldRecord::BogCell(cell));
            }
            for organism in organisms {
                tx.upsert(
                    &EntityKey::BogOrganism(organism.id),
                    &WorldRecord::BogOrganism(organism),
                );
            }
        });
    }

    fn meta(&self) -> WorldMeta {
        self.stream
            .get(&EntityKey::Meta)
            .and_then(record_meta)
            .expect("world metadata must exist after bootstrap")
    }

    fn bog_meta(&self) -> Option<BogMeta> {
        self.stream
            .get(&EntityKey::BogMeta)
            .and_then(record_bog_meta)
    }

    fn bog_cell(&self, id: BogCellId) -> Option<BogCellState> {
        self.stream
            .get(&EntityKey::BogCell(id))
            .and_then(record_bog_cell)
    }

    fn bog_organisms_in_cell(&self, id: BogCellId) -> Vec<BogOrganismState> {
        self.stream.rtx(
            |(_, _, _, _, _, _, _, _, (_, _, _, _, _, organisms_by_cell, _, _))| {
                organisms_by_cell.get(&id)
            },
        )
    }

    fn due_bog_cells(&self, now: WorldTime, limit: usize) -> Vec<BogCellState> {
        self.stream.rtx(
            |(_, _, _, _, _, _, _, _, (_, _, schedule, _, _, _, _, _))| {
                schedule
                    .range(..=now)
                    .flat_map(|(scored, count)| {
                        std::iter::repeat_n(scored.val, count.max(0) as usize)
                    })
                    .take(limit)
                    .collect()
            },
        )
    }

    fn due_bog_organisms(&self, now: WorldTime, limit: usize) -> Vec<BogOrganismState> {
        self.stream.rtx(
            |(_, _, _, _, _, _, _, _, (_, _, _, schedule, _, _, _, _))| {
                schedule
                    .range(..=now)
                    .flat_map(|(scored, count)| {
                        std::iter::repeat_n(scored.val, count.max(0) as usize)
                    })
                    .take(limit)
                    .collect()
            },
        )
    }

    fn room(&self, id: RoomId) -> Option<RoomState> {
        self.stream.get(&EntityKey::Room(id)).and_then(record_room)
    }

    fn garden(&self, id: GardenId) -> Option<GardenState> {
        self.stream
            .get(&EntityKey::Garden(id))
            .and_then(record_garden)
    }

    fn garden_is_unlocked(&self, id: GardenId) -> bool {
        self.stream
            .get(&EntityKey::GardenAccess(id))
            .and_then(record_garden_access)
            .is_some_and(|access| access.unlocked)
    }

    fn has_garden_admission(&self, garden_id: GardenId, actor_id: ActorId) -> bool {
        self.stream
            .get(&EntityKey::GardenAdmission(garden_id, actor_id))
            .and_then(record_garden_admission)
            .is_some()
    }

    fn gate_for_garden(&self, garden: &GardenState) -> Option<RoomState> {
        self.rooms().into_iter().find(|room| {
            room.kind == RoomKind::GardenGate && room.exits.get("in") == Some(&garden.room_id)
        })
    }

    fn garden_at_gate(&self, gate: &RoomState) -> Option<(GardenState, RoomState)> {
        let home_room = self.room(*gate.exits.get("in")?)?;
        let garden = self.garden(home_room.garden_id?)?;
        Some((garden, home_room))
    }

    fn plants_in_room(&self, room_id: RoomId) -> Vec<PlantState> {
        self.stream.rtx(|(_, _, _, plants, ..)| {
            plants
                .iter()
                .filter_map(|(_, plant)| (plant.room_id == room_id).then_some(plant))
                .collect()
        })
    }

    fn actors_in_room(&self, room_id: RoomId) -> Vec<ActorState> {
        self.actors()
            .into_iter()
            .filter(|actor| actor.current_room_id == room_id)
            .collect()
    }

    fn room_recipients(&self, room_id: RoomId, extras: &[ActorId]) -> Vec<ActorId> {
        let mut recipients = self
            .actors_in_room(room_id)
            .into_iter()
            .map(|actor| actor.id)
            .chain(extras.iter().copied())
            .filter(|id| id.0 != 0)
            .collect::<Vec<_>>();
        recipients.sort();
        recipients.dedup();
        recipients
    }

    fn find_plant(&self, room_id: RoomId, target: &str) -> Option<PlantState> {
        let target = target.trim().to_ascii_lowercase();
        self.plants_in_room(room_id).into_iter().find(|plant| {
            plant.name.to_ascii_lowercase() == target
                || plant.species.to_ascii_lowercase() == target
                || plant.id.to_string() == target
                || plant.position.to_string().to_ascii_lowercase() == target
        })
    }

    fn due_plants(&self, now: WorldTime) -> Vec<PlantState> {
        self.stream.rtx(|(_, _, _, _, _, schedule, ..)| {
            schedule
                .range(..=now)
                .flat_map(|(scored, count)| std::iter::repeat_n(scored.val, count.max(0) as usize))
                .collect()
        })
    }

    fn look(&self, actor_id: ActorId, target: Option<&str>) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        if let Some(target) = target {
            if target.eq_ignore_ascii_case("garden") || target.eq_ignore_ascii_case("board") {
                return self.describe_garden(actor_id);
            }
            let normalized = normalize_direction(target);
            let current = self.require_room(actor.current_room_id)?;
            if is_direction(normalized) || current.exits.contains_key(normalized) {
                let destination_id = current.exits.get(normalized).copied().ok_or_else(|| {
                    WorldError::Message(
                        self.content
                            .render("error.no_exit", &[("direction", target.to_string())]),
                    )
                })?;
                let mut output = self.look_room(actor_id, destination_id, true)?;
                output.lines.insert(
                    0,
                    self.content.render(
                        "output.look_direction",
                        &[("direction", normalized.to_string())],
                    ),
                );
                return Ok(output);
            }
            return self.inspect(actor_id, target);
        }
        self.look_room(actor_id, actor.current_room_id, false)
    }

    fn look_room(
        &self,
        actor_id: ActorId,
        room_id: RoomId,
        directional_preview: bool,
    ) -> Result<WorldOutput, WorldError> {
        let room = self.require_room(room_id)?;
        let mut lines = vec![String::new(), room.name.clone(), room.description.clone()];
        if room.id == WILD_EDGE {
            let edge = self.bog_meta().map_or(24, |meta| meta.edge_length);
            lines.push(String::new());
            lines.push(format!(
                "Beyond the garden board, a {edge}×{edge} living peat bog responds to weather, \
                 drainage, competition, and time. Use `bog` or `survey <x> <y>`."
            ));
        }

        if directional_preview && room.garden_id.is_some() {
            lines.push(String::new());
            lines.push(self.content.text("output.garden_ahead").to_string());
        } else {
            let mut plants = self.plants_in_room(room.id);
            let mut decorations = room
                .garden_id
                .and_then(|id| self.garden(id))
                .map_or_else(Vec::new, |garden| garden.decorations);
            if !plants.is_empty() || !decorations.is_empty() {
                lines.push(String::new());
                plants.sort_by_key(|plant| plant.position);
                for plant in plants {
                    lines.push(format!(
                        "{}  {} — {} ({})",
                        plant.position, plant.name, plant.species, plant.stage
                    ));
                }
                decorations.sort_by_key(|decoration| decoration.position);
                for decoration in decorations {
                    lines.push(format!(
                        "{}  {} — {}",
                        decoration.position, decoration.name, decoration.description
                    ));
                }
            }
        }

        let others: Vec<_> = self
            .actors_in_room(room.id)
            .into_iter()
            .filter(|other| other.id != actor_id)
            .map(|other| other.name)
            .collect();
        if !others.is_empty() {
            lines.push(String::new());
            lines.push(
                self.content
                    .render("output.also_here", &[("names", others.join(", "))]),
            );
        }

        if !room.exits.is_empty() {
            lines.push(String::new());
            lines.push(self.content.render(
                if directional_preview {
                    "output.exits_there"
                } else {
                    "output.exits"
                },
                &[(
                    "exits",
                    room.exits.keys().cloned().collect::<Vec<_>>().join(", "),
                )],
            ));
        }
        Ok(WorldOutput::lines(lines))
    }

    fn describe_garden(&self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let room = self.require_room(actor.current_room_id)?;
        if room.garden_id.is_none() {
            return Err(WorldError::Message(
                self.content.text("error.no_garden_board").to_string(),
            ));
        }

        let mut plants = self.plants_in_room(room.id);
        plants.sort_by_key(|plant| plant.position);
        let garden = self
            .garden(room.garden_id.expect("garden ID was checked"))
            .ok_or_else(|| {
                WorldError::Message(self.content.text("error.garden_no_keeper").to_string())
            })?;
        Ok(WorldOutput::lines(render_garden_description(
            &room.name,
            &plants,
            &garden.decorations,
        )))
    }

    fn garden_view(&self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let room = self.require_room(actor.current_room_id)?;
        if room.garden_id.is_none() {
            return Err(WorldError::Message(
                self.content.text("error.no_garden_board").to_string(),
            ));
        }

        let mut plants = self.plants_in_room(room.id);
        let garden = self
            .garden(room.garden_id.expect("garden ID was checked"))
            .ok_or_else(|| {
                WorldError::Message(self.content.text("error.garden_no_keeper").to_string())
            })?;
        let mut lines = render_garden_board(&self.content, &plants, &garden.decorations);
        if !plants.is_empty() {
            lines.push(String::new());
            plants.sort_by_key(|plant| plant.position);
            lines.extend(plants.into_iter().map(|plant| {
                format!(
                    "{}  {} — {} ({})",
                    plant.position, plant.name, plant.species, plant.stage
                )
            }));
        }
        if !garden.decorations.is_empty() {
            lines.push(String::new());
            let mut decorations = garden.decorations;
            decorations.sort_by_key(|decoration| decoration.position);
            lines.extend(decorations.into_iter().map(|decoration| {
                format!(
                    "{}  {} — {}",
                    decoration.position, decoration.name, decoration.description
                )
            }));
        }
        Ok(WorldOutput::lines(lines))
    }

    fn inspect(&self, actor_id: ActorId, target: &str) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        if target.eq_ignore_ascii_case("soil") {
            let plants = self.plants_in_room(actor.current_room_id);
            let damp = if plants.is_empty() {
                self.content.text("state.soil_undisturbed").to_string()
            } else {
                let average = plants
                    .iter()
                    .map(|plant| plant.moisture as i32)
                    .sum::<i32>()
                    / plants.len() as i32;
                moisture_word(average as i16).to_string()
            };
            return Ok(WorldOutput::lines([self
                .content
                .render("output.inspect_soil", &[("moisture", damp)])]));
        }

        if let Some(room) = self.room(actor.current_room_id)
            && let Some(garden) = room.garden_id.and_then(|id| self.garden(id))
            && let Some(decoration) = find_decoration(&garden, target)
        {
            return Ok(WorldOutput::lines([
                format!("{} — {}", decoration.name, decoration.description),
                format!("It stands at {}.", decoration.position),
            ]));
        }

        let plant = self
            .find_plant(actor.current_room_id, target)
            .ok_or_else(|| {
                WorldError::Message(
                    self.content
                        .render("error.not_visible", &[("target", target.to_string())]),
                )
            })?;
        Ok(WorldOutput::lines([
            self.content.render(
                "output.plant_heading",
                &[
                    ("name", plant.name.clone()),
                    ("species", plant.species.clone()),
                ],
            ),
            self.content.render(
                "output.plant_condition",
                &[
                    ("stage", plant.stage.to_string()),
                    ("moisture", moisture_word(plant.moisture).to_string()),
                    ("health", plant.health.to_string()),
                    ("growth", plant.growth.to_string()),
                ],
            ),
            self.content.render(
                "output.plant_next_change",
                &[("next_hour", plant.next_transition_at.to_string())],
            ),
        ]))
    }

    fn go(&mut self, actor_id: ActorId, direction: &str) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let current = self.require_room(actor.current_room_id)?;
        let normalized = normalize_direction(direction);
        if current.kind == RoomKind::GardenGate && normalized.eq_ignore_ascii_case("in") {
            return self.enter_garden(actor_id);
        }
        let destination_id = current.exits.get(normalized).copied().ok_or_else(|| {
            WorldError::Message(
                self.content
                    .render("error.no_exit", &[("direction", direction.to_string())]),
            )
        })?;
        let destination = self.require_room(destination_id)?;
        let mut meta = self.meta();
        let clock = self.clock();
        let departure = allocate_event(
            &mut meta,
            clock.now,
            EventKind::Departure,
            Some(actor_id),
            Some(current.id),
            None,
            self.room_recipients(current.id, &[]),
            self.content.render(
                "event.leaves_toward",
                &[
                    ("actor", actor.name.clone()),
                    ("destination", destination.name.clone()),
                ],
            ),
        );
        let arrival = allocate_event(
            &mut meta,
            clock.now,
            EventKind::Arrival,
            Some(actor_id),
            Some(destination.id),
            None,
            self.room_recipients(destination.id, &[]),
            self.content.render(
                "event.arrives_from",
                &[
                    ("actor", actor.name.clone()),
                    ("origin", current.name.clone()),
                ],
            ),
        );
        actor.current_room_id = destination.id;
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::Actor(actor_id), &WorldRecord::Actor(actor));
            for event in [&departure, &arrival] {
                tx.upsert(
                    &EntityKey::Event(event.id),
                    &WorldRecord::Event(event.clone()),
                );
            }
        });
        let mut output = self.look(actor_id, None)?;
        output.events = vec![departure, arrival];
        Ok(output)
    }

    fn walk_to(
        &mut self,
        actor_id: ActorId,
        destination_name: &str,
    ) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let destination = self.resolve_walk_destination(destination_name)?;
        if actor.current_room_id == destination.id {
            return self.look(actor_id, None);
        }

        let route = self
            .route_between(&actor, actor.current_room_id, destination.id)
            .ok_or_else(|| {
                WorldError::Message(self.content.render(
                    "error.no_route",
                    &[("destination", destination.name.clone())],
                ))
            })?;
        let rooms_by_id = self
            .rooms()
            .into_iter()
            .map(|room| (room.id, room))
            .collect::<BTreeMap<_, _>>();
        let mut current = self.require_room(actor.current_room_id)?;
        let mut directions = Vec::with_capacity(route.len());
        let mut passed = Vec::with_capacity(route.len().saturating_sub(1));
        let mut events = Vec::with_capacity(route.len() * 2);
        let mut admissions = Vec::new();
        let mut meta = self.meta();
        let now = self.clock().now;

        for (direction, room_id) in &route {
            let next = rooms_by_id.get(room_id).cloned().ok_or_else(|| {
                WorldError::Message(self.content.text("error.room_missing").to_string())
            })?;
            directions.push(direction.clone());
            if next.id != destination.id {
                passed.push(next.name.clone());
            }
            let entering_garden =
                if current.kind == RoomKind::GardenGate && direction.eq_ignore_ascii_case("in") {
                    self.garden_at_gate(&current).map(|(garden, _)| garden)
                } else {
                    None
                };
            if let Some(garden) = &entering_garden
                && self.has_garden_admission(garden.id, actor_id)
            {
                admissions.push((garden.id, actor_id));
            }
            let (departure_message, arrival_message, arrival_extras) =
                if let Some(garden) = &entering_garden {
                    (
                        self.content.render(
                            "event.enters_from_gate",
                            &[
                                ("actor", actor.name.clone()),
                                ("garden", garden.name.clone()),
                            ],
                        ),
                        self.content.render(
                            "event.arrives_in_garden",
                            &[
                                ("actor", actor.name.clone()),
                                ("garden", garden.name.clone()),
                            ],
                        ),
                        vec![garden.owner_actor_id],
                    )
                } else {
                    (
                        self.content.render(
                            "event.leaves_toward",
                            &[
                                ("actor", actor.name.clone()),
                                ("destination", next.name.clone()),
                            ],
                        ),
                        self.content.render(
                            "event.arrives_from",
                            &[
                                ("actor", actor.name.clone()),
                                ("origin", current.name.clone()),
                            ],
                        ),
                        Vec::new(),
                    )
                };
            let departure = allocate_event(
                &mut meta,
                now,
                EventKind::Departure,
                Some(actor_id),
                Some(current.id),
                None,
                self.room_recipients(current.id, &[]),
                departure_message,
            );
            let arrival = allocate_event(
                &mut meta,
                now,
                EventKind::Arrival,
                Some(actor_id),
                Some(next.id),
                None,
                self.room_recipients(next.id, &arrival_extras),
                arrival_message,
            );
            events.extend([departure, arrival]);
            current = next;
        }

        actor.current_room_id = destination.id;
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::Actor(actor_id), &WorldRecord::Actor(actor));
            for (garden_id, admitted_actor_id) in admissions {
                tx.remove(&EntityKey::GardenAdmission(garden_id, admitted_actor_id));
            }
            for event in &events {
                tx.upsert(
                    &EntityKey::Event(event.id),
                    &WorldRecord::Event(event.clone()),
                );
            }
        });

        let route_text = directions.join(" → ");
        let summary = if passed.is_empty() {
            self.content.render(
                "output.walk_to_direct",
                &[
                    ("route", route_text),
                    ("destination", destination.name.clone()),
                ],
            )
        } else {
            self.content.render(
                "output.walk_to",
                &[
                    ("route", route_text),
                    ("places", human_join(&passed)),
                    ("destination", destination.name.clone()),
                ],
            )
        };
        let mut output = self.look(actor_id, None)?;
        output.lines.insert(0, summary);
        output.events = events;
        Ok(output)
    }

    fn resolve_walk_destination(&self, query: &str) -> Result<RoomState, WorldError> {
        let query = normalize_place_name(query);
        let rooms = self.rooms();
        let exact = rooms
            .iter()
            .filter(|room| normalize_place_name(&room.name) == query)
            .cloned()
            .collect::<Vec<_>>();
        let mut matches = if exact.is_empty() {
            rooms
                .into_iter()
                .filter(|room| normalize_place_name(&room.name).contains(&query))
                .collect::<Vec<_>>()
        } else {
            exact
        };
        matches.sort_by(|left, right| left.name.cmp(&right.name));
        matches.dedup_by_key(|room| room.id);
        match matches.len() {
            0 => Err(WorldError::Message(
                self.content
                    .render("error.unknown_place", &[("place", query)]),
            )),
            1 => Ok(matches.remove(0)),
            _ => Err(WorldError::Message(
                self.content.render(
                    "error.ambiguous_place",
                    &[(
                        "places",
                        matches
                            .into_iter()
                            .map(|room| room.name)
                            .collect::<Vec<_>>()
                            .join(", "),
                    )],
                ),
            )),
        }
    }

    fn route_between(
        &self,
        actor: &ActorState,
        start: RoomId,
        destination: RoomId,
    ) -> Option<Vec<(String, RoomId)>> {
        let rooms = self
            .rooms()
            .into_iter()
            .map(|room| (room.id, room))
            .collect::<BTreeMap<_, _>>();
        let mut frontier = VecDeque::from([start]);
        let mut previous = BTreeMap::<RoomId, (RoomId, String)>::new();
        let mut visited = BTreeSet::from([start]);

        while let Some(room_id) = frontier.pop_front() {
            let room = rooms.get(&room_id)?;
            for (direction, next_id) in &room.exits {
                let next = rooms.get(next_id)?;
                if !self.actor_may_traverse(actor, room, direction, next) {
                    continue;
                }
                if visited.insert(*next_id) {
                    previous.insert(*next_id, (room_id, direction.clone()));
                    if *next_id == destination {
                        let mut route = Vec::new();
                        let mut cursor = destination;
                        while cursor != start {
                            let (prior, direction) = previous.get(&cursor)?.clone();
                            route.push((direction, cursor));
                            cursor = prior;
                        }
                        route.reverse();
                        return Some(route);
                    }
                    frontier.push_back(*next_id);
                }
            }
        }
        None
    }

    fn actor_may_traverse(
        &self,
        actor: &ActorState,
        from: &RoomState,
        direction: &str,
        _to: &RoomState,
    ) -> bool {
        if from.kind != RoomKind::GardenGate || !direction.eq_ignore_ascii_case("in") {
            return true;
        }
        self.garden_at_gate(from)
            .is_some_and(|(garden, _)| self.may_enter_garden(actor, &garden))
    }

    fn go_home(&mut self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let garden = self.garden(actor.home_garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.home_missing").to_string())
        })?;
        if actor.current_room_id == garden.room_id {
            return self.look(actor_id, None);
        }
        let from = actor.current_room_id;
        actor.current_room_id = garden.room_id;
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Arrival,
            Some(actor_id),
            Some(garden.room_id),
            None,
            self.room_recipients(garden.room_id, &[actor_id]),
            self.content
                .render("event.returns_home", &[("actor", actor.name.clone())]),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::Actor(actor_id), &WorldRecord::Actor(actor));
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        let mut output = self.look(actor_id, None)?;
        output.events = vec![
            WorldEvent {
                room_id: Some(from),
                message: self.content.text("event.someone_leaves_home").to_string(),
                ..event.clone()
            },
            event,
        ];
        Ok(output)
    }

    fn plant(
        &mut self,
        actor_id: ActorId,
        species: &str,
        position: GardenPosition,
        name: Option<&str>,
    ) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let room = self.require_room(actor.current_room_id)?;
        let garden_id = room.garden_id.ok_or_else(|| {
            WorldError::Message(self.content.text("error.nowhere_to_plant").to_string())
        })?;
        let garden = self.garden(garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.garden_no_keeper").to_string())
        })?;
        self.require_tending_permission(&actor, &garden)?;
        if self
            .plants_in_room(room.id)
            .iter()
            .any(|plant| plant.position == position)
            || garden
                .decorations
                .iter()
                .any(|decoration| decoration.position == position)
        {
            return Err(WorldError::Message(self.content.render(
                "error.position_occupied",
                &[("position", position.to_string())],
            )));
        }

        let species = normalize_species(&self.content, species)?;
        let seed_index = actor
            .inventory
            .iter()
            .position(|item| {
                item.kind == ItemKind::Seed && item.species.eq_ignore_ascii_case(&species)
            })
            .ok_or_else(|| {
                WorldError::Message(
                    self.content
                        .render("error.seed_missing", &[("species", species.clone())]),
                )
            })?;
        actor.inventory.remove(seed_index);
        let mut meta = self.meta();
        let id = PlantId(meta.next_plant_id);
        meta.next_plant_id += 1;
        let clock = self.clock();
        let plant = PlantState {
            id,
            name: name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&species)
                .to_string(),
            species: species.clone(),
            position,
            owner_actor_id: actor_id,
            garden_id,
            room_id: room.id,
            moisture: 45,
            nutrients: 60,
            health: 80,
            growth: 0,
            stage: PlantStage::Seed,
            planted_at: clock.now,
            next_transition_at: clock.now + 1,
        };
        let event = allocate_event(
            &mut meta,
            clock.now,
            EventKind::Planting,
            Some(actor_id),
            Some(room.id),
            Some(id),
            self.room_recipients(room.id, &[garden.owner_actor_id]),
            self.content.render(
                "event.plants",
                &[
                    ("actor", actor.name.clone()),
                    ("plant", plant.name.clone()),
                    ("position", plant.position.to_string()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor_id),
                &WorldRecord::Actor(actor.clone()),
            );
            tx.upsert(&EntityKey::Plant(id), &WorldRecord::Plant(plant.clone()));
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![
                self.content.render(
                    "output.plant",
                    &[
                        ("plant", plant.name.clone()),
                        ("position", plant.position.to_string()),
                    ],
                ),
                self.content.text("output.seed_closes").to_string(),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn water(&mut self, actor_id: ActorId, target: &str) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let mut plant = self
            .find_plant(actor.current_room_id, target)
            .ok_or_else(|| {
                WorldError::Message(
                    self.content
                        .render("error.not_visible", &[("target", target.to_string())]),
                )
            })?;
        let garden = self.garden(plant.garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.plant_garden_missing").to_string())
        })?;
        self.require_tending_permission(&actor, &garden)?;
        plant.moisture = (plant.moisture + 30).min(100);
        plant.health = (plant.health + 4).min(100);
        plant.next_transition_at = plant.next_transition_at.min(self.clock().now + 1);

        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Watering,
            Some(actor_id),
            Some(plant.room_id),
            Some(plant.id),
            self.room_recipients(plant.room_id, &[garden.owner_actor_id]),
            self.content.render(
                "event.waters",
                &[("actor", actor.name.clone()), ("plant", plant.name.clone())],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Plant(plant.id),
                &WorldRecord::Plant(plant.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![
                self.content
                    .render("output.water", &[("plant", plant.name.clone())]),
                self.content.render(
                    "output.soil_now",
                    &[("moisture", moisture_word(plant.moisture).to_string())],
                ),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn prune(&mut self, actor_id: ActorId, target: &str) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let mut plant = self
            .find_plant(actor.current_room_id, target)
            .ok_or_else(|| {
                WorldError::Message(
                    self.content
                        .render("error.not_visible", &[("target", target.to_string())]),
                )
            })?;
        let garden = self.garden(plant.garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.plant_garden_missing").to_string())
        })?;
        self.require_tending_permission(&actor, &garden)?;
        plant.health = (plant.health + 8).min(100);
        plant.growth = plant.growth.saturating_sub(4);
        plant.next_transition_at = self.clock().now + 1;
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Pruning,
            Some(actor_id),
            Some(plant.room_id),
            Some(plant.id),
            self.room_recipients(plant.room_id, &[garden.owner_actor_id]),
            self.content.render(
                "event.prunes",
                &[("actor", actor.name.clone()), ("plant", plant.name.clone())],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Plant(plant.id),
                &WorldRecord::Plant(plant.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![
                self.content
                    .render("output.prune", &[("plant", plant.name.clone())]),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn harvest(&mut self, actor_id: ActorId, target: &str) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let mut plant = self
            .find_plant(actor.current_room_id, target)
            .ok_or_else(|| {
                WorldError::Message(
                    self.content
                        .render("error.not_visible", &[("target", target.to_string())]),
                )
            })?;
        let garden = self.garden(plant.garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.plant_garden_missing").to_string())
        })?;
        self.require_harvest_permission(&actor, &garden)?;
        if plant.stage != PlantStage::Fruiting {
            return Err(WorldError::Message(self.content.render(
                "error.not_ready_to_harvest",
                &[("plant", plant.name.clone())],
            )));
        }
        let mut meta = self.meta();
        let seed = allocate_item(&mut meta, ItemKind::Seed, &plant.species);
        let produce = allocate_item(&mut meta, ItemKind::Produce, &plant.species);
        actor.inventory.extend([seed.clone(), produce]);
        plant.growth = 55;
        plant.stage = PlantStage::Growing;
        plant.next_transition_at = self.clock().now + 1;
        let mut event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Harvesting,
            Some(actor_id),
            Some(plant.room_id),
            Some(plant.id),
            self.room_recipients(plant.room_id, &[garden.owner_actor_id]),
            self.content.render(
                "event.harvests",
                &[("actor", actor.name.clone()), ("plant", plant.name.clone())],
            ),
        );
        event.recipients.sort();
        event.recipients.dedup();
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor_id),
                &WorldRecord::Actor(actor.clone()),
            );
            tx.upsert(
                &EntityKey::Plant(plant.id),
                &WorldRecord::Plant(plant.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![
                self.content
                    .render("output.harvest", &[("plant", plant.name.clone())]),
                self.content.text("output.save_seed").to_string(),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn inventory(&self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        if actor.inventory.is_empty() {
            return Ok(WorldOutput::lines([self
                .content
                .text("output.inventory_empty")]));
        }
        let mut lines = vec![self.content.text("output.inventory_heading").to_string()];
        lines.extend(actor.inventory.iter().map(|item| {
            self.content.render(
                "output.inventory_item",
                &[("id", item.id.to_string()), ("item", item.display_name())],
            )
        }));
        Ok(WorldOutput::lines(lines))
    }

    fn shop(&self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        self.require_merchant_here(&actor)?;
        let mut lines = vec![
            self.content.merchant.greeting.clone(),
            self.content.text("output.shop_heading").to_string(),
        ];
        lines.extend(self.content.merchant.catalog.iter().map(|decoration| {
            self.content.render(
                "output.shop_item",
                &[
                    ("item", decoration.name.clone()),
                    ("description", decoration.description.clone()),
                    ("cost", decoration.fruit_cost.to_string()),
                    (
                        "fruit",
                        if decoration.fruit_cost == 1 {
                            "fruit".to_string()
                        } else {
                            "fruits".to_string()
                        },
                    ),
                ],
            )
        }));
        lines.push(self.content.text("output.shop_hint").to_string());
        Ok(WorldOutput::lines(lines))
    }

    fn buy_decoration(
        &mut self,
        actor_id: ActorId,
        decoration_target: &str,
    ) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let merchant = self.require_merchant_here(&actor)?;
        let decoration = find_catalog_decoration(&self.content, decoration_target)
            .cloned()
            .ok_or_else(|| {
                WorldError::Message(self.content.render(
                    "error.unknown_decoration",
                    &[("item", decoration_target.to_string())],
                ))
            })?;
        let mut fruit_indices = actor
            .inventory
            .iter()
            .enumerate()
            .filter_map(|(index, item)| (item.kind == ItemKind::Produce).then_some(index))
            .take(decoration.fruit_cost)
            .collect::<Vec<_>>();
        if fruit_indices.len() < decoration.fruit_cost {
            return Err(WorldError::Message(self.content.render(
                "error.not_enough_fruit",
                &[
                    ("item", decoration.name.clone()),
                    ("cost", decoration.fruit_cost.to_string()),
                    ("have", fruit_indices.len().to_string()),
                ],
            )));
        }
        fruit_indices.reverse();
        for index in fruit_indices {
            actor.inventory.remove(index);
        }

        let mut meta = self.meta();
        let item = allocate_item(&mut meta, ItemKind::Decoration, &decoration.name);
        actor.inventory.push(item);
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Trading,
            Some(actor.id),
            Some(actor.current_room_id),
            None,
            self.room_recipients(actor.current_room_id, &[merchant.id]),
            self.content.render(
                "event.buys_decoration",
                &[
                    ("actor", actor.name.clone()),
                    ("merchant", merchant.name.clone()),
                    ("item", decoration.name.clone()),
                    ("cost", decoration.fruit_cost.to_string()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor.id),
                &WorldRecord::Actor(actor.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![
                self.content.render(
                    "output.buy_decoration",
                    &[
                        ("merchant", merchant.name),
                        ("item", decoration.name),
                        ("cost", decoration.fruit_cost.to_string()),
                    ],
                ),
                self.content.text("output.buy_hint").to_string(),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn place_decoration(
        &mut self,
        actor_id: ActorId,
        decoration_target: &str,
        position: GardenPosition,
    ) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let room = self.require_room(actor.current_room_id)?;
        let garden_id = room.garden_id.ok_or_else(|| {
            WorldError::Message(self.content.text("error.nowhere_to_decorate").to_string())
        })?;
        let mut garden = self.garden(garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.garden_no_keeper").to_string())
        })?;
        self.require_tending_permission(&actor, &garden)?;
        if self
            .plants_in_room(room.id)
            .iter()
            .any(|plant| plant.position == position)
            || garden
                .decorations
                .iter()
                .any(|decoration| decoration.position == position)
        {
            return Err(WorldError::Message(self.content.render(
                "error.position_occupied",
                &[("position", position.to_string())],
            )));
        }
        let item_index =
            find_inventory_item_index(&actor, decoration_target, Some(ItemKind::Decoration))
                .ok_or_else(|| {
                    WorldError::Message(self.content.render(
                        "error.decoration_not_carried",
                        &[("item", decoration_target.to_string())],
                    ))
                })?;
        let item = actor.inventory.remove(item_index);
        let definition = find_catalog_decoration(&self.content, &item.species)
            .cloned()
            .ok_or_else(|| {
                WorldError::Message(self.content.render(
                    "error.unknown_decoration",
                    &[("item", item.species.clone())],
                ))
            })?;
        let decoration = DecorationState {
            id: item.id,
            name: definition.name,
            description: definition.description,
            symbol: definition.symbol,
            position,
            placed_by_actor_id: actor.id,
        };
        garden.decorations.push(decoration.clone());
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Decorating,
            Some(actor.id),
            Some(room.id),
            None,
            self.room_recipients(room.id, &[garden.owner_actor_id]),
            self.content.render(
                "event.places_decoration",
                &[
                    ("actor", actor.name.clone()),
                    ("item", decoration.name.clone()),
                    ("position", position.to_string()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor.id),
                &WorldRecord::Actor(actor.clone()),
            );
            tx.upsert(
                &EntityKey::Garden(garden.id),
                &WorldRecord::Garden(garden.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![self.content.render(
                "output.place_decoration",
                &[
                    ("item", decoration.name),
                    ("position", position.to_string()),
                ],
            )],
            events: vec![event],
            quit: false,
        })
    }

    fn take_decoration(
        &mut self,
        actor_id: ActorId,
        target: &str,
    ) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let room = self.require_room(actor.current_room_id)?;
        let garden_id = room.garden_id.ok_or_else(|| {
            WorldError::Message(self.content.text("error.nowhere_to_decorate").to_string())
        })?;
        let mut garden = self.garden(garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.garden_no_keeper").to_string())
        })?;
        let index = find_decoration_index(&garden, target).ok_or_else(|| {
            WorldError::Message(
                self.content
                    .render("error.not_visible", &[("target", target.to_string())]),
            )
        })?;
        let decoration = garden.decorations[index].clone();
        if decoration.placed_by_actor_id != actor.id && garden.owner_actor_id != actor.id {
            return Err(WorldError::Message(
                self.content
                    .text("error.decoration_remove_permission")
                    .to_string(),
            ));
        }
        garden.decorations.remove(index);
        actor.inventory.push(InventoryItem {
            id: decoration.id,
            kind: ItemKind::Decoration,
            species: decoration.name.clone(),
        });
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Decorating,
            Some(actor.id),
            Some(room.id),
            None,
            self.room_recipients(room.id, &[garden.owner_actor_id]),
            self.content.render(
                "event.takes_decoration",
                &[
                    ("actor", actor.name.clone()),
                    ("item", decoration.name.clone()),
                    ("position", decoration.position.to_string()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor.id),
                &WorldRecord::Actor(actor.clone()),
            );
            tx.upsert(
                &EntityKey::Garden(garden.id),
                &WorldRecord::Garden(garden.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![
                self.content
                    .render("output.take_decoration", &[("item", decoration.name)]),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn require_merchant_here(&self, actor: &ActorState) -> Result<ActorState, WorldError> {
        let merchant = self
            .actor_by_name(&self.content.merchant.name)
            .ok_or_else(|| {
                WorldError::Message(self.content.text("error.merchant_missing").to_string())
            })?;
        if merchant.current_room_id != actor.current_room_id {
            return Err(WorldError::Message(
                self.content
                    .render("error.merchant_not_here", &[("merchant", merchant.name)]),
            ));
        }
        Ok(merchant)
    }

    fn offer(
        &mut self,
        actor_id: ActorId,
        item_target: &str,
        recipient_name: &str,
    ) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let mut recipient = self.actor_by_name(recipient_name).ok_or_else(|| {
            WorldError::Message(self.content.render(
                "error.unknown_person",
                &[("name", recipient_name.to_string())],
            ))
        })?;
        if recipient.id == actor.id {
            return Err(WorldError::Message(
                self.content.text("error.offer_self").to_string(),
            ));
        }
        if recipient.current_room_id != actor.current_room_id {
            return Err(WorldError::Message(self.content.render(
                "error.person_not_here",
                &[("name", recipient.name.clone())],
            )));
        }
        let item_index = find_inventory_item_index(&actor, item_target, None).ok_or_else(|| {
            WorldError::Message(self.content.render(
                "error.item_not_carried",
                &[("item", item_target.to_string())],
            ))
        })?;
        let item = actor.inventory.remove(item_index);
        recipient.inventory.push(item.clone());
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Trading,
            Some(actor_id),
            Some(actor.current_room_id),
            None,
            vec![recipient.id],
            self.content.render(
                "event.offers",
                &[
                    ("actor", actor.name.clone()),
                    ("recipient", recipient.name.clone()),
                    ("item", item.display_name()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor.id),
                &WorldRecord::Actor(actor.clone()),
            );
            tx.upsert(
                &EntityKey::Actor(recipient.id),
                &WorldRecord::Actor(recipient.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![self.content.render(
                "output.offer",
                &[
                    ("recipient", recipient.name.clone()),
                    ("item", item.display_name()),
                ],
            )],
            events: vec![event],
            quit: false,
        })
    }

    fn set_permission(
        &mut self,
        actor_id: ActorId,
        target_name: &str,
        action: &str,
        allow: bool,
    ) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let target = self.actor_by_name(target_name).ok_or_else(|| {
            WorldError::Message(
                self.content
                    .render("error.unknown_person", &[("name", target_name.to_string())]),
            )
        })?;
        let room = self.require_room(actor.current_room_id)?;
        let garden_id = room.garden_id.ok_or_else(|| {
            WorldError::Message(
                self.content
                    .text("error.permissions_outside_garden")
                    .to_string(),
            )
        })?;
        let mut garden = self.garden(garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.garden_no_keeper").to_string())
        })?;
        if garden.owner_actor_id != actor_id {
            return Err(WorldError::Message(
                self.content
                    .text("error.permissions_owner_only")
                    .to_string(),
            ));
        }
        let action = action.to_ascii_lowercase();
        let (list, label) = if action.contains("harvest") {
            (&mut garden.allowed_harvesters, "harvest")
        } else if action.contains("tend") || action.contains("water") || action.contains("prune") {
            (&mut garden.allowed_tenders, "tend")
        } else {
            return Err(WorldError::Message(
                self.content.text("error.permission_action").to_string(),
            ));
        };
        if allow {
            if !list.contains(&target.id) {
                list.push(target.id);
            }
        } else {
            list.retain(|id| *id != target.id);
        }
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Permission,
            Some(actor_id),
            Some(room.id),
            None,
            vec![target.id],
            self.content.render(
                if allow {
                    "event.permission_allow"
                } else {
                    "event.permission_forbid"
                },
                &[
                    ("actor", actor.name.clone()),
                    ("target", target.name.clone()),
                    ("action", label.to_string()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Garden(garden.id),
                &WorldRecord::Garden(garden.clone()),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        let wording = if allow {
            self.content.render(
                "output.permission_allow",
                &[
                    ("target", target.name.clone()),
                    ("action", label.to_string()),
                ],
            )
        } else {
            self.content.render(
                "output.permission_forbid",
                &[
                    ("target", target.name.clone()),
                    ("action", label.to_string()),
                ],
            )
        };
        Ok(WorldOutput {
            lines: vec![wording],
            events: vec![event],
            quit: false,
        })
    }

    fn list_gardens(&self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        self.require_actor(actor_id)?;
        let mut homes = self
            .actors()
            .into_iter()
            .filter_map(|owner| {
                let garden = self.garden(owner.home_garden_id)?;
                self.gate_for_garden(&garden)?;
                Some(owner)
            })
            .collect::<Vec<_>>();
        homes.sort_by(|left, right| left.name.cmp(&right.name));

        let mut lines = vec![self.content.text("output.gardens_heading").to_string()];
        lines.extend(homes.into_iter().map(|owner| {
            let marker = if owner.id == actor_id {
                self.content.text("output.garden_yours")
            } else {
                ""
            };
            self.content.render(
                "output.garden_listing",
                &[("owner", owner.name), ("marker", marker.to_string())],
            )
        }));
        lines.push(self.content.text("output.gardens_hint").to_string());
        Ok(WorldOutput::lines(lines))
    }

    fn set_home_garden_lock(
        &mut self,
        actor_id: ActorId,
        locked: bool,
    ) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let garden = self.garden(actor.home_garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.home_missing").to_string())
        })?;
        let unlocked = !locked;
        if self.garden_is_unlocked(garden.id) == unlocked {
            return Ok(WorldOutput::lines([self
                .content
                .text(if locked {
                    "output.garden_already_locked"
                } else {
                    "output.garden_already_unlocked"
                })
                .to_string()]));
        }

        let access = GardenAccessState {
            garden_id: garden.id,
            unlocked,
        };
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Permission,
            Some(actor_id),
            Some(garden.room_id),
            None,
            self.room_recipients(garden.room_id, &[actor_id]),
            self.content.render(
                if locked {
                    "event.garden_locked"
                } else {
                    "event.garden_unlocked"
                },
                &[("actor", actor.name)],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::GardenAccess(garden.id),
                &WorldRecord::GardenAccess(access),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });

        Ok(WorldOutput {
            lines: vec![
                self.content
                    .text(if locked {
                        "output.garden_locked"
                    } else {
                        "output.garden_unlocked"
                    })
                    .to_string(),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn admit_at_home_gate(
        &mut self,
        actor_id: ActorId,
        target_name: &str,
    ) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let garden = self.garden(actor.home_garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.home_missing").to_string())
        })?;
        let gate = self.gate_for_garden(&garden).ok_or_else(|| {
            WorldError::Message(self.content.text("error.gate_unlinked").to_string())
        })?;
        let target = self.actor_by_name(target_name).ok_or_else(|| {
            WorldError::Message(
                self.content
                    .render("error.unknown_person", &[("name", target_name.to_string())]),
            )
        })?;
        if target.current_room_id != gate.id {
            return Err(WorldError::Message(
                self.content
                    .render("error.admit_not_waiting", &[("name", target.name)]),
            ));
        }
        if self.has_garden_admission(garden.id, target.id) {
            return Ok(WorldOutput::lines([self.content.render(
                "output.garden_already_admitted",
                &[("target", target.name)],
            )]));
        }

        let admission = GardenAdmissionState {
            garden_id: garden.id,
            actor_id: target.id,
        };
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Permission,
            Some(actor_id),
            Some(gate.id),
            None,
            self.room_recipients(gate.id, &[actor_id]),
            self.content.render(
                "event.garden_admits",
                &[("actor", actor.name), ("target", target.name.clone())],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::GardenAdmission(garden.id, target.id),
                &WorldRecord::GardenAdmission(admission),
            );
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });

        Ok(WorldOutput {
            lines: vec![
                self.content
                    .render("output.garden_admit", &[("target", target.name)]),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn enter_garden(&mut self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let gate = self.require_room(actor.current_room_id)?;
        if gate.kind != RoomKind::GardenGate {
            return Err(WorldError::Message(
                self.content.text("error.enter_not_at_gate").to_string(),
            ));
        }
        let (garden, destination) = self.garden_at_gate(&gate).ok_or_else(|| {
            WorldError::Message(self.content.text("error.gate_unlinked").to_string())
        })?;
        let admitted = self.has_garden_admission(garden.id, actor_id);
        if !self.may_enter_garden(&actor, &garden) {
            let owner_name = self
                .actor(garden.owner_actor_id)
                .map_or_else(|| garden.name.clone(), |owner| owner.name);
            return Err(WorldError::Message(
                self.content
                    .render("error.garden_entry_denied", &[("owner", owner_name)]),
            ));
        }

        let mut meta = self.meta();
        let departure = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Departure,
            Some(actor_id),
            Some(gate.id),
            None,
            self.room_recipients(gate.id, &[]),
            self.content.render(
                "event.enters_from_gate",
                &[
                    ("actor", actor.name.clone()),
                    ("garden", garden.name.clone()),
                ],
            ),
        );
        let arrival = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Arrival,
            Some(actor_id),
            Some(destination.id),
            None,
            self.room_recipients(destination.id, &[garden.owner_actor_id]),
            self.content.render(
                "event.arrives_in_garden",
                &[
                    ("actor", actor.name.clone()),
                    ("garden", garden.name.clone()),
                ],
            ),
        );
        actor.current_room_id = destination.id;
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::Actor(actor_id), &WorldRecord::Actor(actor));
            if admitted {
                tx.remove(&EntityKey::GardenAdmission(garden.id, actor_id));
            }
            for event in [&departure, &arrival] {
                tx.upsert(
                    &EntityKey::Event(event.id),
                    &WorldRecord::Event(event.clone()),
                );
            }
        });
        let mut output = self.look(actor_id, None)?;
        output.lines.insert(
            0,
            self.content
                .render("output.enter_garden", &[("garden", garden.name)]),
        );
        output.events = vec![departure, arrival];
        Ok(output)
    }

    fn knock(&mut self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let gate = self.require_room(actor.current_room_id)?;
        if gate.kind != RoomKind::GardenGate {
            return Err(WorldError::Message(
                self.content.text("error.knock_not_at_gate").to_string(),
            ));
        }
        let (garden, _) = self.garden_at_gate(&gate).ok_or_else(|| {
            WorldError::Message(self.content.text("error.gate_unlinked").to_string())
        })?;
        let owner_name = self
            .actor(garden.owner_actor_id)
            .map_or_else(|| garden.name.clone(), |owner| owner.name);
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Knocking,
            Some(actor_id),
            Some(gate.id),
            None,
            self.room_recipients(gate.id, &[garden.owner_actor_id]),
            self.content.render(
                "event.knocks",
                &[("actor", actor.name), ("owner", owner_name.clone())],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![
                self.content
                    .render("output.knock", &[("owner", owner_name)]),
            ],
            events: vec![event],
            quit: false,
        })
    }

    fn visit(&mut self, actor_id: ActorId, target_name: &str) -> Result<WorldOutput, WorldError> {
        let target = self.actor_by_name(target_name).ok_or_else(|| {
            WorldError::Message(
                self.content
                    .render("error.unknown_person", &[("name", target_name.to_string())]),
            )
        })?;
        let destination = self.garden(target.home_garden_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.their_home_missing").to_string())
        })?;
        let destination_gate = self.gate_for_garden(&destination).ok_or_else(|| {
            WorldError::Message(self.content.text("error.gate_unlinked").to_string())
        })?;
        let mut actor = self.require_actor(actor_id)?;
        if actor.current_room_id == destination_gate.id {
            return self.look(actor_id, None);
        }
        let current = self.require_room(actor.current_room_id)?;
        let mut meta = self.meta();
        let departure = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Departure,
            Some(actor_id),
            Some(current.id),
            None,
            self.room_recipients(current.id, &[]),
            self.content.render(
                "event.leaves_to_visit",
                &[
                    ("actor", actor.name.clone()),
                    ("target", target.name.clone()),
                ],
            ),
        );
        let arrival = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Arrival,
            Some(actor_id),
            Some(destination_gate.id),
            None,
            self.room_recipients(destination_gate.id, &[target.id]),
            self.content.render(
                "event.arrives_at_gate",
                &[
                    ("actor", actor.name.clone()),
                    ("target", target.name.clone()),
                ],
            ),
        );
        actor.current_room_id = destination_gate.id;
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Actor(actor_id),
                &WorldRecord::Actor(actor.clone()),
            );
            for event in [&departure, &arrival] {
                tx.upsert(
                    &EntityKey::Event(event.id),
                    &WorldRecord::Event(event.clone()),
                );
            }
        });
        let mut output = self.look(actor_id, None)?;
        output.lines.insert(
            0,
            self.content
                .render("output.visit", &[("destination", destination_gate.name)]),
        );
        output.events = vec![departure, arrival];
        Ok(output)
    }

    fn say(&mut self, actor_id: ActorId, body: &str) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let recipients = self
            .actors_in_room(actor.current_room_id)
            .into_iter()
            .map(|actor| actor.id)
            .collect();
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::Speech,
            Some(actor_id),
            Some(actor.current_room_id),
            None,
            recipients,
            self.content.render(
                "event.says",
                &[
                    ("actor", actor.name.clone()),
                    ("body", body.trim().to_string()),
                ],
            ),
        );
        let muffled_event = self.room(actor.current_room_id).and_then(|gate| {
            if gate.kind != RoomKind::GardenGate {
                return None;
            }
            let (_, garden_room) = self.garden_at_gate(&gate)?;
            let recipients = self.room_recipients(garden_room.id, &[]);
            (!recipients.is_empty()).then(|| {
                allocate_event(
                    &mut meta,
                    self.clock().now,
                    EventKind::Speech,
                    Some(actor_id),
                    Some(garden_room.id),
                    None,
                    recipients,
                    self.content.render(
                        "event.says_muffled_at_gate",
                        &[("actor", actor.name.clone())],
                    ),
                )
            })
        });
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
            if let Some(muffled_event) = &muffled_event {
                tx.upsert(
                    &EntityKey::Event(muffled_event.id),
                    &WorldRecord::Event(muffled_event.clone()),
                );
            }
        });
        let mut events = vec![event];
        events.extend(muffled_event);
        Ok(WorldOutput {
            lines: vec![
                self.content
                    .render("output.say", &[("body", body.trim().to_string())]),
            ],
            events,
            quit: false,
        })
    }

    fn weather(&self) -> WorldOutput {
        let clock = self.clock();
        WorldOutput::lines([
            self.content.render(
                "output.weather_heading",
                &[
                    ("season", title_case(&clock.season.to_string())),
                    ("hour", clock.now.to_string()),
                    ("temperature", clock.temperature_c.to_string()),
                ],
            ),
            self.content
                .render("output.weather", &[("weather", clock.weather.to_string())]),
        ])
    }

    fn bog_overview(&self) -> Result<WorldOutput, WorldError> {
        let meta = self.bog_meta().ok_or_else(|| {
            WorldError::Message(self.content.text("error.bog_not_established").to_string())
        })?;
        let (cell_count, organism_count, stressed_count, mut species, p10, p50, p90) =
            self.stream.rtx(
                |(
                    _,
                    _,
                    _,
                    _,
                    _,
                    _,
                    _,
                    _,
                    (cells, organisms, _, _, stressed, _, species, moisture),
                )| {
                    (
                        cells.iter().count(),
                        organisms.iter().count(),
                        stressed.iter().count(),
                        species.iter().collect::<Vec<_>>(),
                        moisture.quantile(0.1),
                        moisture.quantile(0.5),
                        moisture.quantile(0.9),
                    )
                },
            );
        species.sort_by(|left, right| {
            right
                .1
                .count
                .cmp(&left.1.count)
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut lines = vec![
            self.content.text("output.bog_heading").to_string(),
            self.content.render(
                "output.bog_size",
                &[
                    ("width", meta.edge_length.to_string()),
                    ("height", meta.edge_length.to_string()),
                    ("cells", cell_count.to_string()),
                    ("organisms", organism_count.to_string()),
                ],
            ),
            self.content.render(
                "output.bog_moisture",
                &[
                    ("dry", p10.unwrap_or(0).to_string()),
                    ("median", p50.unwrap_or(0).to_string()),
                    ("wet", p90.unwrap_or(0).to_string()),
                ],
            ),
            self.content.render(
                "output.bog_stress",
                &[("count", stressed_count.to_string())],
            ),
            String::new(),
            self.content.text("output.bog_species_heading").to_string(),
        ];
        for (name, stats) in species.into_iter().take(8) {
            let mean_health = if stats.count > 0 {
                stats.health_total / stats.count
            } else {
                0
            };
            lines.push(self.content.render(
                "output.bog_species",
                &[
                    ("species", name),
                    ("count", stats.count.to_string()),
                    ("health", mean_health.to_string()),
                    ("flowering", stats.flowering.to_string()),
                ],
            ));
        }
        lines.push(String::new());
        lines.push(self.content.render(
            "output.bog_survey_hint",
            &[("maximum", (meta.edge_length - 1).to_string())],
        ));
        Ok(WorldOutput::lines(lines))
    }

    fn survey_bog(
        &self,
        actor_id: ActorId,
        position: Option<(u16, u16)>,
    ) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        let meta = self.bog_meta().ok_or_else(|| {
            WorldError::Message(self.content.text("error.bog_not_established").to_string())
        })?;
        let rooms = self.rooms();
        let room_positions = ecology::room_grid_positions(meta.edge_length, &rooms);
        let (x, y) = position.unwrap_or_else(|| {
            room_positions
                .get(&actor.current_room_id)
                .copied()
                .unwrap_or((meta.edge_length / 2, meta.edge_length / 2))
        });
        let cell_id = ecology::cell_id(meta.edge_length, x, y).ok_or_else(|| {
            WorldError::Message(self.content.render(
                "error.bog_coordinate_range",
                &[("maximum", (meta.edge_length - 1).to_string())],
            ))
        })?;
        let cell = self.bog_cell(cell_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.bog_cell_missing").to_string())
        })?;
        let cell_room_id =
            ecology::room_for_cell(meta.edge_length, &rooms, x, y).ok_or_else(|| {
                WorldError::Message(self.content.text("error.room_missing").to_string())
            })?;
        if cell_room_id != actor.current_room_id {
            return Err(WorldError::Message(
                self.content.text("error.world_cell_location").to_string(),
            ));
        }
        let room = self.require_room(cell_room_id)?;
        let organisms = self.bog_organisms_in_cell(cell_id);
        let mut counts = BTreeMap::<String, (usize, i64)>::new();
        let mut stages = BTreeSet::new();
        for organism in &organisms {
            let entry = counts.entry(organism.species.clone()).or_default();
            entry.0 += 1;
            entry.1 += i64::from(organism.health);
            stages.insert(organism.stage.to_string());
        }
        let ph_whole = cell.ph_cent / 100;
        let ph_fraction = cell.ph_cent % 100;
        let mut lines = vec![
            self.content.render(
                "output.bog_cell_heading",
                &[
                    ("room", room.name),
                    ("x", x.to_string()),
                    ("y", y.to_string()),
                ],
            ),
            self.content.render(
                "output.bog_cell_water",
                &[
                    ("water_table", cell.water_table_mm.to_string()),
                    ("moisture", cell.moisture.to_string()),
                    ("ph_whole", ph_whole.to_string()),
                    ("ph_fraction", format!("{ph_fraction:02}")),
                ],
            ),
            self.content.render(
                "output.bog_cell_conditions",
                &[
                    ("temperature", cell.temperature_c.to_string()),
                    ("light", cell.light.to_string()),
                    ("nutrients", cell.nutrients.to_string()),
                    ("peat", cell.peat_depth_mm.to_string()),
                    ("shrubs", cell.shrub_cover.to_string()),
                ],
            ),
            self.content.render(
                "output.bog_cell_organisms",
                &[
                    ("count", organisms.len().to_string()),
                    (
                        "stages",
                        if stages.is_empty() {
                            self.content.text("state.none").to_string()
                        } else {
                            stages.into_iter().collect::<Vec<_>>().join(", ")
                        },
                    ),
                ],
            ),
        ];
        for (species, (count, health_total)) in counts {
            lines.push(self.content.render(
                "output.bog_cell_species",
                &[
                    ("species", species),
                    ("count", count.to_string()),
                    ("health", (health_total / count as i64).to_string()),
                ],
            ));
        }
        if organisms.is_empty() {
            lines.push(self.content.text("output.bog_cell_empty").to_string());
        }
        Ok(WorldOutput::lines(lines))
    }

    fn restore_bog_cell(
        &mut self,
        actor_id: ActorId,
        x: u16,
        y: u16,
    ) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        if !actor.capabilities.contains(&Capability::TendSharedGarden)
            && !actor.capabilities.contains(&Capability::HelpGardeners)
        {
            return Err(WorldError::Message(
                self.content
                    .text("error.bog_restore_permission")
                    .to_string(),
            ));
        }
        let meta_bog = self.bog_meta().ok_or_else(|| {
            WorldError::Message(self.content.text("error.bog_not_established").to_string())
        })?;
        let cell_id = ecology::cell_id(meta_bog.edge_length, x, y).ok_or_else(|| {
            WorldError::Message(self.content.render(
                "error.bog_coordinate_range",
                &[("maximum", (meta_bog.edge_length - 1).to_string())],
            ))
        })?;
        let mut cell = self.bog_cell(cell_id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.bog_cell_missing").to_string())
        })?;
        let rooms = self.rooms();
        let cell_room_id =
            ecology::room_for_cell(meta_bog.edge_length, &rooms, x, y).ok_or_else(|| {
                WorldError::Message(self.content.text("error.room_missing").to_string())
            })?;
        if cell_room_id != actor.current_room_id {
            return Err(WorldError::Message(
                self.content.text("error.world_cell_location").to_string(),
            ));
        }
        cell.water_table_mm = (cell.water_table_mm + 12).min(20);
        cell.moisture = ecology::moisture_from_water_table(cell.water_table_mm);
        cell.nutrients = cell.nutrients.saturating_add(4).min(100);
        cell.shrub_cover = cell.shrub_cover.saturating_sub(5);
        cell.next_transition_at = self.clock().now + 1;

        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::System,
            Some(actor_id),
            Some(cell_room_id),
            None,
            self.room_recipients(cell_room_id, &[]),
            self.content.render(
                "event.bog_restored",
                &[
                    ("actor", actor.name.clone()),
                    ("x", x.to_string()),
                    ("y", y.to_string()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::BogCell(cell.id), &WorldRecord::BogCell(cell));
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![self.content.render(
                "output.bog_restored",
                &[("x", x.to_string()), ("y", y.to_string())],
            )],
            events: vec![event],
            quit: false,
        })
    }

    fn change_weather(
        &mut self,
        actor_id: ActorId,
        requested: &str,
    ) -> Result<WorldOutput, WorldError> {
        let actor = self.require_actor(actor_id)?;
        if !actor.capabilities.contains(&Capability::ChangeWeather) {
            return Err(WorldError::Message(
                self.content.text("error.cannot_change_weather").to_string(),
            ));
        }
        let mut clock = self.clock();
        clock.weather = parse_weather(&self.content, requested)?;
        clock.temperature_c = match clock.weather {
            Weather::Clear => 18,
            Weather::Cloudy => 16,
            Weather::LightRain => 14,
            Weather::HeavyRain => 12,
            Weather::Mist => 13,
        };
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            clock.now,
            EventKind::Weather,
            Some(actor_id),
            None,
            None,
            Vec::new(),
            self.content.render(
                "event.weather_changed",
                &[
                    ("actor", actor.name.clone()),
                    ("weather", clock.weather.to_string()),
                ],
            ),
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(&EntityKey::Clock, &WorldRecord::Clock(clock.clone()));
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        Ok(WorldOutput {
            lines: vec![self.content.render(
                "output.invoke_weather",
                &[("weather", clock.weather.to_string())],
            )],
            events: vec![event],
            quit: false,
        })
    }

    fn agent_turn(&self, actor: &ActorState) -> Result<AgentTurn, WorldError> {
        self.agent_turn_with_triggers(actor, Vec::new(), Vec::new())
    }

    fn agent_turn_with_triggers(
        &self,
        actor: &ActorState,
        triggering_speech: Vec<String>,
        triggering_knocks: Vec<String>,
    ) -> Result<AgentTurn, WorldError> {
        let profile = actor.agent.as_ref().ok_or_else(|| {
            WorldError::Message(
                self.content
                    .render("error.not_world_agent", &[("name", actor.name.clone())]),
            )
        })?;
        let room = self.require_room(actor.current_room_id)?;
        let clock = self.clock();
        let mut visible_plants = self.plants_in_room(room.id);
        visible_plants.sort_by_key(|plant| plant.id);
        let visible_people = self
            .actors_in_room(room.id)
            .into_iter()
            .filter(|other| other.id != actor.id)
            .map(|other| format!("{} ({:?})", other.name, other.kind))
            .collect();
        let (recent_events, recent_speech) = self.stream.rtx(|(_, _, _, _, events, ..)| {
            let mut relevant = events
                .iter()
                .filter_map(|(scored, _)| {
                    let event = scored.val;
                    (event.actor_id == Some(actor.id)
                        || event.room_id == Some(room.id)
                        || event.room_id.is_none())
                    .then_some(format!("hour {}: {}", event.at, event.message))
                })
                .collect::<Vec<_>>();
            if relevant.len() > 12 {
                relevant = relevant.split_off(relevant.len() - 12);
            }
            let mut own_speech = events
                .iter()
                .filter_map(|(scored, _)| {
                    let event = scored.val;
                    (event.actor_id == Some(actor.id) && event.kind == EventKind::Speech)
                        .then_some(event.message)
                })
                .collect::<Vec<_>>();
            if own_speech.len() > 4 {
                own_speech = own_speech.split_off(own_speech.len() - 4);
            }
            (relevant, own_speech)
        });
        let mut available_commands = self.content.resident_commands.clone();
        if actor.capabilities.contains(&Capability::ChangeWeather) {
            available_commands.push(
                self.content
                    .text("command.resident_invoke_weather")
                    .to_string(),
            );
        }
        let ecology = self.bog_meta().map(|meta| {
            let rooms = self.rooms();
            let (mut restoration_candidates, stressed) =
                self.stream
                    .rtx(|(_, _, _, _, _, _, _, _, (cells, _, _, _, stressed, ..))| {
                        (
                            cells
                                .iter()
                                .map(|(_, cell)| cell)
                                .filter(|cell| {
                                    ecology::room_for_cell(meta.edge_length, &rooms, cell.x, cell.y)
                                        == Some(room.id)
                                })
                                .collect::<Vec<_>>(),
                            stressed
                                .iter()
                                .map(|(_, organism)| organism)
                                .collect::<Vec<_>>(),
                        )
                    });
            let region_cell_ids = restoration_candidates
                .iter()
                .map(|cell| cell.id)
                .collect::<BTreeSet<_>>();
            let stressed_organisms = stressed
                .iter()
                .filter(|organism| region_cell_ids.contains(&organism.cell_id))
                .count();
            let mut moisture = restoration_candidates
                .iter()
                .map(|cell| u64::from(cell.moisture))
                .collect::<Vec<_>>();
            moisture.sort_unstable();
            let region_quantile = |percentile: usize| {
                moisture
                    .get(moisture.len().saturating_sub(1).saturating_mul(percentile) / 100)
                    .copied()
                    .unwrap_or_default()
            };
            let moisture_p10 = region_quantile(10);
            let moisture_p50 = region_quantile(50);
            let moisture_p90 = region_quantile(90);
            restoration_candidates.sort_by(|left, right| {
                left.moisture
                    .cmp(&right.moisture)
                    .then_with(|| right.shrub_cover.cmp(&left.shrub_cover))
                    .then_with(|| left.id.cmp(&right.id))
            });
            restoration_candidates.truncate(4);
            AgentEcologyContext {
                edge_length: meta.edge_length,
                moisture_p10,
                moisture_p50,
                moisture_p90,
                stressed_organisms,
                restoration_candidates,
            }
        });
        if ecology.is_some() {
            available_commands.push(self.content.text("command.resident_bog").to_string());
            available_commands.push(self.content.text("command.resident_survey").to_string());
            if actor.capabilities.contains(&Capability::TendSharedGarden)
                || actor.capabilities.contains(&Capability::HelpGardeners)
            {
                available_commands.push(self.content.text("command.resident_restore").to_string());
            }
        }
        Ok(AgentTurn {
            actor_id: actor.id,
            npc_id: profile.npc_id.clone(),
            name: actor.name.clone(),
            kind: actor.kind.clone(),
            strategy: profile.strategy.clone(),
            goal: profile.goal.clone(),
            world_time: clock.now,
            season: clock.season,
            weather: clock.weather,
            room,
            visible_plants,
            visible_people,
            inventory: actor.inventory.clone(),
            capabilities: actor.capabilities.clone(),
            recent_events,
            recent_speech,
            triggering_speech,
            triggering_knocks,
            available_commands,
            ecology,
        })
    }

    fn record_agent_audit(&mut self, actor_id: ActorId, message: String) -> WorldEvent {
        let actor = self.actor(actor_id);
        let mut meta = self.meta();
        let event = allocate_event(
            &mut meta,
            self.clock().now,
            EventKind::AgentAction,
            Some(actor_id),
            actor.map(|actor| actor.current_room_id),
            None,
            Vec::new(),
            message,
        );
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
            tx.upsert(
                &EntityKey::Event(event.id),
                &WorldRecord::Event(event.clone()),
            );
        });
        event
    }

    fn who(&self, actor_id: ActorId) -> WorldOutput {
        let mut actors = self.actors();
        actors.sort_by(|a, b| a.name.cmp(&b.name));
        let mut lines = vec![self.content.text("output.who_heading").to_string()];
        for actor in actors {
            let location = self
                .room(actor.current_room_id)
                .map(|room| room.name)
                .unwrap_or_else(|| self.content.text("state.nowhere").to_string());
            let you = if actor.id == actor_id {
                self.content.text("output.you_marker")
            } else {
                ""
            };
            lines.push(format!("  {}{} — {}", actor.name, you, location));
        }
        WorldOutput::lines(lines)
    }

    fn changes(&mut self, actor_id: ActorId) -> Result<WorldOutput, WorldError> {
        let mut actor = self.require_actor(actor_id)?;
        let latest = self.meta().next_event_id.saturating_sub(1);
        let mut events = self.stream.rtx(|(_, _, _, _, events, ..)| {
            events
                .iter()
                .filter_map(|(scored, _)| {
                    let event = scored.val;
                    (event.id.0 > actor.last_seen_event_id.0
                        && (event.actor_id == Some(actor_id)
                            || event.room_id.is_none()
                            || event.recipients.contains(&actor_id)))
                    .then_some(event)
                })
                .collect::<Vec<_>>()
        });
        if let Some(latest_weather) = events
            .iter()
            .filter(|event| event.kind == EventKind::Weather)
            .map(|event| event.id)
            .max()
        {
            events.retain(|event| event.kind != EventKind::Weather || event.id == latest_weather);
        }
        actor.last_seen_event_id = EventId(latest);
        self.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Actor(actor_id), &WorldRecord::Actor(actor));
        });
        if events.is_empty() {
            return Ok(WorldOutput::lines([self.content.text("output.help_none")]));
        }
        let mut lines = vec![self.content.text("output.changes_heading").to_string()];
        if events.len() > 24 {
            lines.push(self.content.render(
                "output.earlier_changes",
                &[("count", events.len().saturating_sub(24).to_string())],
            ));
            events = events.split_off(events.len() - 24);
        }
        lines.extend(
            events
                .into_iter()
                .map(|event| format!("  {}", event.message)),
        );
        Ok(WorldOutput::lines(lines))
    }

    fn require_actor(&self, id: ActorId) -> Result<ActorState, WorldError> {
        self.actor(id).ok_or_else(|| {
            WorldError::Message(self.content.text("error.actor_missing").to_string())
        })
    }

    fn actor_by_name(&self, name: &str) -> Option<ActorState> {
        let target = name.trim();
        self.actors()
            .into_iter()
            .find(|actor| actor.name.eq_ignore_ascii_case(target))
    }

    fn require_room(&self, id: RoomId) -> Result<RoomState, WorldError> {
        self.room(id)
            .ok_or_else(|| WorldError::Message(self.content.text("error.room_missing").to_string()))
    }

    fn require_tending_permission(
        &self,
        actor: &ActorState,
        garden: &GardenState,
    ) -> Result<(), WorldError> {
        if actor.id == garden.owner_actor_id
            || garden.allowed_tenders.contains(&actor.id)
            || (garden.kind == GardenKind::Common
                && actor.capabilities.contains(&Capability::TendSharedGarden))
        {
            Ok(())
        } else {
            Err(WorldError::Message(
                self.content.text("error.no_tend_permission").to_string(),
            ))
        }
    }

    fn may_enter_garden(&self, actor: &ActorState, garden: &GardenState) -> bool {
        self.garden_is_unlocked(garden.id)
            || self.has_garden_admission(garden.id, actor.id)
            || actor.id == garden.owner_actor_id
            || garden.allowed_tenders.contains(&actor.id)
            || garden.allowed_harvesters.contains(&actor.id)
            || actor.capabilities.contains(&Capability::EnterPrivateGarden)
    }

    fn require_harvest_permission(
        &self,
        actor: &ActorState,
        garden: &GardenState,
    ) -> Result<(), WorldError> {
        if actor.id == garden.owner_actor_id
            || garden.allowed_harvesters.contains(&actor.id)
            || (garden.kind == GardenKind::Common
                && actor.capabilities.contains(&Capability::TendSharedGarden))
        {
            Ok(())
        } else {
            Err(WorldError::Message(
                self.content.text("error.no_harvest_permission").to_string(),
            ))
        }
    }
}

fn pipeline() -> WorldPipeline {
    (
        FilterMap::new(
            extract_actor as fn(&RootRecord) -> Option<ActorRow>,
            terminal::Table::new("actors"),
        ),
        FilterMap::new(
            extract_garden as fn(&RootRecord) -> Option<GardenRow>,
            terminal::Table::new("gardens"),
        ),
        FilterMap::new(
            extract_room as fn(&RootRecord) -> Option<RoomRow>,
            terminal::Table::new("rooms"),
        ),
        FilterMap::new(
            extract_plant_row as fn(&RootRecord) -> Option<PlantRow>,
            terminal::Table::new("plants"),
        ),
        FilterMap::new(
            extract_event as fn(&RootRecord) -> Option<WorldEvent>,
            ScoreBy::new(
                event_score as fn(&WorldEvent) -> u64,
                terminal::Ranked::new("events_by_time"),
            ),
        ),
        FilterMap::new(
            extract_plant as fn(&RootRecord) -> Option<PlantState>,
            ScoreBy::new(
                plant_schedule_score as fn(&PlantState) -> u64,
                terminal::Ranked::new("plants_by_next_transition"),
            ),
        ),
        FilterMap::new(
            extract_plant_row as fn(&RootRecord) -> Option<PlantRow>,
            Filter::new(
                needs_water as fn(&PlantRow) -> bool,
                terminal::Table::new("plants_needing_water"),
            ),
        ),
        FilterMap::new(
            extract_agent as fn(&RootRecord) -> Option<ActorState>,
            ScoreBy::new(
                agent_wake_score as fn(&ActorState) -> u64,
                terminal::Ranked::new("agents_by_next_wake"),
            ),
        ),
        (
            FilterMap::new(
                extract_bog_cell_row as fn(&RootRecord) -> Option<BogCellRow>,
                terminal::Table::new("bog_cells"),
            ),
            FilterMap::new(
                extract_bog_organism_row as fn(&RootRecord) -> Option<BogOrganismRow>,
                terminal::Table::new("bog_organisms"),
            ),
            FilterMap::new(
                extract_bog_cell as fn(&RootRecord) -> Option<BogCellState>,
                ScoreBy::new(
                    bog_cell_schedule_score as fn(&BogCellState) -> u64,
                    terminal::Ranked::new("bog_cells_by_next_transition"),
                ),
            ),
            FilterMap::new(
                extract_bog_organism as fn(&RootRecord) -> Option<BogOrganismState>,
                ScoreBy::new(
                    bog_organism_schedule_score as fn(&BogOrganismState) -> u64,
                    terminal::Ranked::new("bog_organisms_by_next_transition"),
                ),
            ),
            FilterMap::new(
                extract_bog_organism_row as fn(&RootRecord) -> Option<BogOrganismRow>,
                Filter::new(
                    bog_organism_is_stressed as fn(&BogOrganismRow) -> bool,
                    terminal::Table::new("stressed_bog_organisms"),
                ),
            ),
            FilterMap::new(
                extract_bog_organism_by_cell as fn(&RootRecord) -> Option<BogOrganismByCellRow>,
                terminal::Multimap::new("bog_organisms_by_cell"),
            ),
            FilterMap::new(
                extract_bog_species_row as fn(&RootRecord) -> Option<BogSpeciesRow>,
                Aggregate::new(
                    "bog_species_aggregate",
                    aggregate_bog_species as fn(&mut BogSpeciesStats, &BogOrganismState, isize),
                    terminal::Table::new("bog_species_stats"),
                ),
            ),
            FilterMap::new(
                extract_bog_moisture as fn(&RootRecord) -> Option<Scored<u64, ()>>,
                terminal::Histogram::new(
                    "bog_moisture_histogram",
                    bog_moisture_bucket as fn(&u64) -> u64,
                ),
            ),
        ),
    )
}

fn extract_actor(root: &RootRecord) -> Option<ActorRow> {
    match &root.val {
        WorldRecord::Actor(actor) => Some(Keyed::new(actor.id, actor.clone())),
        _ => None,
    }
}

fn extract_garden(root: &RootRecord) -> Option<GardenRow> {
    match &root.val {
        WorldRecord::Garden(garden) => Some(Keyed::new(garden.id, garden.clone())),
        _ => None,
    }
}

fn extract_room(root: &RootRecord) -> Option<RoomRow> {
    match &root.val {
        WorldRecord::Room(room) => Some(Keyed::new(room.id, room.clone())),
        _ => None,
    }
}

fn extract_plant_row(root: &RootRecord) -> Option<PlantRow> {
    match &root.val {
        WorldRecord::Plant(plant) => Some(Keyed::new(plant.id, plant.clone())),
        _ => None,
    }
}

fn extract_plant(root: &RootRecord) -> Option<PlantState> {
    match &root.val {
        WorldRecord::Plant(plant) => Some(plant.clone()),
        _ => None,
    }
}

fn extract_event(root: &RootRecord) -> Option<WorldEvent> {
    match &root.val {
        WorldRecord::Event(event) => Some(event.clone()),
        _ => None,
    }
}

fn extract_agent(root: &RootRecord) -> Option<ActorState> {
    match &root.val {
        WorldRecord::Actor(actor) if actor.agent.as_ref().is_some_and(|agent| agent.enabled) => {
            Some(actor.clone())
        }
        _ => None,
    }
}

fn extract_bog_cell_row(root: &RootRecord) -> Option<BogCellRow> {
    match &root.val {
        WorldRecord::BogCell(cell) => Some(Keyed::new(cell.id, cell.clone())),
        _ => None,
    }
}

fn extract_bog_cell(root: &RootRecord) -> Option<BogCellState> {
    match &root.val {
        WorldRecord::BogCell(cell) => Some(cell.clone()),
        _ => None,
    }
}

fn extract_bog_organism_row(root: &RootRecord) -> Option<BogOrganismRow> {
    match &root.val {
        WorldRecord::BogOrganism(organism) => Some(Keyed::new(organism.id, organism.clone())),
        _ => None,
    }
}

fn extract_bog_organism(root: &RootRecord) -> Option<BogOrganismState> {
    match &root.val {
        WorldRecord::BogOrganism(organism) => Some(organism.clone()),
        _ => None,
    }
}

fn extract_bog_organism_by_cell(root: &RootRecord) -> Option<BogOrganismByCellRow> {
    match &root.val {
        WorldRecord::BogOrganism(organism) => Some(Keyed::new(organism.cell_id, organism.clone())),
        _ => None,
    }
}

fn extract_bog_species_row(root: &RootRecord) -> Option<BogSpeciesRow> {
    match &root.val {
        WorldRecord::BogOrganism(organism) => {
            Some(Keyed::new(organism.species.clone(), organism.clone()))
        }
        _ => None,
    }
}

fn extract_bog_moisture(root: &RootRecord) -> Option<Scored<u64, ()>> {
    match &root.val {
        WorldRecord::BogCell(cell) => Some(Scored::new(u64::from(cell.moisture), ())),
        _ => None,
    }
}

fn event_score(event: &WorldEvent) -> u64 {
    event.id.0
}

fn plant_schedule_score(plant: &PlantState) -> u64 {
    plant.next_transition_at
}

fn agent_wake_score(actor: &ActorState) -> u64 {
    actor
        .agent
        .as_ref()
        .map_or(u64::MAX, |agent| agent.next_wake_at)
}

fn bog_cell_schedule_score(cell: &BogCellState) -> u64 {
    cell.next_transition_at
}

fn bog_organism_schedule_score(organism: &BogOrganismState) -> u64 {
    organism.next_transition_at
}

fn bog_organism_is_stressed(organism: &BogOrganismRow) -> bool {
    organism.val.health < 40 || organism.val.stage == BogLifeStage::Dead
}

fn aggregate_bog_species(
    aggregate: &mut BogSpeciesStats,
    organism: &BogOrganismState,
    delta: isize,
) {
    let delta = delta as i64;
    aggregate.count += delta;
    aggregate.health_total += i64::from(organism.health) * delta;
    aggregate.biomass_total_g += i64::from(organism.biomass_g) * delta;
    if organism.stage == BogLifeStage::Flowering {
        aggregate.flowering += delta;
    }
}

fn bog_moisture_bucket(moisture: &u64) -> u64 {
    moisture / 5 * 5
}

fn needs_water(plant: &PlantRow) -> bool {
    plant.val.moisture < 35
}

fn record_meta(record: WorldRecord) -> Option<WorldMeta> {
    match record {
        WorldRecord::Meta(meta) => Some(meta),
        _ => None,
    }
}

fn record_clock(record: WorldRecord) -> Option<WorldClock> {
    match record {
        WorldRecord::Clock(clock) => Some(clock),
        _ => None,
    }
}

fn record_actor(record: WorldRecord) -> Option<ActorState> {
    match record {
        WorldRecord::Actor(actor) => Some(actor),
        _ => None,
    }
}

fn record_garden(record: WorldRecord) -> Option<GardenState> {
    match record {
        WorldRecord::Garden(garden) => Some(garden),
        _ => None,
    }
}

fn record_garden_access(record: WorldRecord) -> Option<GardenAccessState> {
    match record {
        WorldRecord::GardenAccess(access) => Some(access),
        _ => None,
    }
}

fn record_garden_admission(record: WorldRecord) -> Option<GardenAdmissionState> {
    match record {
        WorldRecord::GardenAdmission(admission) => Some(admission),
        _ => None,
    }
}

fn record_room(record: WorldRecord) -> Option<RoomState> {
    match record {
        WorldRecord::Room(room) => Some(room),
        _ => None,
    }
}

fn record_bog_meta(record: WorldRecord) -> Option<BogMeta> {
    match record {
        WorldRecord::BogMeta(meta) => Some(meta),
        _ => None,
    }
}

fn record_bog_cell(record: WorldRecord) -> Option<BogCellState> {
    match record {
        WorldRecord::BogCell(cell) => Some(cell),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn allocate_event(
    meta: &mut WorldMeta,
    at: WorldTime,
    kind: EventKind,
    actor_id: Option<ActorId>,
    room_id: Option<RoomId>,
    plant_id: Option<PlantId>,
    recipients: Vec<ActorId>,
    message: String,
) -> WorldEvent {
    let id = EventId(meta.next_event_id);
    meta.next_event_id += 1;
    WorldEvent {
        id,
        at,
        kind,
        actor_id,
        room_id,
        plant_id,
        recipients,
        message,
    }
}

fn allocate_item(meta: &mut WorldMeta, kind: ItemKind, species: &str) -> InventoryItem {
    let id = ItemId(meta.next_item_id);
    meta.next_item_id += 1;
    InventoryItem {
        id,
        kind,
        species: species.to_string(),
    }
}

fn find_inventory_item_index(
    actor: &ActorState,
    target: &str,
    required_kind: Option<ItemKind>,
) -> Option<usize> {
    let target = target.trim().trim_start_matches('#').to_ascii_lowercase();
    actor.inventory.iter().position(|item| {
        let kind_matches = required_kind
            .as_ref()
            .is_none_or(|required| required == &item.kind);
        kind_matches
            && (item.id.to_string() == target
                || item.species.to_ascii_lowercase() == target
                || item.display_name().to_ascii_lowercase() == target)
    })
}

fn find_catalog_decoration<'a>(
    content: &'a GameContent,
    target: &str,
) -> Option<&'a crate::content::DecorationDefinition> {
    let target = target
        .trim()
        .trim_start_matches('#')
        .trim_start_matches("the ")
        .to_ascii_lowercase();
    content
        .merchant
        .catalog
        .iter()
        .find(|decoration| decoration.name.to_ascii_lowercase() == target)
}

fn find_decoration_index(garden: &GardenState, target: &str) -> Option<usize> {
    let target = target.trim().trim_start_matches('#').to_ascii_lowercase();
    garden.decorations.iter().position(|decoration| {
        decoration.id.to_string() == target
            || decoration.name.to_ascii_lowercase() == target
            || decoration.position.to_string().to_ascii_lowercase() == target
    })
}

fn find_decoration<'a>(garden: &'a GardenState, target: &str) -> Option<&'a DecorationState> {
    find_decoration_index(garden, target).map(|index| &garden.decorations[index])
}

fn shared_rooms(content: &GameContent) -> Vec<RoomState> {
    [
        (GATE, RoomKind::Gate),
        (COMMON_PATH, RoomKind::CommonPath),
        (GLASSHOUSE, RoomKind::Glasshouse),
        (MOON_BED, RoomKind::MoonBed),
        (POND, RoomKind::Pond),
        (COMPOST, RoomKind::Compost),
        (WILD_EDGE, RoomKind::WildEdge),
    ]
    .into_iter()
    .map(|(id, kind)| {
        let definition = content.room(&kind);
        room(id, &definition.name, &definition.description, kind)
    })
    .collect()
}

fn shared_gardens(content: &GameContent) -> Vec<GardenState> {
    [
        (GLASSHOUSE_GARDEN, GLASSHOUSE, RoomKind::Glasshouse),
        (MOON_BED_GARDEN, MOON_BED, RoomKind::MoonBed),
        (POND_GARDEN, POND, RoomKind::Pond),
        (COMPOST_GARDEN, COMPOST, RoomKind::Compost),
        (WILD_EDGE_GARDEN, WILD_EDGE, RoomKind::WildEdge),
    ]
    .into_iter()
    .map(|(id, room_id, room_kind)| {
        let definition = content
            .world
            .gardens
            .iter()
            .find(|garden| garden.room == room_kind)
            .unwrap_or_else(|| panic!("content config is missing garden for {room_kind:?}"));
        GardenState {
            id,
            owner_actor_id: ActorId(0),
            name: definition.name.clone(),
            room_id,
            kind: GardenKind::Common,
            allowed_tenders: Vec::new(),
            allowed_harvesters: Vec::new(),
            decorations: Vec::new(),
        }
    })
    .collect()
}

fn room(id: RoomId, name: &str, description: &str, kind: RoomKind) -> RoomState {
    RoomState {
        id,
        name: name.to_string(),
        description: description.to_string(),
        kind,
        garden_id: None,
        exits: BTreeMap::new(),
    }
}

fn garden_gate_room(
    content: &GameContent,
    id: RoomId,
    home_room_id: RoomId,
    owner_name: &str,
) -> RoomState {
    RoomState {
        id,
        name: content.render(
            "world.garden_gate_name",
            &[("owner", owner_name.to_string())],
        ),
        description: content.render(
            "world.garden_gate_description",
            &[("owner", owner_name.to_string())],
        ),
        kind: RoomKind::GardenGate,
        garden_id: None,
        exits: BTreeMap::from([
            ("in".to_string(), home_room_id),
            ("out".to_string(), COMMON_PATH),
        ]),
    }
}

fn connect(rooms: &mut [RoomState], from: RoomId, direction: &str, to: RoomId) {
    rooms
        .iter_mut()
        .find(|room| room.id == from)
        .expect("shared room exists")
        .exits
        .insert(direction.to_string(), to);
}

fn normalize_name(content: &GameContent, name: &str) -> Result<String, WorldError> {
    let name = name.trim();
    if name.len() < 2 || name.len() > 24 {
        return Err(WorldError::Message(
            content.text("error.name_length").to_string(),
        ));
    }
    if !name
        .chars()
        .all(|ch| ch.is_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(WorldError::Message(
            content.text("error.name_characters").to_string(),
        ));
    }
    Ok(name.to_string())
}

fn normalize_place_name(name: &str) -> String {
    let normalized = name.trim().to_ascii_lowercase();
    normalized
        .strip_prefix("the ")
        .unwrap_or(&normalized)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn human_join(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let (last, rest) = names.split_last().expect("non-empty names");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn normalize_species(content: &GameContent, species: &str) -> Result<String, WorldError> {
    let species = species.trim().to_ascii_lowercase();
    if content.world.species.contains(&species) {
        Ok(species)
    } else {
        Err(WorldError::Message(content.render(
            "error.unknown_species",
            &[
                ("species", species),
                ("available", content.world.species.join(", ")),
            ],
        )))
    }
}

fn parse_weather(content: &GameContent, value: &str) -> Result<Weather, WorldError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "clear" | "sun" | "sunny" => Ok(Weather::Clear),
        "cloudy" | "cloud" => Ok(Weather::Cloudy),
        "light rain" | "rain" => Ok(Weather::LightRain),
        "heavy rain" | "storm" => Ok(Weather::HeavyRain),
        "mist" | "fog" => Ok(Weather::Mist),
        _ => Err(WorldError::Message(
            content.text("error.known_weather").to_string(),
        )),
    }
}

fn normalize_direction(direction: &str) -> &str {
    match direction.trim().to_ascii_lowercase().as_str() {
        "n" => "north",
        "s" => "south",
        "e" => "east",
        "w" => "west",
        "o" => "out",
        _ => {
            let normalized = direction.trim();
            if normalized.eq_ignore_ascii_case("north") {
                "north"
            } else if normalized.eq_ignore_ascii_case("south") {
                "south"
            } else if normalized.eq_ignore_ascii_case("east") {
                "east"
            } else if normalized.eq_ignore_ascii_case("west") {
                "west"
            } else if normalized.eq_ignore_ascii_case("out") {
                "out"
            } else {
                normalized
            }
        }
    }
}

fn is_direction(direction: &str) -> bool {
    matches!(direction, "north" | "south" | "east" | "west" | "out")
}

fn capabilities_for(kind: &ActorKind) -> Vec<Capability> {
    match kind {
        ActorKind::Human | ActorKind::GardenerAgent => {
            vec![Capability::TendOwnGarden, Capability::TendSharedGarden]
        }
        ActorKind::Helper => vec![
            Capability::TendOwnGarden,
            Capability::TendSharedGarden,
            Capability::HelpGardeners,
        ],
        ActorKind::Spirit => vec![Capability::TendOwnGarden],
        ActorKind::God => vec![Capability::TendOwnGarden, Capability::ChangeWeather],
    }
}

fn home_description(content: &GameContent, kind: &ActorKind) -> String {
    let descriptions = &content.world.home_descriptions;
    match kind {
        ActorKind::Human => descriptions.human.clone(),
        ActorKind::GardenerAgent => descriptions.gardener.clone(),
        ActorKind::Helper => descriptions.helper.clone(),
        ActorKind::Spirit => descriptions.spirit.clone(),
        ActorKind::God => descriptions.god.clone(),
    }
}

fn moisture_word(moisture: i16) -> &'static str {
    match moisture {
        ..=14 => "parched",
        15..=34 => "dry",
        35..=69 => "damp",
        70..=89 => "wet",
        _ => "waterlogged",
    }
}

fn stage_for(growth: i16, health: i16) -> PlantStage {
    if health <= 20 {
        PlantStage::Dormant
    } else {
        match growth {
            ..=19 => PlantStage::Seed,
            20..=39 => PlantStage::Sprout,
            40..=69 => PlantStage::Growing,
            70..=89 => PlantStage::Flowering,
            _ => PlantStage::Fruiting,
        }
    }
}

fn apply_weather_cycle(clock: &mut WorldClock) {
    clock.season = match (clock.now / 168) % 4 {
        0 => Season::Spring,
        1 => Season::Summer,
        2 => Season::Autumn,
        _ => Season::Winter,
    };
    clock.weather = match clock.now % 12 {
        0..=2 => Weather::LightRain,
        3..=5 => Weather::Cloudy,
        6..=9 => Weather::Clear,
        10 => Weather::Mist,
        _ => Weather::HeavyRain,
    };
    clock.temperature_c = match clock.weather {
        Weather::Clear => 18,
        Weather::Cloudy => 16,
        Weather::LightRain => 14,
        Weather::HeavyRain => 12,
        Weather::Mist => 13,
    };
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn render_garden_description(
    room_name: &str,
    plants: &[PlantState],
    decorations: &[DecorationState],
) -> Vec<String> {
    let mut lines = vec![room_name.to_string()];
    if plants.is_empty() && decorations.is_empty() {
        lines.push(
            "The 8×8 beds are bare: all 64 plots lie open, their soil undisturbed.".to_string(),
        );
        lines.push("Use `survey garden` to see the individual plots.".to_string());
        return lines;
    }

    let open_plots =
        usize::from(GARDEN_FILES) * usize::from(GARDEN_RANKS) - plants.len() - decorations.len();
    if plants.is_empty() {
        lines.push(format!(
            "The beds wait for planting; {} {} arranged, leaving {} {} open.",
            count_word(decorations.len()),
            if decorations.len() == 1 {
                "decoration is"
            } else {
                "decorations are"
            },
            count_word(open_plots),
            if open_plots == 1 { "plot" } else { "plots" },
        ));
    } else {
        lines.push(format!(
            "{} {}, {}; {} {} open.",
            title_case(&count_word(plants.len())),
            if plants.len() == 1 {
                "plant occupies the beds"
            } else {
                "plants occupy the beds"
            },
            garden_spatial_phrase(plants),
            count_word(open_plots),
            if open_plots == 1 {
                "plot remains"
            } else {
                "plots remain"
            },
        ));
    }

    if !plants.is_empty() {
        let mut stages = BTreeMap::<u8, (&PlantStage, usize)>::new();
        for plant in plants {
            let key = plant_stage_order(&plant.stage);
            stages
                .entry(key)
                .and_modify(|(_, count)| *count += 1)
                .or_insert((&plant.stage, 1));
        }
        lines.push(format!(
            "Growth: {}.",
            natural_join(
                stages
                    .values()
                    .map(|(stage, count)| plant_stage_count(*count, stage))
                    .collect()
            )
        ));
    }

    if !plants.is_empty() && plants.len() <= 3 {
        lines.extend(plants.iter().map(describe_visible_plant));
    } else if !plants.is_empty() {
        let mut species = BTreeMap::<&str, usize>::new();
        for plant in plants {
            *species.entry(plant.species.as_str()).or_default() += 1;
        }
        let mut species = species.into_iter().collect::<Vec<_>>();
        species.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        if species.len() == 1 {
            lines.push(format!("Every planting is {}.", species[0].0));
        } else {
            lines.push(format!(
                "{} is most common ({} of {} plants), among {} species.",
                title_case(species[0].0),
                species[0].1,
                plants.len(),
                species.len()
            ));
        }

        let most_advanced = plants
            .iter()
            .max_by_key(|plant| {
                (
                    !matches!(plant.stage, PlantStage::Dormant),
                    plant.growth,
                    plant.health,
                    std::cmp::Reverse(plant.position),
                )
            })
            .expect("a non-empty garden has a most advanced plant");
        lines.push(format!(
            "{} at {} is furthest along: {}.",
            title_case(&most_advanced.name),
            most_advanced.position,
            most_advanced.stage
        ));
    }

    if !decorations.is_empty() {
        lines.push(format!(
            "Decorations: {}.",
            natural_join(
                decorations
                    .iter()
                    .map(|decoration| format!("{} at {}", decoration.name, decoration.position))
                    .collect()
            )
        ));
    }

    if !plants.is_empty() {
        let average_moisture = plants
            .iter()
            .map(|plant| i32::from(plant.moisture))
            .sum::<i32>()
            / plants.len() as i32;
        let thirsty = plants.iter().filter(|plant| plant.moisture < 35).count();
        let struggling = plants.iter().filter(|plant| plant.health <= 30).count();
        let condition = match (thirsty, struggling) {
            (0, 0) => format!(
                "The beds feel {} overall, and the planting looks healthy.",
                moisture_word(average_moisture as i16)
            ),
            (thirsty, 0) => format!(
                "The beds feel {} overall, but {} {} thirsty.",
                moisture_word(average_moisture as i16),
                count_word(thirsty),
                if thirsty == 1 {
                    "plant looks"
                } else {
                    "plants look"
                }
            ),
            (0, struggling) => format!(
                "The beds feel {} overall, but {} {} struggling.",
                moisture_word(average_moisture as i16),
                count_word(struggling),
                if struggling == 1 {
                    "plant is"
                } else {
                    "plants are"
                }
            ),
            (thirsty, struggling) => format!(
                "The beds feel {} overall; {} {} thirsty, and {} {} struggling.",
                moisture_word(average_moisture as i16),
                count_word(thirsty),
                if thirsty == 1 {
                    "plant looks"
                } else {
                    "plants look"
                },
                count_word(struggling),
                if struggling == 1 {
                    "plant is"
                } else {
                    "plants are"
                }
            ),
        };
        lines.push(condition);
    }
    lines.push("Use `survey garden` to see the individual plots.".to_string());
    lines
}

fn garden_spatial_phrase(plants: &[PlantState]) -> String {
    let min_file = plants
        .iter()
        .map(|plant| plant.position.file())
        .min()
        .unwrap_or_default();
    let max_file = plants
        .iter()
        .map(|plant| plant.position.file())
        .max()
        .unwrap_or_default();
    let min_rank = plants
        .iter()
        .map(|plant| plant.position.rank())
        .min()
        .unwrap_or_default();
    let max_rank = plants
        .iter()
        .map(|plant| plant.position.rank())
        .max()
        .unwrap_or_default();
    let region = garden_region(
        plants
            .iter()
            .map(|plant| u16::from(plant.position.file()))
            .sum::<u16>()
            / plants.len() as u16,
        plants
            .iter()
            .map(|plant| u16::from(plant.position.rank()))
            .sum::<u16>()
            / plants.len() as u16,
    );
    let file_span = max_file - min_file;
    let rank_span = max_rank - min_rank;

    if plants.len() == 1 {
        format!("set in the {region} at {}", plants[0].position)
    } else if file_span <= 2 && rank_span <= 2 {
        format!("clustered in the {region}")
    } else if file_span >= 6 && rank_span >= 6 {
        "spread nearly corner to corner".to_string()
    } else if file_span >= 6 {
        "running nearly west to east".to_string()
    } else if rank_span >= 6 {
        "running nearly south to north".to_string()
    } else if region == "center" {
        "scattered around the center".to_string()
    } else {
        format!("scattered mostly across the {region}")
    }
}

fn garden_region(file: u16, rank: u16) -> &'static str {
    match (file, rank) {
        (0..=1, 0..=1) => "southwest corner",
        (6.., 0..=1) => "southeast corner",
        (0..=1, 6..) => "northwest corner",
        (6.., 6..) => "northeast corner",
        (0..=1, _) => "western side",
        (6.., _) => "eastern side",
        (_, 0..=1) => "southern side",
        (_, 6..) => "northern side",
        _ => "center",
    }
}

fn plant_stage_order(stage: &PlantStage) -> u8 {
    match stage {
        PlantStage::Seed => 0,
        PlantStage::Sprout => 1,
        PlantStage::Growing => 2,
        PlantStage::Flowering => 3,
        PlantStage::Fruiting => 4,
        PlantStage::Dormant => 5,
    }
}

fn plant_stage_count(count: usize, stage: &PlantStage) -> String {
    let noun = match (stage, count) {
        (PlantStage::Seed, 1) => "seed",
        (PlantStage::Seed, _) => "seeds",
        (PlantStage::Sprout, 1) => "sprout",
        (PlantStage::Sprout, _) => "sprouts",
        (PlantStage::Growing, 1) => "growing plant",
        (PlantStage::Growing, _) => "growing plants",
        (PlantStage::Flowering, 1) => "flowering plant",
        (PlantStage::Flowering, _) => "flowering plants",
        (PlantStage::Fruiting, 1) => "fruiting plant",
        (PlantStage::Fruiting, _) => "fruiting plants",
        (PlantStage::Dormant, 1) => "dormant plant",
        (PlantStage::Dormant, _) => "dormant plants",
    };
    format!("{} {noun}", count_word(count))
}

fn describe_visible_plant(plant: &PlantState) -> String {
    match plant.stage {
        PlantStage::Seed | PlantStage::Sprout => format!(
            "At {}, {} is a {} {}.",
            plant.position, plant.name, plant.species, plant.stage
        ),
        _ => format!(
            "At {}, {}, the {}, is {}.",
            plant.position, plant.name, plant.species, plant.stage
        ),
    }
}

fn count_word(count: usize) -> String {
    match count {
        0 => "no".to_string(),
        1 => "one".to_string(),
        2 => "two".to_string(),
        3 => "three".to_string(),
        4 => "four".to_string(),
        5 => "five".to_string(),
        6 => "six".to_string(),
        7 => "seven".to_string(),
        8 => "eight".to_string(),
        _ => count.to_string(),
    }
}

fn natural_join(parts: Vec<String>) -> String {
    match parts.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        _ => {
            let (last, rest) = parts.split_last().expect("non-empty list has a last item");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn render_garden_board(
    content: &GameContent,
    plants: &[PlantState],
    decorations: &[DecorationState],
) -> Vec<String> {
    let mut lines = vec![
        content.world.board_header.clone(),
        content.world.board_border.clone(),
    ];
    for rank in (0..GARDEN_RANKS).rev() {
        let cells = (0..GARDEN_FILES)
            .map(|file| {
                let position =
                    GardenPosition::new(file, rank).expect("board coordinates are in range");
                decorations
                    .iter()
                    .find(|decoration| decoration.position == position)
                    .map(|decoration| decoration.symbol)
                    .or_else(|| {
                        plants
                            .iter()
                            .find(|plant| plant.position == position)
                            .map(|plant| plant_stage_symbol(&plant.stage))
                    })
                    .unwrap_or('.')
            })
            .map(|symbol| format!(" {symbol} "))
            .collect::<Vec<_>>()
            .join("|");
        lines.push(format!(" {}  |{}|  {}", rank + 1, cells, rank + 1));
        lines.push(content.world.board_border.clone());
    }
    lines.push(content.world.board_header.clone());
    lines.push(content.world.board_legend.clone());
    lines
}

fn plant_stage_symbol(stage: &PlantStage) -> char {
    match stage {
        PlantStage::Seed => 's',
        PlantStage::Sprout => '+',
        PlantStage::Growing => 'g',
        PlantStage::Flowering => '*',
        PlantStage::Fruiting => 'o',
        PlantStage::Dormant => 'd',
    }
}

fn help(content: &GameContent) -> WorldOutput {
    WorldOutput::lines(content.command_help.clone())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn actor_gets_exactly_one_persistent_home() {
        let dir = tempdir().unwrap();
        let actor_id;
        let garden_id;
        {
            let mut world = World::open(dir.path());
            let actor = world.ensure_human("Daniel", None).unwrap();
            actor_id = actor.id;
            garden_id = actor.home_garden_id;
            assert_eq!(world.ensure_human("Daniel", None).unwrap().id, actor_id);
            world.checkpoint();
        }
        let world = World::open(dir.path());
        let actor = world.actor(actor_id).unwrap();
        assert_eq!(actor.home_garden_id, garden_id);
        assert_eq!(world.actors().len(), 1);
    }

    #[test]
    fn actor_starts_with_two_fruit() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("Daniel", None).unwrap();

        let fruit = actor
            .inventory
            .iter()
            .filter(|item| item.kind == ItemKind::Produce)
            .collect::<Vec<_>>();

        assert_eq!(fruit.len(), 2);
        assert_eq!(fruit[0].species, "scarlet runner bean");
        assert_eq!(fruit[1].species, "blue cornflower");
    }

    #[test]
    fn agents_begin_with_full_decorated_gardens() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let agents = world.ensure_world_agents().unwrap();
        let original_plant_ids = agents
            .iter()
            .flat_map(|agent| {
                let garden = world.garden(agent.home_garden_id).unwrap();
                world.plants_in_room(garden.room_id)
            })
            .map(|plant| plant.id)
            .collect::<BTreeSet<_>>();

        for agent in &agents {
            let garden = world.garden(agent.home_garden_id).unwrap();
            let plants = world.plants_in_room(garden.room_id);
            let occupied = plants
                .iter()
                .map(|plant| plant.position)
                .chain(
                    garden
                        .decorations
                        .iter()
                        .map(|decoration| decoration.position),
                )
                .collect::<BTreeSet<_>>();

            assert_eq!(occupied.len(), 64, "{}'s garden is not full", agent.name);
            assert!(
                !garden.decorations.is_empty(),
                "{}'s garden is not decorated",
                agent.name
            );
            assert!(plants.iter().all(|plant| {
                matches!(
                    plant.stage,
                    PlantStage::Growing | PlantStage::Flowering | PlantStage::Fruiting
                )
            }));
        }

        let human = world.ensure_human("Daniel", None).unwrap();
        let human_garden = world.garden(human.home_garden_id).unwrap();
        assert!(world.plants_in_room(human_garden.room_id).is_empty());
        assert!(human_garden.decorations.is_empty());

        let agents = world.ensure_world_agents().unwrap();
        assert_eq!(
            agents
                .iter()
                .flat_map(|agent| {
                    let garden = world.garden(agent.home_garden_id).unwrap();
                    world.plants_in_room(garden.room_id)
                })
                .map(|plant| plant.id)
                .collect::<BTreeSet<_>>(),
            original_plant_ids
        );
    }

    #[test]
    fn existing_homes_gain_a_gate_when_the_world_reopens() {
        let dir = tempdir().unwrap();
        let actor_id;
        {
            let mut world = World::open(dir.path());
            let actor = world.ensure_human("Daniel", None).unwrap();
            actor_id = actor.id;
            let garden = world.garden(actor.home_garden_id).unwrap();
            let gate = world.gate_for_garden(&garden).unwrap();
            let mut home = world.room(garden.room_id).unwrap();
            home.exits.insert("out".to_string(), COMMON_PATH);
            world.stream.wtx(|tx| {
                tx.remove(&EntityKey::Room(gate.id));
                tx.upsert(&EntityKey::Room(home.id), &WorldRecord::Room(home));
            });
            world.checkpoint();
        }

        let world = World::open(dir.path());
        let actor = world.actor(actor_id).unwrap();
        let garden = world.garden(actor.home_garden_id).unwrap();
        let gate = world.gate_for_garden(&garden).unwrap();
        assert_eq!(
            world.room(garden.room_id).unwrap().exits.get("out"),
            Some(&gate.id)
        );
        assert_eq!(gate.exits.get("in"), Some(&garden.room_id));
        assert_eq!(gate.exits.get("out"), Some(&COMMON_PATH));
    }

    #[test]
    fn walk_to_follows_real_exits_and_summarizes_the_route() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("Daniel", None).unwrap();

        let output = world
            .execute(actor.id, Command::WalkTo("wild edge".to_string()))
            .unwrap();

        assert_eq!(world.actor(actor.id).unwrap().current_room_id, WILD_EDGE);
        assert_eq!(output.events.len(), 8);
        assert_eq!(
            output.lines[0],
            "You walk out → out → west → north, passing Daniel's Garden Gate, \
             The Common Path, and The Pond, and arrive at The Wild Edge."
        );
        assert!(output.lines.iter().any(|line| line == "The Wild Edge"));
        assert_eq!(
            output.events.first().and_then(|event| event.room_id),
            Some(actor.current_room_id)
        );
        assert_eq!(
            output.events.last().and_then(|event| event.room_id),
            Some(WILD_EDGE)
        );
    }

    #[test]
    fn walk_to_respects_private_garden_permissions() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();
        world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap();

        let denied = world
            .execute(mara.id, Command::WalkTo("Daniel's Garden".to_string()))
            .unwrap_err();
        assert!(denied.to_string().contains("No open path"));

        world
            .execute(
                daniel.id,
                Command::Allow {
                    actor: "Mara".to_string(),
                    action: "tend".to_string(),
                },
            )
            .unwrap();
        let output = world
            .execute(mara.id, Command::WalkTo("Daniel's Garden".to_string()))
            .unwrap();
        assert_eq!(
            output.lines[0],
            "You walk in and arrive at Daniel's Garden."
        );
        assert_eq!(
            world.actor(mara.id).unwrap().current_room_id,
            daniel.current_room_id
        );
        assert!(
            output
                .events
                .last()
                .is_some_and(|event| event.recipients.contains(&daniel.id))
        );
        assert!(
            output
                .events
                .last()
                .is_some_and(|event| event.message == "Mara enters Daniel's Garden.")
        );
    }

    #[test]
    fn walk_to_reports_unknown_and_ambiguous_places() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("Daniel", None).unwrap();
        world.ensure_human("Mara", None).unwrap();

        let unknown = world
            .execute(actor.id, Command::WalkTo("stone orchard".to_string()))
            .unwrap_err();
        assert!(unknown.to_string().contains("stone orchard"));

        let ambiguous = world
            .execute(actor.id, Command::WalkTo("Daniel".to_string()))
            .unwrap_err();
        assert!(ambiguous.to_string().contains("several places"));
        assert!(ambiguous.to_string().contains("Daniel's Garden"));
        assert!(ambiguous.to_string().contains("Daniel's Garden Gate"));
    }

    #[test]
    fn garden_is_an_addressable_eight_by_eight_board() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("Daniel", None).unwrap();
        world
            .execute(
                actor.id,
                Command::Plant {
                    species: "scarlet runner bean".to_string(),
                    position: "C4".parse().unwrap(),
                    name: Some("luna".to_string()),
                },
            )
            .unwrap();

        let look = world
            .execute(actor.id, Command::Look(None))
            .unwrap()
            .lines
            .join("\n");
        assert!(!look.contains("      A   B   C   D   E   F   G   H"));
        assert!(look.contains("C4  luna — scarlet runner bean (seed)"));

        let description = world
            .execute(actor.id, Command::Look(Some("garden".to_string())))
            .unwrap()
            .lines
            .join("\n");
        assert!(description.contains("Daniel's Garden"));
        assert!(description.contains("One plant occupies the beds, set in the center at C4"));
        assert!(description.contains("At C4, luna is a scarlet runner bean seed."));
        assert!(description.contains("Use `survey garden` to see the individual plots."));
        assert!(!description.contains("      A   B   C   D   E   F   G   H"));

        let garden = world
            .execute(actor.id, Command::Garden)
            .unwrap()
            .lines
            .join("\n");
        assert!(garden.contains("      A   B   C   D   E   F   G   H"));
        assert!(garden.contains(" 4  | . | . | s | . | . | . | . | . |  4"));
        assert!(garden.contains("C4  luna — scarlet runner bean (seed)"));

        let surveyed_garden = world
            .execute(actor.id, crate::commands::parse("survey garden").unwrap())
            .unwrap()
            .lines
            .join("\n");
        assert_eq!(surveyed_garden, garden);

        let inspect = world
            .execute(actor.id, Command::Inspect("c4".to_string()))
            .unwrap();
        assert_eq!(inspect.lines[0], "luna — scarlet runner bean");

        let before = world.actor(actor.id).unwrap().inventory.len();
        let error = world
            .execute(
                actor.id,
                Command::Plant {
                    species: "blue cornflower".to_string(),
                    position: "C4".parse().unwrap(),
                    name: None,
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("already occupied"));
        assert_eq!(world.actor(actor.id).unwrap().inventory.len(), before);
    }

    #[test]
    fn looking_through_an_exit_previews_the_room_without_moving() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("Daniel", None).unwrap();
        world
            .execute(actor.id, Command::Go("out".to_string()))
            .unwrap();
        world
            .execute(actor.id, Command::Go("out".to_string()))
            .unwrap();
        world
            .execute(actor.id, Command::Go("east".to_string()))
            .unwrap();
        world
            .execute(
                actor.id,
                Command::Plant {
                    species: "scarlet runner bean".to_string(),
                    position: "A1".parse().unwrap(),
                    name: Some("hidden moon".to_string()),
                },
            )
            .unwrap();
        world
            .execute(actor.id, Command::Go("west".to_string()))
            .unwrap();
        let current_room_id = world.actor(actor.id).unwrap().current_room_id;

        let output = world
            .execute(actor.id, Command::Look(Some("e".to_string())))
            .unwrap();

        assert_eq!(output.lines[0], "Looking east, you see:");
        assert!(output.lines.iter().any(|line| line == "The Moon Bed"));
        assert!(output.lines.iter().any(|line| line
            == "A garden lies there. Move there, then use `look garden` to take it in or `survey garden` to map the beds."));
        assert!(
            !output
                .lines
                .iter()
                .any(|line| line.contains("A   B   C   D   E   F   G   H"))
        );
        assert!(!output.lines.iter().any(|line| line.contains("hidden moon")));
        assert!(
            output
                .lines
                .iter()
                .any(|line| line == "From there, exits: west.")
        );
        assert!(output.events.is_empty());
        assert_eq!(
            world.actor(actor.id).unwrap().current_room_id,
            current_room_id
        );
    }

    #[test]
    fn looking_toward_a_missing_exit_uses_the_movement_error() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("Daniel", None).unwrap();

        let error = world
            .execute(actor.id, Command::Look(Some("east".to_string())))
            .unwrap_err();

        assert_eq!(error.to_string(), "There is no way east from here.");
    }

    #[test]
    fn plant_growth_updates_schedule_without_duplicates() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let actor = world.ensure_human("Daniel", None).unwrap();
        world
            .execute(
                actor.id,
                Command::Plant {
                    species: "scarlet runner bean".to_string(),
                    position: "A1".parse().unwrap(),
                    name: None,
                },
            )
            .unwrap();
        for _ in 0..4 {
            world.tick().unwrap();
        }
        let plants = world.plants_in_room(actor.current_room_id);
        assert_eq!(plants.len(), 1);
        assert_eq!(plants[0].stage, PlantStage::Flowering);
        assert_eq!(world.due_plants(world.clock().now).len(), 0);
    }

    #[test]
    fn bog_updates_are_bounded_and_reactive_views_do_not_duplicate_records() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let meta = world.bog_meta().unwrap();
        let clock = WorldClock {
            now: 24,
            ..world.clock()
        };
        let (cells, organisms, _, _) = world.calculate_bog_updates(&clock);
        assert!(!cells.is_empty());
        assert!(cells.len() + organisms.len() <= world.bog_config.work_budget);

        for _ in 0..48 {
            world.tick().unwrap();
        }
        let (cell_records, cell_schedule, organism_records, organism_schedule) = world.stream.rtx(
            |(_, _, _, _, _, _, _, _, (cells, organisms, cell_due, organism_due, ..))| {
                (
                    cells.iter().count(),
                    cell_due
                        .iter()
                        .map(|(_, count)| count.max(0) as usize)
                        .sum::<usize>(),
                    organisms.iter().count(),
                    organism_due
                        .iter()
                        .map(|(_, count)| count.max(0) as usize)
                        .sum::<usize>(),
                )
            },
        );
        assert_eq!(
            cell_records,
            usize::from(meta.edge_length) * usize::from(meta.edge_length)
        );
        assert_eq!(cell_schedule, cell_records);
        assert_eq!(organism_schedule, organism_records);
    }

    #[test]
    fn players_can_survey_restore_and_reopen_their_room_ecology() {
        let dir = tempdir().unwrap();
        let restored_cell;
        {
            let mut world = World::open(dir.path());
            let actor = world.ensure_human("Daniel", None).unwrap();
            let meta = world.bog_meta().unwrap();
            let (x, y) = ecology::room_grid_positions(meta.edge_length, &world.rooms())
                [&actor.current_room_id];
            let cell_id = ecology::cell_id(meta.edge_length, x, y).unwrap();
            let before = world.bog_cell(cell_id).unwrap();
            let survey = world
                .execute(actor.id, Command::Survey(None))
                .unwrap()
                .lines
                .join("\n");
            assert!(survey.contains("Daniel's Garden"));
            world.execute(actor.id, Command::Restore(x, y)).unwrap();
            restored_cell = world.bog_cell(cell_id).unwrap();
            assert!(restored_cell.water_table_mm > before.water_table_mm);
            assert!(restored_cell.nutrients >= before.nutrients);

            let ivo = world
                .ensure_world_agents()
                .unwrap()
                .into_iter()
                .find(|actor| actor.name == "Ivo")
                .unwrap();
            let turn = world.agent_turn(&ivo).unwrap();
            assert!(
                !turn
                    .ecology
                    .as_ref()
                    .unwrap()
                    .restoration_candidates
                    .is_empty()
            );
            assert!(
                turn.available_commands
                    .iter()
                    .any(|command| command.starts_with("restore"))
            );
            world.checkpoint();
        }

        let world = World::open(dir.path());
        assert_eq!(world.bog_cell(restored_cell.id).unwrap(), restored_cell);
        assert_eq!(
            world.debug_snapshot(10).species.len(),
            ecology::SPECIES.len()
        );
    }

    #[test]
    fn another_actor_cannot_tend_private_home() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();
        world
            .execute(
                daniel.id,
                Command::Plant {
                    species: "scarlet runner bean".to_string(),
                    position: "A1".parse().unwrap(),
                    name: None,
                },
            )
            .unwrap();
        let mut mara_state = world.actor(mara.id).unwrap();
        mara_state.current_room_id = daniel.current_room_id;
        world.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Actor(mara.id), &WorldRecord::Actor(mara_state));
        });
        let error = world
            .execute(mara.id, Command::Water("scarlet runner bean".to_string()))
            .unwrap_err();
        assert!(error.to_string().contains("permission"));
    }

    #[test]
    fn planting_consumes_a_seed_and_permissions_enable_tending() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();
        world
            .execute(
                daniel.id,
                Command::Plant {
                    species: "scarlet runner bean".to_string(),
                    position: "A1".parse().unwrap(),
                    name: Some("luna".to_string()),
                },
            )
            .unwrap();
        assert_eq!(world.actor(daniel.id).unwrap().inventory.len(), 4);

        world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap();
        world
            .execute(
                daniel.id,
                Command::Allow {
                    actor: "Mara".to_string(),
                    action: "tend here".to_string(),
                },
            )
            .unwrap();
        world.execute(mara.id, Command::Enter).unwrap();
        world
            .execute(mara.id, Command::Water("luna".to_string()))
            .unwrap();
    }

    #[test]
    fn garden_gates_support_discovery_knocking_permissions_and_return() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();

        let gardens = world
            .execute(mara.id, Command::Gardens)
            .unwrap()
            .lines
            .join("\n");
        assert!(gardens.contains("Daniel's gate"));
        assert!(gardens.contains("Mara's gate (yours)"));
        assert!(gardens.contains("visit <name>"));

        let visit = world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap()
            .lines
            .join("\n");
        let gate = world
            .room(world.actor(mara.id).unwrap().current_room_id)
            .unwrap();
        assert_eq!(gate.kind, RoomKind::GardenGate);
        assert!(visit.contains("Daniel's Garden Gate"));
        assert!(
            world
                .execute(mara.id, Command::Enter)
                .unwrap_err()
                .to_string()
                .contains("Try knocking")
        );

        let knock = world.execute(mara.id, Command::Knock).unwrap();
        assert_eq!(knock.events[0].kind, EventKind::Knocking);
        assert!(knock.events[0].recipients.contains(&daniel.id));

        world
            .execute(
                daniel.id,
                Command::Allow {
                    actor: "Mara".to_string(),
                    action: "tend here".to_string(),
                },
            )
            .unwrap();
        world.execute(mara.id, Command::Enter).unwrap();
        assert_eq!(
            world.actor(mara.id).unwrap().current_room_id,
            daniel.current_room_id
        );

        world
            .execute(mara.id, Command::Go("out".to_string()))
            .unwrap();
        assert_eq!(
            world
                .room(world.actor(mara.id).unwrap().current_room_id)
                .unwrap()
                .kind,
            RoomKind::GardenGate
        );
        world
            .execute(mara.id, Command::Go("out".to_string()))
            .unwrap();
        assert_eq!(world.actor(mara.id).unwrap().current_room_id, COMMON_PATH);

        world.execute(mara.id, Command::Home).unwrap();
        assert_eq!(
            world.actor(mara.id).unwrap().current_room_id,
            world.garden(mara.home_garden_id).unwrap().room_id
        );
    }

    #[test]
    fn owners_can_unlock_and_relock_entry_without_granting_tending_permission() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();

        world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap();
        assert!(
            world
                .execute(mara.id, Command::Enter)
                .unwrap_err()
                .to_string()
                .contains("Try knocking")
        );

        let unlocked = world.execute(daniel.id, Command::UnlockGarden).unwrap();
        assert_eq!(unlocked.lines, ["You unlock your garden gate."]);
        assert!(world.garden_is_unlocked(daniel.home_garden_id));

        world.execute(mara.id, Command::Enter).unwrap();
        let tending_error = world
            .execute(
                mara.id,
                Command::Plant {
                    species: "scarlet runner bean".to_string(),
                    position: "A1".parse().unwrap(),
                    name: None,
                },
            )
            .unwrap_err();
        assert!(tending_error.to_string().contains("permission"));

        world
            .execute(mara.id, Command::Go("out".to_string()))
            .unwrap();
        let locked = world.execute(daniel.id, Command::LockGarden).unwrap();
        assert_eq!(locked.lines, ["You lock your garden gate."]);
        assert!(!world.garden_is_unlocked(daniel.home_garden_id));
        assert!(
            world
                .execute(mara.id, Command::Enter)
                .unwrap_err()
                .to_string()
                .contains("Try knocking")
        );
    }

    #[test]
    fn garden_lock_state_persists_across_reopen() {
        let dir = tempdir().unwrap();
        let garden_id = {
            let mut world = World::open(dir.path());
            let owner = world.ensure_human("Daniel", None).unwrap();
            assert!(!world.garden_is_unlocked(owner.home_garden_id));
            world.execute(owner.id, Command::UnlockGarden).unwrap();
            owner.home_garden_id
        };

        let world = World::open(dir.path());
        assert!(world.garden_is_unlocked(garden_id));
    }

    #[test]
    fn owners_can_admit_a_waiting_visitor_for_one_entry() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();

        let not_waiting = world
            .execute(daniel.id, Command::Admit("Mara".to_string()))
            .unwrap_err();
        assert!(not_waiting.to_string().contains("not waiting"));

        world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap();
        let admitted = world
            .execute(daniel.id, Command::Admit("Mara".to_string()))
            .unwrap();
        assert_eq!(
            admitted.lines,
            ["You unlatch the gate for Mara. They may enter once."]
        );
        assert!(admitted.events[0].recipients.contains(&mara.id));
        assert!(world.has_garden_admission(daniel.home_garden_id, mara.id));

        world.execute(mara.id, Command::Enter).unwrap();
        assert!(!world.has_garden_admission(daniel.home_garden_id, mara.id));
        let tending_error = world
            .execute(
                mara.id,
                Command::Plant {
                    species: "scarlet runner bean".to_string(),
                    position: "A1".parse().unwrap(),
                    name: None,
                },
            )
            .unwrap_err();
        assert!(tending_error.to_string().contains("permission"));

        world
            .execute(mara.id, Command::Go("out".to_string()))
            .unwrap();
        assert!(
            world
                .execute(mara.id, Command::Enter)
                .unwrap_err()
                .to_string()
                .contains("Try knocking")
        );
    }

    #[test]
    fn walking_through_an_admitted_gate_consumes_the_entry() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();
        world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap();
        world
            .execute(daniel.id, Command::Admit("Mara".to_string()))
            .unwrap();

        world
            .execute(mara.id, Command::WalkTo("Daniel's Garden".to_string()))
            .unwrap();
        assert!(!world.has_garden_admission(daniel.home_garden_id, mara.id));

        world
            .execute(mara.id, Command::Go("out".to_string()))
            .unwrap();
        let denied = world
            .execute(mara.id, Command::WalkTo("Daniel's Garden".to_string()))
            .unwrap_err();
        assert!(denied.to_string().contains("No open path"));
    }

    #[test]
    fn speech_outside_a_gate_is_muffled_inside_the_garden() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();
        world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap();

        let speech = world
            .execute(
                mara.id,
                Command::Say("the runner beans are ready".to_string()),
            )
            .unwrap();

        assert_eq!(speech.events.len(), 2);
        assert_eq!(
            speech.events[0].message,
            "Mara says, “the runner beans are ready”"
        );
        let muffled = &speech.events[1];
        assert_eq!(muffled.kind, EventKind::Speech);
        assert_eq!(muffled.room_id, Some(daniel.current_room_id));
        assert_eq!(muffled.recipients, vec![daniel.id]);
        assert_eq!(
            muffled.message,
            "Mara says something muffled from behind the gate."
        );

        let changes = world
            .execute(daniel.id, Command::Changes)
            .unwrap()
            .lines
            .join("\n");
        assert!(changes.contains("Mara says something muffled from behind the gate."));
        assert!(!changes.contains("the runner beans are ready"));
    }

    #[test]
    fn human_speech_immediately_prepares_turns_for_agents_in_the_room() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let agents = world.ensure_world_agents().unwrap();
        let mut ivo = agents
            .iter()
            .find(|actor| actor.name == "Ivo")
            .cloned()
            .unwrap();
        let wren = agents.iter().find(|actor| actor.name == "Wren").unwrap();
        ivo.current_room_id = daniel.current_room_id;
        world.stream.wtx(|tx| {
            tx.upsert(&EntityKey::Actor(ivo.id), &WorldRecord::Actor(ivo.clone()));
        });
        world
            .execute(
                ivo.id,
                Command::Say(
                    "Hello, Daniel. The blue cornflower is holding through this rain.".to_string(),
                ),
            )
            .unwrap();

        let speech = world
            .execute(
                daniel.id,
                Command::Say("Ivo, how are the cornflowers?".to_string()),
            )
            .unwrap();
        let turns = world.prepare_reactive_agent_turns(&speech.events).unwrap();

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].actor_id, ivo.id);
        assert_eq!(
            turns[0].triggering_speech,
            vec!["Daniel says, “Ivo, how are the cornflowers?”"]
        );
        assert_eq!(
            turns[0].recent_speech,
            vec!["Ivo says, “Hello, Daniel. The blue cornflower is holding through this rain.”"]
        );
        assert!(world.actor(ivo.id).unwrap().agent.unwrap().next_wake_at > world.clock().now);
        assert_eq!(
            world.actor(wren.id).unwrap().agent.unwrap().next_wake_at,
            wren.agent.as_ref().unwrap().next_wake_at
        );
    }

    #[test]
    fn human_knock_immediately_prepares_a_turn_for_the_agent_owner() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let ivo = world
            .ensure_world_agents()
            .unwrap()
            .into_iter()
            .find(|actor| actor.name == "Ivo")
            .unwrap();
        world
            .execute(daniel.id, Command::Visit("Ivo".to_string()))
            .unwrap();

        let knock = world.execute(daniel.id, Command::Knock).unwrap();
        let turns = world.prepare_reactive_agent_turns(&knock.events).unwrap();

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].actor_id, ivo.id);
        assert_eq!(
            turns[0].triggering_knocks,
            vec!["Daniel knocks at Ivo's garden gate."]
        );
        assert!(
            turns[0]
                .available_commands
                .iter()
                .any(|command| command == "admit <person>")
        );
        assert!(world.actor(ivo.id).unwrap().agent.unwrap().next_wake_at > world.clock().now);
    }

    #[test]
    fn sorrel_answers_human_speech_on_the_common_path() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let mut daniel = world.ensure_human("Daniel", None).unwrap();
        let sorrel = world
            .ensure_world_agents()
            .unwrap()
            .into_iter()
            .find(|actor| actor.name == "Sorrel")
            .unwrap();
        assert_eq!(sorrel.current_room_id, COMMON_PATH);
        assert!(sorrel.agent.is_some());
        daniel.current_room_id = COMMON_PATH;
        world.stream.wtx(|tx| {
            tx.upsert(
                &EntityKey::Actor(daniel.id),
                &WorldRecord::Actor(daniel.clone()),
            );
        });

        let speech = world
            .execute(daniel.id, Command::Say("hi".to_string()))
            .unwrap();
        let turns = world.prepare_reactive_agent_turns(&speech.events).unwrap();

        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].actor_id, sorrel.id);
        assert_eq!(turns[0].triggering_speech, vec!["Daniel says, “hi”"]);
    }

    #[test]
    fn agent_speech_does_not_trigger_an_endless_reply_chain() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let agents = world.ensure_world_agents().unwrap();
        let ivo = agents.iter().find(|actor| actor.name == "Ivo").unwrap();
        let mut wren = agents
            .iter()
            .find(|actor| actor.name == "Wren")
            .cloned()
            .unwrap();
        wren.current_room_id = ivo.current_room_id;
        world.stream.wtx(|tx| {
            tx.upsert(
                &EntityKey::Actor(wren.id),
                &WorldRecord::Actor(wren.clone()),
            );
        });

        let speech = world
            .execute(
                ivo.id,
                Command::Say("The cornflowers held their color.".to_string()),
            )
            .unwrap();

        assert!(
            world
                .prepare_reactive_agent_turns(&speech.events)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn offers_transfer_inventory_between_present_actors() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let daniel = world.ensure_human("Daniel", None).unwrap();
        let mara = world.ensure_human("Mara", None).unwrap();
        world
            .execute(
                daniel.id,
                Command::Allow {
                    actor: "Mara".to_string(),
                    action: "tend here".to_string(),
                },
            )
            .unwrap();
        world
            .execute(mara.id, Command::Visit("Daniel".to_string()))
            .unwrap();
        world.execute(mara.id, Command::Enter).unwrap();
        let before = world.actor(mara.id).unwrap().inventory.len();
        world
            .execute(
                daniel.id,
                Command::Offer {
                    item: "blue cornflower seed".to_string(),
                    recipient: "Mara".to_string(),
                },
            )
            .unwrap();
        assert_eq!(world.actor(mara.id).unwrap().inventory.len(), before + 1);
        assert_eq!(world.actor(daniel.id).unwrap().inventory.len(), 4);
    }

    #[test]
    fn fruit_buys_a_persistent_decoration_that_can_be_placed_and_taken() {
        let dir = tempdir().unwrap();
        let actor_id;
        let garden_id;
        {
            let mut world = World::open(dir.path());
            world.ensure_world_agents().unwrap();
            let actor = world.ensure_human("Daniel", None).unwrap();
            actor_id = actor.id;
            garden_id = actor.home_garden_id;

            let mut actor = world.actor(actor_id).unwrap();
            let mut meta = world.meta();
            actor.inventory.push(allocate_item(
                &mut meta,
                ItemKind::Produce,
                "blue cornflower",
            ));
            world.stream.wtx(|tx| {
                tx.upsert(&EntityKey::Meta, &WorldRecord::Meta(meta));
                tx.upsert(
                    &EntityKey::Actor(actor.id),
                    &WorldRecord::Actor(actor.clone()),
                );
            });

            world
                .execute(actor_id, Command::Go("out".to_string()))
                .unwrap();
            world
                .execute(actor_id, Command::Go("out".to_string()))
                .unwrap();
            let shop = world
                .execute(actor_id, Command::Shop)
                .unwrap()
                .lines
                .join("\n");
            assert!(shop.contains("mossy stone seat"));
            world
                .execute(actor_id, Command::Buy("mossy stone seat".to_string()))
                .unwrap();
            assert!(
                world
                    .actor(actor_id)
                    .unwrap()
                    .inventory
                    .iter()
                    .any(|item| item.kind == ItemKind::Decoration)
            );

            world.execute(actor_id, Command::Home).unwrap();
            world
                .execute(
                    actor_id,
                    Command::Place {
                        decoration: "mossy stone seat".to_string(),
                        position: "C3".parse().unwrap(),
                    },
                )
                .unwrap();
            let board = world
                .execute(actor_id, Command::Garden)
                .unwrap()
                .lines
                .join("\n");
            assert!(board.contains("C3  mossy stone seat"));
            assert_eq!(world.garden(garden_id).unwrap().decorations.len(), 1);
            world.checkpoint();
        }

        let mut world = World::open(dir.path());
        let decoration = world.garden(garden_id).unwrap().decorations[0].clone();
        assert_eq!(decoration.position, "C3".parse().unwrap());
        world
            .execute(actor_id, Command::TakeDecoration("C3".to_string()))
            .unwrap();
        assert!(world.garden(garden_id).unwrap().decorations.is_empty());
        assert!(
            world
                .actor(actor_id)
                .unwrap()
                .inventory
                .iter()
                .any(|item| item.id == decoration.id && item.kind == ItemKind::Decoration)
        );
    }

    #[test]
    fn reactive_agent_schedule_produces_model_context_and_accepts_a_plan() {
        let dir = tempdir().unwrap();
        let mut world = World::open(dir.path());
        let agents = world.ensure_world_agents().unwrap();
        assert_eq!(agents.len(), 5);
        world.tick().unwrap();
        let turns = world.prepare_due_agent_turns().unwrap();
        assert_eq!(turns.len(), 5);
        let ivo_turn = turns.iter().find(|turn| turn.name == "Ivo").unwrap();
        assert!(ivo_turn.goal.contains("blue cornflowers"));
        assert!(
            ivo_turn
                .available_commands
                .iter()
                .any(|form| form.starts_with("plant"))
        );
        let ivo = world.actor_by_name("Ivo").unwrap();
        let plant = world
            .plants_in_room(ivo.current_room_id)
            .into_iter()
            .min_by_key(|plant| plant.position)
            .unwrap();
        world
            .execute_agent_plan(
                ivo_turn.actor_id,
                Command::Water(plant.position.to_string()),
                "tend the established garden",
            )
            .unwrap();
        let ivo = world.actor_by_name("Ivo").unwrap();
        let tended = world
            .plants_in_room(ivo.current_room_id)
            .into_iter()
            .find(|candidate| candidate.id == plant.id)
            .unwrap();
        assert!(tended.moisture > plant.moisture);
    }

    #[test]
    fn existing_agent_definition_refreshes_role_strategy_and_budget() {
        let dir = tempdir().unwrap();
        let ivo_id;
        {
            let mut world = World::open_with_content(dir.path(), GameContent::bundled());
            let ivo = world
                .ensure_world_agents()
                .unwrap()
                .into_iter()
                .find(|actor| actor.name == "Ivo")
                .unwrap();
            ivo_id = ivo.id;
            world.checkpoint();
        }

        let mut content = GameContent::bundled().as_ref().clone();
        let ivo = content.npcs.get_mut("ivo").unwrap();
        ivo.kind = ActorKind::Helper;
        ivo.strategy = AgentStrategy::Helper;
        ivo.goal = "help every new gardener".to_string();
        ivo.action_budget = 7;

        let mut world = World::open_with_content(dir.path(), Arc::new(content));
        let ivo = world
            .ensure_world_agents()
            .unwrap()
            .into_iter()
            .find(|actor| actor.id == ivo_id)
            .unwrap();
        assert_eq!(ivo.kind, ActorKind::Helper);
        assert!(ivo.capabilities.contains(&Capability::HelpGardeners));
        let profile = ivo.agent.unwrap();
        assert_eq!(profile.strategy, AgentStrategy::Helper);
        assert_eq!(profile.goal, "help every new gardener");
        assert_eq!(profile.action_budget, 7);
    }
}
