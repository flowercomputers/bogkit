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

/// The full battleground catalog, in file order — embedded into the index
/// page so the client can search it without a round trip.
pub fn all() -> &'static [Park] {
    static PARKS: OnceLock<Vec<Park>> = OnceLock::new();
    PARKS.get_or_init(|| serde_json::from_str(PARKS_JSON).expect("data/nyc_parks.json"))
}

/// Looks up a park by id, as chosen through the client's search menu
/// (`ClientMsg::ConfigureBattle`).
pub fn find_by_id(id: &str) -> Option<Park> {
    all().iter().find(|p| p.id == id).cloned()
}
