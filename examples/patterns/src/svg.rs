//! Render a drafted [`Pattern`] to a single SVG document.
//!
//! User units are centimeters and the document declares its physical size in
//! mm, so printing at 100% scale yields a full-size pattern. All numbers are
//! formatted to two decimals, keeping equal drafts byte-identical.

use std::fmt::Write;

use crate::garment::{Pattern, Piece};
use crate::geometry::pt;

const MARGIN: f64 = 3.0; // cm between and around pieces

pub fn render_svg(pattern: &Pattern) -> String {
    // lay pieces out left to right, each normalized to its own origin
    struct Placed<'a> {
        piece: &'a Piece,
        dx: f64,
        dy: f64,
    }

    let mut placed = Vec::new();
    let mut cursor_x = MARGIN;
    let mut max_h: f64 = 0.0;
    for piece in &pattern.pieces {
        let (min, max) = piece.outline.bbox();
        placed.push(Placed {
            piece,
            dx: cursor_x - min.x,
            dy: MARGIN - min.y,
        });
        cursor_x += (max.x - min.x) + MARGIN;
        max_h = max_h.max(max.y - min.y);
    }
    let total_w = cursor_x;
    let total_h = max_h + 2.0 * MARGIN + 2.0; // room for labels below

    let mut svg = String::new();
    write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.2}mm" height="{h:.2}mm" viewBox="0 0 {vw:.2} {vh:.2}">"#,
        w = total_w * 10.0,
        h = total_h * 10.0,
        vw = total_w,
        vh = total_h,
    )
    .unwrap();
    svg.push_str(
        r##"<defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" fill="#555"/></marker></defs>"##,
    );
    write!(
        svg,
        r##"<text x="{x:.2}" y="1.8" font-size="1.0" fill="#8a8072" font-family="sans-serif">{name}</text>"##,
        x = MARGIN,
        name = pattern.garment,
    )
    .unwrap();

    for p in &placed {
        write!(
            svg,
            r#"<g transform="translate({dx:.2} {dy:.2})">"#,
            dx = p.dx,
            dy = p.dy
        )
        .unwrap();

        write!(
            svg,
            r##"<path d="{d}" fill="#f3ede2" stroke="#333" stroke-width="0.15"/>"##,
            d = p.piece.outline.to_svg_d()
        )
        .unwrap();

        let (g0, g1) = p.piece.grainline;
        write!(
            svg,
            r##"<line x1="{x1:.2}" y1="{y1:.2}" x2="{x2:.2}" y2="{y2:.2}" stroke="#555" stroke-width="0.1" marker-start="url(#arrow)" marker-end="url(#arrow)"/>"##,
            x1 = g0.x,
            y1 = g0.y,
            x2 = g1.x,
            y2 = g1.y,
        )
        .unwrap();

        // label sits under the piece, in the outline's local coordinates
        let (min, max) = p.piece.outline.bbox();
        let label_at = pt((min.x + max.x) / 2.0, max.y + 1.5);
        write!(
            svg,
            r##"<text x="{x:.2}" y="{y:.2}" font-size="1.2" text-anchor="middle" fill="#333" font-family="sans-serif">{name} — cut {n}</text>"##,
            x = label_at.x,
            y = label_at.y,
            name = p.piece.name,
            n = p.piece.cut_count,
        )
        .unwrap();

        svg.push_str("</g>");
    }

    svg.push_str("</svg>");
    svg
}
