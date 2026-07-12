//! Minimal 2d geometry for pattern drafting. Units are centimeters, y grows
//! downward (like the drafting table and like svg).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

pub fn pt(x: f64, y: f64) -> Point {
    Point { x, y }
}

impl Point {
    pub fn lerp(self, other: Point, t: f64) -> Point {
        pt(
            self.x + (other.x - self.x) * t,
            self.y + (other.y - self.y) * t,
        )
    }

    pub fn dist(self, other: Point) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// One segment of an outline; the start point is implicit (the previous
/// segment's end, or the path's start).
#[derive(Debug, Clone, Copy)]
pub enum Segment {
    LineTo(Point),
    /// Cubic bezier: two control points, then the end point.
    CubicTo(Point, Point, Point),
}

impl Segment {
    pub fn end(&self) -> Point {
        match *self {
            Segment::LineTo(p) => p,
            Segment::CubicTo(_, _, p) => p,
        }
    }
}

/// A closed outline: a start point followed by segments. Rendering closes
/// the path back to the start.
#[derive(Debug, Clone)]
pub struct Path {
    pub start: Point,
    pub segments: Vec<Segment>,
}

/// Number of chords used to approximate each bezier's length; fixed so
/// measurements are deterministic.
const BEZIER_STEPS: usize = 64;

fn cubic_at(p0: Point, c1: Point, c2: Point, p1: Point, t: f64) -> Point {
    let a = p0.lerp(c1, t);
    let b = c1.lerp(c2, t);
    let c = c2.lerp(p1, t);
    let d = a.lerp(b, t);
    let e = b.lerp(c, t);
    d.lerp(e, t)
}

impl Path {
    pub fn new(start: Point) -> Self {
        Path {
            start,
            segments: Vec::new(),
        }
    }

    pub fn line_to(mut self, p: Point) -> Self {
        self.segments.push(Segment::LineTo(p));
        self
    }

    pub fn cubic_to(mut self, c1: Point, c2: Point, p: Point) -> Self {
        self.segments.push(Segment::CubicTo(c1, c2, p));
        self
    }

    /// Curve to `p` bulging sideways from the straight chord: positive
    /// `bulge` bows to the left of travel, negative to the right. Handy for
    /// gentle seam curves without hand-placing control points.
    pub fn bulge_to(self, p: Point, bulge: f64) -> Self {
        let start = self.current();
        let chord = pt(p.x - start.x, p.y - start.y);
        let len = start.dist(p).max(1e-9);
        // unit normal, left of travel in y-down coordinates
        let n = pt(chord.y / len, -chord.x / len);
        let c1 = pt(
            start.x + chord.x / 3.0 + n.x * bulge,
            start.y + chord.y / 3.0 + n.y * bulge,
        );
        let c2 = pt(
            start.x + chord.x * 2.0 / 3.0 + n.x * bulge,
            start.y + chord.y * 2.0 / 3.0 + n.y * bulge,
        );
        self.cubic_to(c1, c2, p)
    }

    /// Where the next segment will start.
    pub fn current(&self) -> Point {
        self.segments.last().map(|s| s.end()).unwrap_or(self.start)
    }

    /// Total length of the outline (not counting the implicit closing edge).
    /// Only exercised by tests today, but it's the primitive future garments
    /// need for walking seam lengths (sleeve caps, eased seams).
    #[allow(dead_code)]
    pub fn length(&self) -> f64 {
        let mut total = 0.0;
        let mut cur = self.start;
        for seg in &self.segments {
            total += match *seg {
                Segment::LineTo(p) => cur.dist(p),
                Segment::CubicTo(c1, c2, p) => {
                    let mut len = 0.0;
                    let mut prev = cur;
                    for i in 1..=BEZIER_STEPS {
                        let t = i as f64 / BEZIER_STEPS as f64;
                        let q = cubic_at(cur, c1, c2, p, t);
                        len += prev.dist(q);
                        prev = q;
                    }
                    len
                }
            };
            cur = seg.end();
        }
        total
    }

    /// Axis-aligned bounding box `(min, max)`, sampling curves.
    pub fn bbox(&self) -> (Point, Point) {
        let mut min = self.start;
        let mut max = self.start;
        let mut grow = |p: Point| {
            min = pt(min.x.min(p.x), min.y.min(p.y));
            max = pt(max.x.max(p.x), max.y.max(p.y));
        };
        let mut cur = self.start;
        for seg in &self.segments {
            match *seg {
                Segment::LineTo(p) => grow(p),
                Segment::CubicTo(c1, c2, p) => {
                    for i in 1..=BEZIER_STEPS {
                        let t = i as f64 / BEZIER_STEPS as f64;
                        grow(cubic_at(cur, c1, c2, p, t));
                    }
                }
            }
            cur = seg.end();
        }
        (min, max)
    }

    /// SVG path `d` attribute. Coordinates are formatted to a fixed two
    /// decimals so equal drafts render byte-identical documents.
    pub fn to_svg_d(&self) -> String {
        use std::fmt::Write;
        let mut d = String::new();
        write!(d, "M {:.2} {:.2}", self.start.x, self.start.y).unwrap();
        for seg in &self.segments {
            match *seg {
                Segment::LineTo(p) => write!(d, " L {:.2} {:.2}", p.x, p.y).unwrap(),
                Segment::CubicTo(c1, c2, p) => write!(
                    d,
                    " C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                    c1.x, c1.y, c2.x, c2.y, p.x, p.y
                )
                .unwrap(),
            }
        }
        d.push_str(" Z");
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_lengths_are_exact() {
        let p = Path::new(pt(0.0, 0.0)).line_to(pt(3.0, 4.0));
        assert!((p.length() - 5.0).abs() < 1e-12);
    }

    #[test]
    fn bulge_zero_matches_chord_length() {
        let straight = Path::new(pt(0.0, 0.0)).line_to(pt(10.0, 0.0));
        let curved = Path::new(pt(0.0, 0.0)).bulge_to(pt(10.0, 0.0), 0.0);
        assert!((straight.length() - curved.length()).abs() < 1e-6);
    }

    #[test]
    fn bulged_curve_is_longer_than_chord() {
        let curved = Path::new(pt(0.0, 0.0)).bulge_to(pt(10.0, 0.0), 1.0);
        assert!(curved.length() > 10.0);
    }
}
