import SwiftUI
import UIKit

struct TargetSilhouetteOverlay: View {
    let result: PersonTargetingResult
    let imageSize: CGSize
    let eliminated: Bool
    /// The opponent's chosen skin. Falls back to the pre-skins look when an
    /// opponent has none recorded.
    var skin: SilhouetteSkin = .fallback

    private var outlineColor: Color {
        eliminated ? .gray : skin.accent.color
    }

    var body: some View {
        GeometryReader { proxy in
            let geometry = PreviewGeometry(viewSize: proxy.size, imageSize: imageSize)
            let rect = geometry.rect(fromVisionNormalized: result.visionBoundingBox)
            ZStack {
                if let mask = result.maskImage {
                    // Fully opaque: the target reads as a marked silhouette
                    // rather than a tint over the real person. The pattern is
                    // tiled at a fixed screen size so it stays identifiable at
                    // any range.
                    Image(uiImage: SilhouetteSkinRenderer.tile(for: skin))
                        .resizable(resizingMode: .tile)
                        .frame(width: proxy.size.width, height: proxy.size.height)
                        .mask {
                            Image(uiImage: mask)
                                .resizable()
                                .aspectRatio(contentMode: .fill)
                                .frame(width: proxy.size.width, height: proxy.size.height)
                                .mask {
                                    Rectangle()
                                        .frame(width: rect.width, height: rect.height)
                                        .position(x: rect.midX, y: rect.midY)
                                }
                        }
                        .saturation(eliminated ? 0 : 1)
                        .opacity(
                            eliminated
                                ? SilhouetteSkinRenderer.eliminatedOpacity
                                : SilhouetteSkinRenderer.fillOpacity
                        )
                        .animation(.easeInOut(duration: 0.25), value: eliminated)
                }
                RoundedRectangle(cornerRadius: 8)
                    .stroke(
                        outlineColor,
                        style: StrokeStyle(
                            lineWidth: ReticleStyle.default.primary,
                            dash: result.maskImage == nil ? [6, 4] : []
                        )
                    )
                    .shadow(color: .black.opacity(0.6), radius: 1)
                    .frame(width: rect.width, height: rect.height)
                    .position(x: rect.midX, y: rect.midY)
                Text(
                    result.identityIsFixture
                        ? "BOT TEST TARGET"
                        : String(format: "IDENTITY %.0f%%", result.targetScore * 100)
                )
                    .font(.caption2.bold().monospaced())
                    .foregroundStyle(.white)
                    .padding(4)
                    .background(.black.opacity(0.7))
                    .position(x: rect.midX, y: max(18, rect.minY - 14))
            }
        }
        .allowsHitTesting(false)
    }
}

struct GameplayReticleOverlay: View {
    let state: GameplayTargetingState?
    let imageSize: CGSize
    var style: ReticleStyle = .default

    var body: some View {
        GeometryReader { proxy in
            if let state {
                let geometry = PreviewGeometry(viewSize: proxy.size, imageSize: imageSize)
                let point = geometry.point(fromVisionNormalized: state.gameplayPoint)
                let scale = max(
                    proxy.size.width / max(imageSize.width, 1),
                    proxy.size.height / max(imageSize.height, 1)
                )
                let sourceRadius = min(imageSize.width, imageSize.height)
                    * GameplayTargetingTuning.default.reticleRadiusFraction
                let radius = max(sourceRadius * scale, 14)
                // The bars stop short of the ring so nothing crosses the middle
                // of the target the player is trying to read.
                let barInner = radius + 3
                let barOuter = max(radius * 1.9, radius + 16)
                let canvasSide = (barOuter + 8) * 2
                ZStack {
                    Canvas { context, size in
                        let centre = CGPoint(x: size.width / 2, y: size.height / 2)
                        let tint = color(for: state.status)

                        context.strokeTactical(
                            Path(ellipseIn: CGRect(
                                x: centre.x - radius,
                                y: centre.y - radius,
                                width: radius * 2,
                                height: radius * 2
                            )),
                            color: tint,
                            width: style.primary,
                            style: style
                        )

                        var bars = Path()
                        bars.move(to: CGPoint(x: centre.x, y: centre.y - barOuter))
                        bars.addLine(to: CGPoint(x: centre.x, y: centre.y - barInner))
                        bars.move(to: CGPoint(x: centre.x, y: centre.y + barInner))
                        bars.addLine(to: CGPoint(x: centre.x, y: centre.y + barOuter))
                        bars.move(to: CGPoint(x: centre.x - barOuter, y: centre.y))
                        bars.addLine(to: CGPoint(x: centre.x - barInner, y: centre.y))
                        bars.move(to: CGPoint(x: centre.x + barInner, y: centre.y))
                        bars.addLine(to: CGPoint(x: centre.x + barOuter, y: centre.y))
                        context.strokeTactical(
                            bars,
                            color: tint,
                            width: style.hairline,
                            style: style
                        )

                        context.fillTactical(dotAt: centre, radius: 1.5, color: tint, style: style)
                    }
                    .frame(width: canvasSide, height: canvasSide)
                    if let label = label(for: state.status) {
                        Text(label)
                            .font(.caption2.bold().monospaced())
                            .foregroundStyle(.white)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 3)
                            .background(.black.opacity(0.76), in: Capsule())
                            .offset(y: radius + 22)
                    }
                }
                .position(point)
            }
        }
        .allowsHitTesting(false)
    }

    private func color(for status: GameplayTargetingStatus) -> Color {
        switch status {
        case .ready: return .green
        case .unavailable, .stale, .identityWeak: return .orange
        case .outsideMask: return .white
        }
    }

    private func label(for status: GameplayTargetingStatus) -> String? {
        switch status {
        case .ready: return "ON TARGET"
        case .unavailable: return "ACQUIRING TARGET"
        case .stale: return "REFRESHING TARGET"
        case .identityWeak: return "IDENTITY LOW"
        case .outsideMask: return nil
        }
    }
}

struct MultiplayerHUD: View {
    @ObservedObject var game: GameplayCoordinator

    var body: some View {
        VStack {
            HStack(alignment: .top, spacing: 10) {
                radar
                Spacer()
                VStack(alignment: .trailing, spacing: 6) {
                    health(game.myState?.health ?? 3, color: .green)
                    if let opponent = game.opponentState {
                        health(opponent.health, color: opponent.eliminated ? .gray : .red)
                    }
                    if let result = game.lastShotResult {
                        Text(result).font(.headline.bold().monospaced()).foregroundStyle(result == "HIT" ? .green : .orange)
                    }
                }
                opponentBriefing
            }
            Spacer()
            if game.match?.status == .lobby {
                VStack(spacing: 5) {
                    Text(game.match?.players.count == 1 ? "WAITING FOR FPS-BOT" : "READY UP IN LOBBY")
                        .font(.headline.bold().monospaced())
                    Text(game.match?.players.count == 1 ? "Open Play for the launch command" : "Both players must be ready")
                        .font(.caption.monospaced())
                }
                .foregroundStyle(.white)
                .padding()
                .background(.black.opacity(0.82), in: RoundedRectangle(cornerRadius: 12))
            } else if game.match?.status == .completed {
                Text(game.match?.winner == game.session?.playerId ? "YOU WIN" : "ELIMINATED")
                    .font(.system(size: 36, weight: .black, design: .rounded))
                    .foregroundStyle(.white)
                    .padding()
                    .background(.black.opacity(0.8), in: RoundedRectangle(cornerRadius: 14))
            }
        }
        .padding(14)
        .allowsHitTesting(false)
    }

    private var radar: some View {
        VStack(alignment: .leading, spacing: 4) {
            ZStack {
                Circle().fill(.black.opacity(0.72)).frame(width: 92, height: 92)
                Circle().stroke(.green.opacity(0.8), lineWidth: 2).frame(width: 92, height: 92)
                Circle().stroke(.green.opacity(0.35), lineWidth: 1).frame(width: 48, height: 48)
                if let direction = game.nearby.reading?.direction, direction.count == 3 {
                    Image(systemName: "location.north.fill")
                        .foregroundStyle(.red)
                        .offset(y: -24)
                        .rotationEffect(.radians(atan2(Double(direction[0]), Double(-direction[2]))))
                } else if game.nearby.reading != nil {
                    Circle().fill(.yellow).frame(width: 8, height: 8)
                } else {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.caption)
                        .foregroundStyle(.orange)
                        .offset(y: -20)
                }
                Text(
                    game.nearby.reading?.distanceMeters.map {
                        String(format: "%.1fm", $0)
                    } ?? "NO RANGE"
                )
                    .font(.caption2.bold().monospaced())
                    .foregroundStyle(.white)
                    .offset(y: 28)
            }
            Text(game.nearby.status)
                .font(.caption2.bold().monospaced())
                .foregroundStyle(game.nearby.reading == nil ? .orange : .white)
                .lineLimit(2)
                .frame(width: 132, alignment: .leading)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Nearby ranging: \(game.nearby.status)")
    }

    @ViewBuilder private var opponentBriefing: some View {
        if let base64 = game.opponentProfile?.briefingThumbnail,
           let data = Data(base64Encoded: base64),
           let image = UIImage(data: data) {
            Image(uiImage: image)
                .resizable()
                .scaledToFill()
                .grayscale(1)
                .contrast(1.5)
                .frame(width: 64, height: 82)
                .clipShape(RoundedRectangle(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).stroke(.white.opacity(0.7)))
        }
    }

    private func health(_ value: Int, color: Color) -> some View {
        HStack(spacing: 4) {
            ForEach(0..<3, id: \.self) { index in
                Image(systemName: index < value ? "shield.fill" : "shield")
                    .foregroundStyle(index < value ? color : .gray)
            }
        }
        .padding(6)
        .background(.black.opacity(0.65), in: Capsule())
    }
}
