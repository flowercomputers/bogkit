//! Body measurements and the profiles that carry them through fold.
//! All fields are centimeters, taken snug against the body; ease is applied
//! by the draft, not baked into the measurements.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Measurements {
    /// Around the natural waist.
    pub waist: f64,
    /// Around the fullest part of the seat.
    pub hip: f64,
    /// Waist to chair when seated: waistband to crotch depth.
    pub body_rise: f64,
    /// Crotch to floor along the inside of the leg.
    pub inseam: f64,
    /// Around the knee.
    pub knee: f64,
    /// Around the ankle (or desired hem opening).
    pub ankle: f64,
    /// Wearing ease added to the hip circumference by the draft.
    pub hip_ease: f64,
    /// Wearing ease added to the waist circumference by the draft.
    pub waist_ease: f64,
}

impl Default for Measurements {
    fn default() -> Self {
        // a middle-of-the-chart size, so the app boots with a real draft
        Measurements {
            waist: 80.0,
            hip: 100.0,
            body_rise: 28.0,
            inseam: 78.0,
            knee: 40.0,
            ankle: 24.0,
            hip_ease: 4.0,
            waist_ease: 2.0,
        }
    }
}

/// A named measurement set; the unit of storage in fold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: u64,
    pub name: String,
    pub m: Measurements,
}
