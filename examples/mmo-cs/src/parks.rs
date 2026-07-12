//! The battleground catalog.
//!
//! `data/nyc_parks.json` is a curated extract of real Brooklyn/Queens parks
//! (name + bounding box) from NYC Open Data's public "Parks Properties"
//! dataset (data.cityofnewyork.us/resource/enfh-gkve, unauthenticated,
//! city-maintained) — used in place of the Foursquare OS Places dataset
//! named in the original spec, since Foursquare's copy now requires a
//! gated Hugging Face account/token this environment doesn't have.
//! Swap this file for a real Foursquare extract if/when that access is
//! available; the shape (`{id, name, bbox}`) is dataset-agnostic.
//!
//! Baked in via `include_str!` rather than read at runtime: the data needs
//! no build-time transformation, so this is the whole "build step" —
//! it keeps the compiled binary self-contained for deployment.

use std::sync::OnceLock;

use crate::domain::Park;

static PARKS_JSON: &str = include_str!("../data/nyc_parks.json");

fn parks() -> &'static [Park] {
    static PARKS: OnceLock<Vec<Park>> = OnceLock::new();
    PARKS.get_or_init(|| serde_json::from_str(PARKS_JSON).expect("data/nyc_parks.json"))
}

/// Pick a random park for a new battle. `MMO_PARK` can pin a specific park
/// by (case-insensitive, substring) name match, for a rehearsed demo.
pub fn pick_battleground() -> Park {
    let all = parks();

    if let Ok(want) = std::env::var("MMO_PARK") {
        let want = want.to_lowercase();
        if let Some(p) = all.iter().find(|p| p.name.to_lowercase().contains(&want)) {
            return p.clone();
        }
        eprintln!("MMO_PARK={want:?} matched no park in data/nyc_parks.json, picking randomly");
    }

    // simple xorshift seeded from the wall clock — no `rand` dependency
    // needed for picking one of ~20 parks
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let mut x = seed ^ 0x9E3779B97F4A7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;

    all[(x as usize) % all.len()].clone()
}
