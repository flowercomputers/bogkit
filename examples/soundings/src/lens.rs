//! Lens authoring — runtime lenses live as KEYS, not code.
//!
//! The operator graph is compile-time static, but data keys are dynamic
//! (the master plan's load-bearing fix): a user-authored lens is a row in
//! the `defs` table plus one `(lens_id, sentence_id)` key per sentence in
//! the `lens_t` view. Authoring = one atomic backfill wtx over every
//! sentence; every later sentence edit mirrors into `(lens, sentence)` keys
//! for every lens, so all axes stay eagerly maintained beside the seeded one.
//!
//! Fold contract: the lens stream's `Map` must be deterministic per datum so
//! a retraction re-maps to the value that was inserted. Therefore a lens
//! definition is IMMUTABLE once created (a new lens = a new id), and a lens
//! is deleted by retracting all its keys while its axis is still in the
//! registry — only then is the axis dropped.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::serve::Axis;

/// A user-authored axis: two anchor SENTENCES (never words — the G4 rule)
/// and a display name. Persisted in the `defs` table, id-keyed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LensDef {
    pub id: u32,
    pub name: String,
    pub a: String,
    pub b: String,
}

/// Axes by lens id, shared with the lens stream's `Map` closure. Written
/// only by the doc thread (author: insert before backfill; delete: remove
/// after the retract wtx); read inside the pipeline.
pub type LensRegistry = Arc<RwLock<HashMap<u32, Axis>>>;

pub fn axis_for(def: &LensDef) -> Axis {
    Axis::from_anchors(&[def.a.as_str()], &[def.b.as_str()])
}

/// The lens stream's scoring function: project `text` onto the axis of
/// `lens_id`. Pure in (lens_id, text) as long as the registry entry is
/// immutable — see the module docs. A missing axis (cannot happen in the
/// author/delete protocol above) scores the midpoint.
pub fn lens_t(registry: &LensRegistry, lens_id: u32, text: &str) -> f32 {
    registry
        .read()
        .unwrap()
        .get(&lens_id)
        .map(|ax| ax.score(text).t)
        .unwrap_or(0.5)
}
