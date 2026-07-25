use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    BogCellId, BogCellState, BogLifeStage, BogOrganismId, BogOrganismState, RoomId, RoomKind,
    RoomState, Season, Weather, WorldClock,
};

pub const ECOLOGY_VERSION: u16 = 1;
pub const CELL_TRANSITION_HOURS: u64 = 12;
pub const ORGANISM_TRANSITION_HOURS: u64 = 24;

#[derive(Debug, Clone)]
pub struct BogConfig {
    pub edge_length: u16,
    pub initial_organisms: u64,
    pub work_budget: usize,
}

impl BogConfig {
    pub fn from_env() -> Self {
        let test_defaults = cfg!(test);
        let edge_length = env_number("MUDGARDEN_GRID_EDGE")
            .or_else(|| env_number("MUDGARDEN_BOG_EDGE"))
            .unwrap_or(if test_defaults { 4 } else { 24 })
            .clamp(4, 32) as u16;
        let cells = u64::from(edge_length) * u64::from(edge_length);
        let initial_organisms = env_number("MUDGARDEN_WORLD_ORGANISMS")
            .or_else(|| env_number("MUDGARDEN_BOG_ORGANISMS"))
            .unwrap_or(if test_defaults { 32 } else { 2_000 })
            .clamp(cells / 2, 10_000);
        let work_budget = env_number("MUDGARDEN_ECOLOGY_WORK_BUDGET")
            .unwrap_or(if test_defaults { 32 } else { 160 })
            .clamp(16, 2_000) as usize;
        Self {
            edge_length,
            initial_organisms,
            work_budget,
        }
    }
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

#[derive(Debug, Clone, Copy)]
pub struct SpeciesProfile {
    pub name: &'static str,
    pub ideal_water_table_mm: i16,
    pub water_tolerance_mm: i16,
    pub ideal_ph_cent: u16,
    pub ph_tolerance_cent: u16,
    pub minimum_light: u8,
    pub growth_g_per_day: u16,
    pub maximum_biomass_g: u32,
    pub flowers_in: Season,
    pub peat_builder: bool,
    pub shrub: bool,
}

pub const SPECIES: [SpeciesProfile; 12] = [
    SpeciesProfile {
        name: "sphagnum moss",
        ideal_water_table_mm: -5,
        water_tolerance_mm: 35,
        ideal_ph_cent: 400,
        ph_tolerance_cent: 100,
        minimum_light: 25,
        growth_g_per_day: 3,
        maximum_biomass_g: 900,
        flowers_in: Season::Spring,
        peat_builder: true,
        shrub: false,
    },
    SpeciesProfile {
        name: "cottongrass",
        ideal_water_table_mm: -15,
        water_tolerance_mm: 45,
        ideal_ph_cent: 440,
        ph_tolerance_cent: 120,
        minimum_light: 40,
        growth_g_per_day: 5,
        maximum_biomass_g: 1_400,
        flowers_in: Season::Summer,
        peat_builder: true,
        shrub: false,
    },
    SpeciesProfile {
        name: "round-leaved sundew",
        ideal_water_table_mm: -8,
        water_tolerance_mm: 28,
        ideal_ph_cent: 410,
        ph_tolerance_cent: 80,
        minimum_light: 55,
        growth_g_per_day: 1,
        maximum_biomass_g: 120,
        flowers_in: Season::Summer,
        peat_builder: false,
        shrub: false,
    },
    SpeciesProfile {
        name: "bogbean",
        ideal_water_table_mm: 4,
        water_tolerance_mm: 35,
        ideal_ph_cent: 480,
        ph_tolerance_cent: 120,
        minimum_light: 45,
        growth_g_per_day: 4,
        maximum_biomass_g: 1_100,
        flowers_in: Season::Spring,
        peat_builder: false,
        shrub: false,
    },
    SpeciesProfile {
        name: "cranberry",
        ideal_water_table_mm: -18,
        water_tolerance_mm: 45,
        ideal_ph_cent: 430,
        ph_tolerance_cent: 100,
        minimum_light: 40,
        growth_g_per_day: 2,
        maximum_biomass_g: 650,
        flowers_in: Season::Summer,
        peat_builder: false,
        shrub: false,
    },
    SpeciesProfile {
        name: "bog rosemary",
        ideal_water_table_mm: -22,
        water_tolerance_mm: 40,
        ideal_ph_cent: 420,
        ph_tolerance_cent: 90,
        minimum_light: 45,
        growth_g_per_day: 2,
        maximum_biomass_g: 700,
        flowers_in: Season::Spring,
        peat_builder: false,
        shrub: true,
    },
    SpeciesProfile {
        name: "cross-leaved heath",
        ideal_water_table_mm: -28,
        water_tolerance_mm: 55,
        ideal_ph_cent: 430,
        ph_tolerance_cent: 110,
        minimum_light: 50,
        growth_g_per_day: 3,
        maximum_biomass_g: 1_000,
        flowers_in: Season::Summer,
        peat_builder: false,
        shrub: true,
    },
    SpeciesProfile {
        name: "purple moor-grass",
        ideal_water_table_mm: -38,
        water_tolerance_mm: 65,
        ideal_ph_cent: 470,
        ph_tolerance_cent: 140,
        minimum_light: 45,
        growth_g_per_day: 6,
        maximum_biomass_g: 2_200,
        flowers_in: Season::Autumn,
        peat_builder: false,
        shrub: false,
    },
    SpeciesProfile {
        name: "bog asphodel",
        ideal_water_table_mm: -20,
        water_tolerance_mm: 42,
        ideal_ph_cent: 440,
        ph_tolerance_cent: 100,
        minimum_light: 55,
        growth_g_per_day: 2,
        maximum_biomass_g: 500,
        flowers_in: Season::Summer,
        peat_builder: false,
        shrub: false,
    },
    SpeciesProfile {
        name: "royal fern",
        ideal_water_table_mm: -12,
        water_tolerance_mm: 48,
        ideal_ph_cent: 500,
        ph_tolerance_cent: 130,
        minimum_light: 25,
        growth_g_per_day: 5,
        maximum_biomass_g: 2_800,
        flowers_in: Season::Summer,
        peat_builder: false,
        shrub: false,
    },
    SpeciesProfile {
        name: "downy birch",
        ideal_water_table_mm: -70,
        water_tolerance_mm: 75,
        ideal_ph_cent: 500,
        ph_tolerance_cent: 150,
        minimum_light: 55,
        growth_g_per_day: 8,
        maximum_biomass_g: 20_000,
        flowers_in: Season::Spring,
        peat_builder: false,
        shrub: true,
    },
    SpeciesProfile {
        name: "grey willow",
        ideal_water_table_mm: -45,
        water_tolerance_mm: 70,
        ideal_ph_cent: 530,
        ph_tolerance_cent: 160,
        minimum_light: 45,
        growth_g_per_day: 9,
        maximum_biomass_g: 18_000,
        flowers_in: Season::Spring,
        peat_builder: false,
        shrub: true,
    },
];

pub fn profile(species: &str) -> &'static SpeciesProfile {
    SPECIES
        .iter()
        .find(|profile| profile.name == species)
        .unwrap_or(&SPECIES[0])
}

pub fn cell_id(edge_length: u16, x: u16, y: u16) -> Option<BogCellId> {
    (x < edge_length && y < edge_length).then_some(BogCellId(
        u64::from(y) * u64::from(edge_length) + u64::from(x) + 1,
    ))
}

pub fn room_grid_positions(edge_length: u16, rooms: &[RoomState]) -> BTreeMap<RoomId, (u16, u16)> {
    let mut logical = BTreeMap::<RoomId, (u32, u32)>::new();
    let mut homes = Vec::new();
    let mut garden_gates = BTreeSet::new();
    for room in rooms {
        let position = match room.kind {
            RoomKind::Gate => Some((500, 380)),
            RoomKind::CommonPath => Some((500, 255)),
            RoomKind::Glasshouse => Some((500, 85)),
            RoomKind::MoonBed => Some((800, 245)),
            RoomKind::Pond => Some((200, 245)),
            RoomKind::Compost => Some((800, 80)),
            RoomKind::WildEdge => Some((200, 80)),
            RoomKind::HomeGarden => {
                homes.push(room);
                None
            }
            RoomKind::GardenGate => {
                garden_gates.insert(room.id);
                None
            }
        };
        if let Some(position) = position {
            logical.insert(room.id, position);
        }
    }
    homes.sort_by_key(|room| room.id);
    let columns = homes.len().clamp(1, 5);
    let column_gap = if columns == 1 {
        0
    } else {
        (800 / (columns - 1)).min(200)
    };
    let start_x = 500 - column_gap * (columns - 1) / 2;
    let mut assigned_gates = BTreeSet::new();
    for (index, room) in homes.iter().enumerate() {
        let row = index / columns;
        let column = index % columns;
        let x = start_x + column * column_gap;
        logical.insert(room.id, (x as u32, (590 + row * 210) as u32));
        if let Some(gate_id) = room.exits.get("out").filter(|id| garden_gates.contains(id)) {
            logical.insert(*gate_id, (x as u32, (475 + row * 210) as u32));
            assigned_gates.insert(*gate_id);
        }
    }
    let unpaired = garden_gates
        .difference(&assigned_gates)
        .copied()
        .collect::<Vec<_>>();
    let gap = if unpaired.len() <= 1 {
        0
    } else {
        (800 / (unpaired.len() - 1)).min(200)
    };
    let start = 500 - gap * unpaired.len().saturating_sub(1) / 2;
    for (index, room_id) in unpaired.into_iter().enumerate() {
        logical.insert(room_id, ((start + index * gap) as u32, 475));
    }

    let logical_height = 650 + homes.len().div_ceil(columns).saturating_sub(1) * 210;
    let maximum = u32::from(edge_length.saturating_sub(1));
    logical
        .into_iter()
        .map(|(room_id, (x, y))| {
            (
                room_id,
                (
                    ((x * maximum + 500) / 1000) as u16,
                    ((y * maximum + logical_height as u32 / 2) / logical_height as u32) as u16,
                ),
            )
        })
        .collect()
}

pub fn room_for_cell(edge_length: u16, rooms: &[RoomState], x: u16, y: u16) -> Option<RoomId> {
    room_grid_positions(edge_length, rooms)
        .into_iter()
        .min_by_key(|(room_id, (room_x, room_y))| {
            let dx = i64::from(*room_x) - i64::from(x);
            let dy = i64::from(*room_y) - i64::from(y);
            (dx * dx + dy * dy, *room_id)
        })
        .map(|(room_id, _)| room_id)
}

pub fn seed_cell(edge_length: u16, x: u16, y: u16) -> BogCellState {
    let id = cell_id(edge_length, x, y).expect("seed coordinates are inside the bog");
    let ridge = ((u32::from(x) * 17 + u32::from(y) * 11 + u32::from(x * y)) % 31) as i16;
    let north_south =
        (i16::try_from(y).unwrap_or(0) - i16::try_from(edge_length / 2).unwrap_or(0)) / 2;
    let water_table_mm = (-18 - ridge + north_south).clamp(-80, 18);
    BogCellState {
        id,
        x,
        y,
        water_table_mm,
        moisture: moisture_from_water_table(water_table_mm),
        ph_cent: (390 + ((u32::from(x) * 13 + u32::from(y) * 7) % 95)) as u16,
        nutrients: (18 + ((u32::from(x) * 5 + u32::from(y) * 3) % 22)) as u8,
        temperature_c: 13,
        light: (62 + ((u32::from(x) * 3 + u32::from(y) * 5) % 30)) as u8,
        peat_depth_mm: (450 + ((u32::from(x) * 29 + u32::from(y) * 19) % 700)) as u16,
        shrub_cover: ((u32::from(x) * 7 + u32::from(y) * 11) % 18) as u8,
        next_transition_at: 1 + id.0 % CELL_TRANSITION_HOURS,
    }
}

pub fn seed_organism(
    id: BogOrganismId,
    total_cells: u64,
    cell_lookup: impl Fn(BogCellId) -> BogCellState,
) -> BogOrganismState {
    let cell_id = BogCellId((id.0.wrapping_mul(37).wrapping_add(17) % total_cells) + 1);
    let cell = cell_lookup(cell_id);
    let wetness_band = if cell.water_table_mm >= -15 {
        0
    } else if cell.water_table_mm >= -35 {
        1
    } else {
        2
    };
    let candidates: &[usize] = match wetness_band {
        0 => &[0, 1, 2, 3, 8],
        1 => &[0, 1, 4, 5, 6, 8, 9],
        _ => &[5, 6, 7, 9, 10, 11],
    };
    let species_index = if id.0 <= SPECIES.len() as u64 {
        id.0 as usize - 1
    } else {
        candidates[(id.0 as usize * 7 + cell_id.0 as usize) % candidates.len()]
    };
    let species = SPECIES[species_index];
    let suitability = habitat_suitability(species, &cell);
    BogOrganismState {
        id,
        species: species.name.to_string(),
        cell_id,
        health: (55 + suitability / 2 + (id.0 % 12) as i16).clamp(20, 100),
        biomass_g: 20
            + (id.0.wrapping_mul(43) % u64::from(species.maximum_biomass_g / 4 + 1)) as u32,
        age_days: (id.0.wrapping_mul(13) % 1_500) as u32,
        stage: BogLifeStage::Growing,
        next_transition_at: 1 + id.0 % ORGANISM_TRANSITION_HOURS,
    }
}

pub fn update_cell(
    mut cell: BogCellState,
    clock: &WorldClock,
    neighbor_water_tables: &[i16],
    total_biomass_g: u64,
    peat_builders: usize,
) -> BogCellState {
    let rainfall_mm = match clock.weather {
        Weather::HeavyRain => 10,
        Weather::LightRain => 4,
        Weather::Mist => 1,
        Weather::Cloudy | Weather::Clear => 0,
    };
    let evaporation_mm = match clock.weather {
        Weather::Clear => 4,
        Weather::Cloudy => 2,
        Weather::Mist => 1,
        Weather::LightRain | Weather::HeavyRain => 0,
    } + i16::from(cell.light > 80);
    let neighbor_mean = if neighbor_water_tables.is_empty() {
        cell.water_table_mm
    } else {
        neighbor_water_tables.iter().copied().sum::<i16>()
            / i16::try_from(neighbor_water_tables.len()).unwrap_or(1)
    };
    let lateral_flow_mm = (neighbor_mean - cell.water_table_mm).clamp(-16, 16) / 4;
    let plant_uptake_mm = i16::try_from((total_biomass_g / 8_000).min(4)).unwrap_or(4);
    cell.water_table_mm = (cell.water_table_mm + rainfall_mm - evaporation_mm + lateral_flow_mm
        - plant_uptake_mm)
        .clamp(-120, 35);
    cell.moisture = moisture_from_water_table(cell.water_table_mm);
    cell.temperature_c = clock.temperature_c - i16::from(cell.moisture > 85);
    let weather_light: u8 = match clock.weather {
        Weather::Clear => 100,
        Weather::Cloudy => 72,
        Weather::LightRain => 58,
        Weather::HeavyRain => 44,
        Weather::Mist => 50,
    };
    cell.light = weather_light.saturating_sub(cell.shrub_cover / 2);
    if clock.now.is_multiple_of(168) && cell.water_table_mm >= -35 && peat_builders > 0 {
        cell.peat_depth_mm = cell.peat_depth_mm.saturating_add(1);
    }
    if matches!(clock.season, Season::Summer | Season::Autumn) && cell.water_table_mm < -55 {
        cell.shrub_cover = cell.shrub_cover.saturating_add(1).min(100);
    } else if cell.water_table_mm > 5 {
        cell.shrub_cover = cell.shrub_cover.saturating_sub(1);
    }
    cell.next_transition_at = clock.now + CELL_TRANSITION_HOURS;
    cell
}

pub fn update_organism(
    mut organism: BogOrganismState,
    cell: &BogCellState,
    clock: &WorldClock,
) -> BogOrganismState {
    let species = profile(&organism.species);
    let suitability = habitat_suitability(*species, cell);
    let competition = i16::from(cell.shrub_cover) / if species.shrub { 8 } else { 4 };
    let health_delta = ((suitability - 58) / 8 - competition).clamp(-12, 6);
    organism.health = (organism.health + health_delta).clamp(0, 100);
    organism.age_days = organism.age_days.saturating_add(1);

    if organism.health == 0 {
        organism.stage = BogLifeStage::Dead;
        organism.biomass_g = organism
            .biomass_g
            .saturating_sub((organism.biomass_g / 12).max(1));
    } else {
        let seasonal_growth = match clock.season {
            Season::Spring | Season::Summer => 100,
            Season::Autumn => 45,
            Season::Winter => 8,
        };
        let growth = u32::from(species.growth_g_per_day)
            * u32::try_from(suitability.max(0)).unwrap_or(0)
            * seasonal_growth
            / 10_000;
        organism.biomass_g = (organism.biomass_g + growth).min(species.maximum_biomass_g);
        organism.stage = if clock.season == species.flowers_in && organism.health >= 65 {
            BogLifeStage::Flowering
        } else if matches!(clock.season, Season::Winter) {
            BogLifeStage::Dormant
        } else if organism.age_days < 30 {
            BogLifeStage::Establishing
        } else {
            BogLifeStage::Growing
        };
    }
    organism.next_transition_at = clock.now + ORGANISM_TRANSITION_HOURS;
    organism
}

pub fn habitat_suitability(species: SpeciesProfile, cell: &BogCellState) -> i16 {
    let water_distance = (cell.water_table_mm - species.ideal_water_table_mm).unsigned_abs();
    let water = 100
        - i16::try_from(
            u32::from(water_distance) * 100
                / u32::from(species.water_tolerance_mm.unsigned_abs().max(1)),
        )
        .unwrap_or(100);
    let ph_distance = cell.ph_cent.abs_diff(species.ideal_ph_cent);
    let ph = 100
        - i16::try_from(u32::from(ph_distance) * 100 / u32::from(species.ph_tolerance_cent.max(1)))
            .unwrap_or(100);
    let light = if cell.light >= species.minimum_light {
        100
    } else {
        100 - i16::from(species.minimum_light - cell.light) * 3
    };
    water
        .clamp(0, 100)
        .min(ph.clamp(0, 100))
        .min(light.clamp(0, 100))
}

pub fn moisture_from_water_table(water_table_mm: i16) -> u8 {
    (95 - (-water_table_mm).max(0) * 2 / 3).clamp(5, 100) as u8
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    #[test]
    fn wet_weather_raises_the_water_table_and_dry_weather_lowers_it() {
        let cell = seed_cell(4, 1, 1);
        let mut wet_clock = WorldClock {
            now: 12,
            season: Season::Spring,
            weather: Weather::HeavyRain,
            temperature_c: 12,
        };
        let wet = update_cell(cell.clone(), &wet_clock, &[cell.water_table_mm], 0, 1);
        assert!(wet.water_table_mm > cell.water_table_mm);

        wet_clock.weather = Weather::Clear;
        wet_clock.temperature_c = 18;
        let dry = update_cell(cell.clone(), &wet_clock, &[cell.water_table_mm], 20_000, 0);
        assert!(dry.water_table_mm < cell.water_table_mm);
    }

    #[test]
    fn habitat_suitability_distinguishes_wetland_and_dryland_species() {
        let mut wet = seed_cell(4, 1, 1);
        wet.water_table_mm = -5;
        wet.ph_cent = 400;
        wet.light = 80;
        assert!(
            habitat_suitability(*profile("sphagnum moss"), &wet)
                > habitat_suitability(*profile("downy birch"), &wet)
        );
    }

    #[test]
    fn global_grid_gives_every_room_a_region() {
        let mut rooms = [
            RoomKind::Gate,
            RoomKind::CommonPath,
            RoomKind::Glasshouse,
            RoomKind::MoonBed,
            RoomKind::Pond,
            RoomKind::Compost,
            RoomKind::WildEdge,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| RoomState {
            id: RoomId(index as u64 + 1),
            name: format!("{kind:?}"),
            description: String::new(),
            kind,
            garden_id: None,
            exits: BTreeMap::new(),
        })
        .collect::<Vec<_>>();
        for index in 0..5_u64 {
            let home_id = RoomId(100 + index * 2);
            let gate_id = RoomId(home_id.0 + 1);
            rooms.push(RoomState {
                id: home_id,
                name: format!("home {index}"),
                description: String::new(),
                kind: RoomKind::HomeGarden,
                garden_id: None,
                exits: BTreeMap::from([("out".to_string(), gate_id)]),
            });
            rooms.push(RoomState {
                id: gate_id,
                name: format!("gate {index}"),
                description: String::new(),
                kind: RoomKind::GardenGate,
                garden_id: None,
                exits: BTreeMap::from([("in".to_string(), home_id)]),
            });
        }

        let positions = room_grid_positions(24, &rooms);
        assert_eq!(positions.len(), rooms.len());
        assert_eq!(
            positions.values().copied().collect::<BTreeSet<_>>().len(),
            rooms.len()
        );

        let mut covered = BTreeSet::new();
        for y in 0..24 {
            for x in 0..24 {
                if let Some(room_id) = room_for_cell(24, &rooms, x, y) {
                    covered.insert(room_id);
                }
            }
        }
        assert_eq!(covered.len(), rooms.len());
    }
}
