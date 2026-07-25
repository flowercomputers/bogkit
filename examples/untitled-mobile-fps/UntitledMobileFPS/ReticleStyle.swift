import SwiftUI

/// Shared weights and colours for every aiming overlay.
///
/// Hairline reticles only stay readable over a live camera image if all of them
/// use the same treatment, so the numbers live here rather than at each call
/// site. Display-only geometry that is unique to one overlay stays local to it.
struct ReticleStyle: Equatable, Sendable {
    /// Ticks and secondary marks.
    var hairline: CGFloat = 1.0
    /// Rings and crosshair arms.
    var primary: CGFloat = 1.4
    /// Added to the stroke width for the dark contrast pass. Half of it shows
    /// on each side of the line, so this is deliberately small.
    var contrastPadding: CGFloat = 1.2
    var contrastOpacity: Double = 0.55
    /// Added to the stroke width for the blurred bloom pass.
    var bloomPadding: CGFloat = 3
    var bloomRadius: CGFloat = 3
    var bloomOpacity: Double = 0.3
    /// Extra bloom around a filled dot, as a multiple of its radius.
    var dotBloomScale: CGFloat = 2.6
    var laser = Color(red: 1.0, green: 0.17, blue: 0.21)

    static let `default` = ReticleStyle()
}

extension GraphicsContext {
    /// Strokes `path` as a hairline that survives both a white wall and a dark
    /// room.
    ///
    /// Three passes, back to front: a blurred additive bloom for the emitted
    /// light of a laser sight, a narrow dark edge so the line holds against a
    /// bright background, then the stroke itself. The dark pass is what
    /// replaces the fat black halos these overlays used to carry — it buys the
    /// same contrast for roughly a sixth of the covered area.
    mutating func strokeTactical(
        _ path: Path,
        color: Color,
        width: CGFloat,
        style: ReticleStyle = .default,
        dash: [CGFloat] = []
    ) {
        drawLayer { layer in
            layer.addFilter(.blur(radius: style.bloomRadius))
            layer.blendMode = .plusLighter
            Self.stroke(
                path,
                in: &layer,
                color: color.opacity(style.bloomOpacity),
                width: width + style.bloomPadding,
                dash: dash
            )
        }
        Self.stroke(
            path,
            in: &self,
            color: .black.opacity(style.contrastOpacity),
            width: width + style.contrastPadding,
            dash: dash
        )
        Self.stroke(path, in: &self, color: color, width: width, dash: dash)
    }

    /// Draws the laser dot: a small solid core inside its own bloom.
    mutating func fillTactical(
        dotAt center: CGPoint,
        radius: CGFloat,
        color: Color,
        style: ReticleStyle = .default
    ) {
        let bloomRadius = radius * style.dotBloomScale
        drawLayer { layer in
            layer.addFilter(.blur(radius: style.bloomRadius))
            layer.blendMode = .plusLighter
            layer.fill(
                Path(ellipseIn: Self.square(around: center, radius: bloomRadius)),
                with: .color(color.opacity(style.bloomOpacity))
            )
        }
        stroke(
            Path(ellipseIn: Self.square(around: center, radius: radius)),
            with: .color(.black.opacity(style.contrastOpacity)),
            lineWidth: style.contrastPadding
        )
        fill(
            Path(ellipseIn: Self.square(around: center, radius: radius)),
            with: .color(color)
        )
    }

    private static func stroke(
        _ path: Path,
        in context: inout GraphicsContext,
        color: Color,
        width: CGFloat,
        dash: [CGFloat]
    ) {
        context.stroke(
            path,
            with: .color(color),
            style: StrokeStyle(lineWidth: width, lineCap: .round, dash: dash)
        )
    }

    private static func square(around center: CGPoint, radius: CGFloat) -> CGRect {
        CGRect(
            x: center.x - radius,
            y: center.y - radius,
            width: radius * 2,
            height: radius * 2
        )
    }
}
