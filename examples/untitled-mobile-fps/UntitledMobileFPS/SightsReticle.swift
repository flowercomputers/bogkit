import SwiftUI

struct SightsReticle: View {
    var style: ReticleStyle = .default

    // Display-only geometry for this reticle. The arms start outside the ring
    // so the centre of the frame — where the target is — stays uncovered.
    private let ringRadius: CGFloat = 17
    private let armInnerGap: CGFloat = 24
    private let armReach: CGFloat = 92
    private let dotRadius: CGFloat = 1.25
    /// Distance from centre and half-width of each rung of the tick ladder.
    private let tickLadder: [(offset: CGFloat, halfWidth: CGFloat)] = [
        (44, 4.5), (60, 3.5), (76, 2.5)
    ]

    var body: some View {
        Canvas { context, size in
            let center = CGPoint(x: size.width / 2, y: size.height / 2)

            context.strokeTactical(
                Path(ellipseIn: CGRect(
                    x: center.x - ringRadius,
                    y: center.y - ringRadius,
                    width: ringRadius * 2,
                    height: ringRadius * 2
                )),
                color: style.laser,
                width: style.primary,
                style: style
            )

            var arms = Path()
            arms.move(to: CGPoint(x: center.x, y: center.y - armReach))
            arms.addLine(to: CGPoint(x: center.x, y: center.y - armInnerGap))
            arms.move(to: CGPoint(x: center.x, y: center.y + armInnerGap))
            arms.addLine(to: CGPoint(x: center.x, y: center.y + armReach))
            arms.move(to: CGPoint(x: center.x - armReach, y: center.y))
            arms.addLine(to: CGPoint(x: center.x - armInnerGap, y: center.y))
            arms.move(to: CGPoint(x: center.x + armInnerGap, y: center.y))
            arms.addLine(to: CGPoint(x: center.x + armReach, y: center.y))
            context.strokeTactical(arms, color: style.laser, width: style.primary, style: style)

            var ticks = Path()
            for rung in tickLadder {
                ticks.move(to: CGPoint(x: center.x - rung.halfWidth, y: center.y - rung.offset))
                ticks.addLine(to: CGPoint(x: center.x + rung.halfWidth, y: center.y - rung.offset))
                ticks.move(to: CGPoint(x: center.x - rung.halfWidth, y: center.y + rung.offset))
                ticks.addLine(to: CGPoint(x: center.x + rung.halfWidth, y: center.y + rung.offset))
                ticks.move(to: CGPoint(x: center.x - rung.offset, y: center.y - rung.halfWidth))
                ticks.addLine(to: CGPoint(x: center.x - rung.offset, y: center.y + rung.halfWidth))
                ticks.move(to: CGPoint(x: center.x + rung.offset, y: center.y - rung.halfWidth))
                ticks.addLine(to: CGPoint(x: center.x + rung.offset, y: center.y + rung.halfWidth))
            }
            context.strokeTactical(
                ticks,
                color: style.laser.opacity(0.85),
                width: style.hairline,
                style: style
            )

            // Drop post inside the ring. Gives the sight an up/down orientation
            // at a glance without adding anything that covers the target.
            var post = Path()
            post.move(to: CGPoint(x: center.x, y: center.y + 5))
            post.addLine(to: CGPoint(x: center.x, y: center.y + ringRadius))
            context.strokeTactical(
                post,
                color: style.laser.opacity(0.7),
                width: style.hairline,
                style: style
            )

            context.fillTactical(dotAt: center, radius: dotRadius, color: style.laser, style: style)
        }
        .accessibilityHidden(true)
    }
}

/// Fills as the finger gun is drawn toward the phone. Without it the proximity
/// threshold is invisible and the player has to guess how close is close
/// enough; with it, the gesture teaches itself on the first attempt.
struct ScopeEntryIndicator: View {
    let progress: Double

    var body: some View {
        let clamped = min(max(progress, 0), 1)
        let track: CGFloat = 2.5
        VStack(spacing: 8) {
            ZStack {
                // Same idea as `strokeTactical`'s contrast pass: a dark backing
                // only slightly wider than the ring, rather than a fat halo.
                Circle()
                    .stroke(
                        .black.opacity(ReticleStyle.default.contrastOpacity),
                        lineWidth: track + ReticleStyle.default.contrastPadding
                    )
                Circle()
                    .stroke(.white.opacity(0.28), lineWidth: track)
                Circle()
                    .trim(from: 0, to: clamped)
                    .stroke(
                        ReticleStyle.default.laser.opacity(0.55 + 0.45 * clamped),
                        style: StrokeStyle(lineWidth: track, lineCap: .round)
                    )
                    .rotationEffect(.degrees(-90))
                Image(systemName: "scope")
                    .font(.system(size: 20, weight: .semibold))
                    .foregroundStyle(.white.opacity(0.35 + 0.55 * clamped))
            }
            .frame(width: 54, height: 54)

            Text(clamped >= 1 ? "HOLD" : "PULL IN TO SCOPE")
                .font(.caption2.bold().monospaced())
                .foregroundStyle(.white.opacity(0.55 + 0.45 * clamped))
                .padding(.horizontal, 7)
                .padding(.vertical, 3)
                .background(.black.opacity(0.55), in: Capsule())
        }
        .padding(.bottom, 108)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
        .animation(.easeOut(duration: 0.12), value: clamped)
        .accessibilityHidden(true)
    }
}

/// The scoped state made unmistakable.
///
/// The first device test reported that it was "really hard to tell whether
/// you're in sights mode", and the honest reason was that almost nothing said
/// so: the mode line lived in a debug overlay that is hidden during a match,
/// the plain crosshair was suppressed during a match to avoid stacking with the
/// gameplay reticle, and a 1.25x zoom is far too subtle to read as a state
/// change. This is the chrome that does say so, and it deliberately reads at a
/// glance and from across a room: a vignette that darkens the frame edges,
/// brackets that close in on the centre, and a badge.
///
/// It draws no crosshair of its own, so it can be layered over the gameplay
/// reticle during a match without stacking two of them.
struct SightsFrameOverlay: View {
    var style: ReticleStyle = .default

    var body: some View {
        GeometryReader { proxy in
            let size = proxy.size
            let inset = min(size.width, size.height) * 0.06
            let bracket = min(size.width, size.height) * 0.09
            ZStack {
                // Vignette: unmistakable at a glance without hiding the target.
                RadialGradient(
                    colors: [.clear, .clear, .black.opacity(0.30), .black.opacity(0.62)],
                    center: .center,
                    startRadius: min(size.width, size.height) * 0.16,
                    endRadius: max(size.width, size.height) * 0.62
                )
                .allowsHitTesting(false)

                Canvas { context, canvasSize in
                    let rect = CGRect(origin: .zero, size: canvasSize).insetBy(dx: inset, dy: inset)
                    var path = Path()
                    for (corner, dx, dy) in [
                        (CGPoint(x: rect.minX, y: rect.minY), 1.0, 1.0),
                        (CGPoint(x: rect.maxX, y: rect.minY), -1.0, 1.0),
                        (CGPoint(x: rect.minX, y: rect.maxY), 1.0, -1.0),
                        (CGPoint(x: rect.maxX, y: rect.maxY), -1.0, -1.0)
                    ] {
                        path.move(to: CGPoint(x: corner.x + dx * bracket, y: corner.y))
                        path.addLine(to: corner)
                        path.addLine(to: CGPoint(x: corner.x, y: corner.y + dy * bracket))
                    }
                    // Brackets carry the same treatment as the reticles, so the
                    // scoped frame reads as one instrument rather than as two
                    // overlays drawn to different rules.
                    context.strokeTactical(
                        path,
                        color: style.laser,
                        width: style.primary,
                        style: style
                    )
                }
            }
        }
        .accessibilityHidden(true)
    }
}

/// Always-visible mode badge. Unlike the debug HUD's `MODE` row, this is part of
/// gameplay chrome, so the player can tell which mode they are in during a match.
struct AimingModeBadge: View {
    let mode: AimingMode

    var body: some View {
        let scoped = mode == .sights
        HStack(spacing: 6) {
            Image(systemName: scoped ? "scope" : "hand.raised.fill")
                .font(.system(size: 12, weight: .bold))
            Text(scoped ? "SIGHTS" : "HIP")
                .font(.caption2.bold().monospaced())
        }
        .foregroundStyle(scoped ? .white : .white.opacity(0.75))
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(
            Capsule().fill(scoped ? Color.red.opacity(0.85) : Color.black.opacity(0.55))
        )
        .overlay(
            Capsule().stroke(scoped ? Color.white.opacity(0.7) : Color.white.opacity(0.18), lineWidth: 1)
        )
        .accessibilityLabel(scoped ? "Sights mode" : "Hip fire mode")
    }
}
