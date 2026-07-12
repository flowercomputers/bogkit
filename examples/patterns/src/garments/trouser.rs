//! A classic straight trouser block, drafted flat from body measurements.
//!
//! Both legs share four pieces: front and back (cut mirrored pairs) plus a
//! straight waistband. The draft follows the usual block logic: quarter-hip
//! panels with the back an inch wider than the front, crotch extensions of
//! 1/16 (front) and 1/8 (back) of the eased hip, a raised and slanted back
//! waist, and legs tapered hip -> knee -> hem around a vertical crease line.
//! No darts and no seam allowance in v0 — this is the sloper you iterate on.

use crate::garment::{Garment, Pattern, Piece};
use crate::geometry::{Path, pt};
use crate::measurements::Measurements;

/// Total circumference ease added around the leg at knee and hem.
const LEG_EASE: f64 = 4.0;
/// The hip line sits this far above the crotch line.
const HIP_DROP: f64 = 7.0;
/// Front fly slants in this much at the waist.
const CF_INSET: f64 = 1.0;
/// Center back slants in this much at the waist...
const CB_INSET: f64 = 3.0;
/// ...while rising this much above the natural waist line.
const CB_RISE: f64 = 2.5;
/// Back crotch point drops below the front's for seat room.
const BACK_CROTCH_DROP: f64 = 1.0;
/// Waistband overlap for the closure.
const WAISTBAND_STAND: f64 = 3.0;
/// Waistband cut height (folds to half).
const WAISTBAND_HEIGHT: f64 = 8.0;

pub struct Trouser;

/// Vertical landmarks shared by front and back, y-down from the waist line.
struct Levels {
    hip: f64,
    crotch: f64,
    knee: f64,
    hem: f64,
}

impl Levels {
    fn from(m: &Measurements) -> Self {
        let crotch = m.body_rise;
        Levels {
            hip: crotch - HIP_DROP,
            crotch,
            knee: crotch + m.inseam / 2.0,
            hem: crotch + m.inseam,
        }
    }
}

impl Garment for Trouser {
    fn name(&self) -> &str {
        "trouser block"
    }

    fn draft(&self, m: &Measurements) -> Pattern {
        Pattern {
            garment: self.name().to_string(),
            pieces: vec![front(m), back(m), waistband(m)],
        }
    }
}

/// Front: side seam at x=0, center front at the right, inseam beyond it.
fn front(m: &Measurements) -> Piece {
    let lv = Levels::from(m);
    let eased_hip = m.hip + m.hip_ease;

    let hip_w = eased_hip / 4.0 - 1.0;
    let waist_w = (m.waist + m.waist_ease) / 4.0 - 1.0;
    let ext = eased_hip / 16.0;

    let crease = (hip_w + ext) / 2.0;
    let knee_w = (m.knee + LEG_EASE) / 2.0 - 1.0;
    let hem_w = (m.ankle + LEG_EASE) / 2.0 - 1.0;

    let cf_waist = pt(hip_w - CF_INSET, 0.0);
    let side_waist = pt(cf_waist.x - waist_w, 0.0);
    let cf_hip = pt(hip_w, lv.hip);
    let crotch_pt = pt(hip_w + ext, lv.crotch);

    let outline = Path::new(side_waist)
        .line_to(cf_waist)
        .line_to(cf_hip)
        .bulge_to(crotch_pt, -2.0)
        .bulge_to(pt(crease + knee_w / 2.0, lv.knee), -0.75)
        .line_to(pt(crease + hem_w / 2.0, lv.hem))
        .line_to(pt(crease - hem_w / 2.0, lv.hem))
        .line_to(pt(crease - knee_w / 2.0, lv.knee))
        .bulge_to(pt(0.0, lv.hip), 0.75)
        .bulge_to(side_waist, 0.5);

    Piece {
        name: "front".to_string(),
        outline,
        grainline: (pt(crease, lv.hip + 5.0), pt(crease, lv.hem - 5.0)),
        cut_count: 2,
    }
}

/// Back: same orientation as the front (center back at the right), with a
/// wider panel, deeper crotch extension, and a raised slanted waist.
fn back(m: &Measurements) -> Piece {
    let lv = Levels::from(m);
    let eased_hip = m.hip + m.hip_ease;

    let hip_w = eased_hip / 4.0 + 1.0;
    let waist_w = (m.waist + m.waist_ease) / 4.0 + 1.0;
    let ext = eased_hip / 8.0;

    let crease = (hip_w + ext) / 2.0;
    let knee_w = (m.knee + LEG_EASE) / 2.0 + 1.0;
    let hem_w = (m.ankle + LEG_EASE) / 2.0 + 1.0;

    let cb_waist = pt(hip_w - CB_INSET, -CB_RISE);
    // land the side waist point on the natural waist line, exactly
    // `waist_w` away from the raised CB point so the seam measures true
    let side_waist = pt(
        cb_waist.x - (waist_w * waist_w - CB_RISE * CB_RISE).sqrt(),
        0.0,
    );
    let cb_hip = pt(hip_w, lv.hip);
    let crotch_pt = pt(hip_w + ext, lv.crotch + BACK_CROTCH_DROP);

    let outline = Path::new(side_waist)
        .line_to(cb_waist)
        .line_to(cb_hip)
        .bulge_to(crotch_pt, -3.0)
        .bulge_to(pt(crease + knee_w / 2.0, lv.knee), -1.0)
        .line_to(pt(crease + hem_w / 2.0, lv.hem))
        .line_to(pt(crease - hem_w / 2.0, lv.hem))
        .line_to(pt(crease - knee_w / 2.0, lv.knee))
        .bulge_to(pt(0.0, lv.hip), 0.75)
        .bulge_to(side_waist, 0.5);

    Piece {
        name: "back".to_string(),
        outline,
        grainline: (pt(crease, lv.hip + 5.0), pt(crease, lv.hem - 5.0)),
        cut_count: 2,
    }
}

/// Straight waistband cut on the fold: full eased waist plus a button stand.
fn waistband(m: &Measurements) -> Piece {
    let len = m.waist + m.waist_ease + WAISTBAND_STAND;
    let outline = Path::new(pt(0.0, 0.0))
        .line_to(pt(len, 0.0))
        .line_to(pt(len, WAISTBAND_HEIGHT))
        .line_to(pt(0.0, WAISTBAND_HEIGHT))
        .line_to(pt(0.0, 0.0));

    Piece {
        name: "waistband".to_string(),
        outline,
        grainline: (pt(3.0, WAISTBAND_HEIGHT / 2.0), pt(len - 3.0, WAISTBAND_HEIGHT / 2.0)),
        cut_count: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Point;
    use crate::svg::render_svg;

    fn m() -> Measurements {
        Measurements::default()
    }

    /// End point of segment `i` of a piece's outline.
    fn seg_end(piece: &Piece, i: usize) -> Point {
        piece.outline.segments[i].end()
    }

    #[test]
    fn waist_seams_sum_to_eased_waist() {
        let meas = m();
        // the waist seam is each piece's first segment; two fronts and two
        // backs together must measure the full eased waist
        let f = front(&meas);
        let b = back(&meas);
        let front_waist = f.outline.start.dist(seg_end(&f, 0));
        let back_waist = b.outline.start.dist(seg_end(&b, 0));
        let total = 2.0 * (front_waist + back_waist);
        assert!((total - (meas.waist + meas.waist_ease)).abs() < 1e-9);
    }

    #[test]
    fn hem_openings_sum_to_eased_ankle() {
        let meas = m();
        let f = front(&meas);
        let b = back(&meas);
        // hem edge runs between the ends of segments 4 and 5
        let front_hem = seg_end(&f, 4).dist(seg_end(&f, 5));
        let back_hem = seg_end(&b, 4).dist(seg_end(&b, 5));
        assert!((front_hem + back_hem - (meas.ankle + LEG_EASE)).abs() < 1e-9);
    }

    #[test]
    fn crotch_sits_at_body_rise() {
        let meas = m();
        let f = front(&meas);
        let b = back(&meas);
        // the crotch point ends segment 2 on both pieces
        assert!((seg_end(&f, 2).y - meas.body_rise).abs() < 1e-9);
        assert!((seg_end(&b, 2).y - (meas.body_rise + BACK_CROTCH_DROP)).abs() < 1e-9);
    }

    #[test]
    fn leg_reaches_full_inseam_below_crotch() {
        let meas = m();
        let f = front(&meas);
        let (_, max) = f.outline.bbox();
        assert!((max.y - (meas.body_rise + meas.inseam)).abs() < 1e-9);
    }

    #[test]
    fn waistband_measures_waist_plus_ease_and_stand() {
        let meas = m();
        let wb = waistband(&meas);
        let (min, max) = wb.outline.bbox();
        assert!((max.x - min.x - (meas.waist + meas.waist_ease + WAISTBAND_STAND)).abs() < 1e-9);
        assert!((max.y - min.y - WAISTBAND_HEIGHT).abs() < 1e-9);
    }

    #[test]
    fn draft_is_deterministic() {
        let meas = m();
        let a = render_svg(&Trouser.draft(&meas));
        let b = render_svg(&Trouser.draft(&meas));
        assert_eq!(a, b, "equal measurements must render byte-identical svg");

        let mut bigger = meas;
        bigger.hip += 5.0;
        let c = render_svg(&Trouser.draft(&bigger));
        assert_ne!(a, c, "different measurements must change the draft");
    }
}
