//! The framework contract: a garment is a pure function from measurements
//! to pattern pieces. Determinism is load-bearing — fold retracts a profile
//! by re-drafting it, so equal inputs must produce equal outputs.

use crate::geometry::{Path, Point};
use crate::measurements::Measurements;

pub trait Garment {
    fn name(&self) -> &str;
    /// Pure and deterministic: no clocks, no randomness, no globals.
    fn draft(&self, m: &Measurements) -> Pattern;
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub garment: String,
    pub pieces: Vec<Piece>,
}

#[derive(Debug, Clone)]
pub struct Piece {
    pub name: String,
    /// Closed outline in cm, y-down, local coordinates (layout happens at
    /// render time).
    pub outline: Path,
    /// Drawn as an arrow; runs along the intended fabric grain.
    pub grainline: (Point, Point),
    /// How many times to cut this piece (mirrored pairs count as 2).
    pub cut_count: u8,
}
