use std::collections::BTreeMap;

use chrono::DateTime;
use serde::{Deserialize, Serialize};

pub type Minute = i64;
pub type CaregiverId = u32;
pub type VisitId = u64;

pub const REGIONS: u8 = 8;
pub const CERTIFICATIONS: u8 = 8;
pub const BASE_MINUTE: Minute = 29_541_900; // 2026-03-02T00:00:00-05:00

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Tiny,
    Representative,
}

impl Scale {
    pub fn counts(self) -> (u32, u64) {
        match self {
            Self::Tiny => (80, 480),
            Self::Representative => (2_000, 12_000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Interval {
    pub start: Minute,
    pub end: Minute,
}

impl Interval {
    pub fn contains(self, start: Minute, end: Minute) -> bool {
        self.start <= start && end <= self.end
    }

    pub fn overlaps(self, start: Minute, end: Minute) -> bool {
        self.start < end && start < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caregiver {
    pub id: CaregiverId,
    pub certification_mask: u16,
    pub region_mask: u16,
    pub availability: Vec<Interval>,
    pub required_rest: Vec<Interval>,
    pub max_minutes: Minute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    pub id: VisitId,
    pub client_id: u32,
    pub start: Minute,
    pub end: Minute,
    pub region: u8,
    pub required_certification: u8,
    pub urgency: u8,
    pub preferred_caregiver: Option<CaregiverId>,
    pub canceled: bool,
}

impl Visit {
    pub fn duration(&self) -> Minute {
        self.end - self.start
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum UnfilledReason {
    NoCertification,
    NoRegionCoverage,
    OutsideAvailability,
    RequiredRest,
    HourLimit,
    TravelConflict,
}

impl UnfilledReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoCertification => "NO_CERTIFICATION",
            Self::NoRegionCoverage => "NO_REGION_COVERAGE",
            Self::OutsideAvailability => "OUTSIDE_AVAILABILITY",
            Self::RequiredRest => "REQUIRED_REST",
            Self::HourLimit => "HOUR_LIMIT",
            Self::TravelConflict => "TRAVEL_CONFLICT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Assigned(CaregiverId),
    Unfilled(UnfilledReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub caregivers: BTreeMap<CaregiverId, Caregiver>,
    pub visits: BTreeMap<VisitId, Visit>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EntityKey {
    Caregiver(CaregiverId),
    Visit(VisitId),
    Outcome(VisitId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Record {
    Caregiver(Caregiver),
    Visit(Visit),
    Outcome { visit_id: VisitId, outcome: Outcome },
}

#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn range(&mut self, upper: u64) -> u64 {
        self.next() % upper
    }
}

pub fn parse_import_minute(value: &str) -> Result<Minute, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.timestamp() / 60)
        .map_err(|error| format!("timestamp must be RFC3339 with an explicit offset: {error}"))
}

pub fn generate(seed: u64, scale: Scale) -> State {
    let (caregiver_count, visit_count) = scale.counts();
    let mut rng = Rng::new(seed);
    let mut caregivers = BTreeMap::new();
    for id in 0..caregiver_count {
        let home_region = (id % u32::from(REGIONS)) as u8;
        let second_region = (home_region + 1 + (rng.range(2) as u8)) % REGIONS;
        let first_cert = rng.range(u64::from(CERTIFICATIONS)) as u8;
        let second_cert = (first_cert + 1 + rng.range(3) as u8) % CERTIFICATIONS;
        let mut availability = Vec::with_capacity(14);
        let mut required_rest = Vec::with_capacity(14);
        for day in 0..14 {
            let day_start = BASE_MINUTE + day * 1_440;
            availability.push(Interval {
                start: day_start + 6 * 60,
                end: day_start + 23 * 60,
            });
            required_rest.push(Interval {
                start: day_start + 22 * 60,
                end: day_start + 23 * 60,
            });
        }
        caregivers.insert(
            id,
            Caregiver {
                id,
                certification_mask: (1 << first_cert) | (1 << second_cert),
                region_mask: (1 << home_region) | (1 << second_region),
                availability,
                required_rest,
                max_minutes: 4 * 60,
            },
        );
    }

    let mut visits = BTreeMap::new();
    for id in 0..visit_count {
        let day = (id % 14) as i64;
        let slot = rng.range(60) as i64;
        let night_case = id % 97 == 0;
        let start = if night_case {
            BASE_MINUTE + day * 1_440 + 22 * 60
        } else {
            BASE_MINUTE + day * 1_440 + 7 * 60 + slot * 15
        };
        let required_certification = if id % 211 == 0 {
            15
        } else {
            rng.range(u64::from(CERTIFICATIONS)) as u8
        };
        let preferred = if id % 3 == 0 {
            Some(rng.range(u64::from(caregiver_count)) as u32)
        } else {
            None
        };
        visits.insert(
            id,
            Visit {
                id,
                client_id: (id % (visit_count / 5).max(1)) as u32,
                start,
                end: start + 45,
                region: rng.range(u64::from(REGIONS)) as u8,
                required_certification,
                urgency: (rng.range(4) + 1) as u8,
                preferred_caregiver: preferred,
                canceled: false,
            },
        );
    }

    State { caregivers, visits }
}

pub fn travel_minutes(from_region: u8, to_region: u8) -> Minute {
    let distance = (i64::from(from_region) - i64::from(to_region)).abs();
    10 + distance * 5
}
