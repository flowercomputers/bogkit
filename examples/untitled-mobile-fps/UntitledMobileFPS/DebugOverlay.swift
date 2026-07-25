import SwiftUI

struct DebugOverlay: View {
    let hand: TrackedHand?
    let mediaPipeHand: TrackedHand?
    let analysis: VisionFingerGunAnalysis?
    let mediaPipeAnalysis: FingerGunAnalysis?
    let observation: VisionFingerGunObservation?
    let aim: AimSolution?
    let aimRejectionReason: String?
    let aimingMode: AimingMode
    let scopeProximity: ScopeProximityDiagnostic?
    let state: GestureState
    let calibrationState: CalibrationState
    let metrics: AnalysisMetrics
    let flash: FlashEvent?
    let imageSize: CGSize
    let trackerName: String

    var body: some View {
        GeometryReader { proxy in
            let mapping = PreviewGeometry(viewSize: proxy.size, imageSize: imageSize)
            ZStack(alignment: .topLeading) {
                Canvas { context, _ in
                    drawSkeleton(hand, source: "VN", boneColor: .cyan, jointColor: .pink, in: &context, mapping: mapping)
                    drawAim(in: &context, mapping: mapping)
                    drawFlash(in: &context, mapping: mapping)
                }
                hud.padding(.top, 12).padding(.leading, 12)
            }
        }
        .allowsHitTesting(false)
    }

    private var hud: some View {
        let mediaPipeBarrel = mediaPipeAnalysis?.indexDirection ?? .zero
        return VStack(alignment: .leading, spacing: 3) {
            Text("FINGER-GUN VISION").fontWeight(.bold)
            Text("TRACK  \(trackerName) PRIMARY")
            Text("MODEL  \(VisionAimCalibration.modelVersion)")
            Text("MODE   \(aimingMode.rawValue)")
                .foregroundStyle(aimingMode == .sights ? .red : .white)
            Text(String(
                format: "PROX   %.2fx  BASE %.3f  %@ %.0f%%",
                scopeProximity?.ratio ?? 0,
                scopeProximity?.baseline ?? 0,
                (scopeProximity?.warm ?? false) ? "RDY" : "WARM",
                (scopeProximity?.progress ?? 0) * 100
            ))
            .foregroundStyle((scopeProximity?.warm ?? false) ? .white : .yellow)
            Text("STATE  \(state.rawValue)").foregroundStyle(state == .armed ? .green : state == .fired ? .orange : .white)
            Text("CAL    \(calibrationState.label)")
            Text("POSE   \(observation?.variation.rawValue ?? "—")  TH \(analysis?.thumbState.rawValue ?? "—")")
            Text("FING   I\(short(analysis?.indexState)) M\(short(analysis?.middleState)) R\(short(analysis?.ringState)) L\(short(analysis?.littleState))")
            Text("REJECT \(analysis?.rejectionReason ?? "—")")
            Text(String(format: "CONF   %.2f  M %.3f", observation?.confidence ?? 0, observation?.poseMargin ?? 0))
            Text(String(format: "MP Z   %+.2f  2D Δ %.3f", mediaPipeBarrel.z, jointDelta))
                .foregroundStyle(mediaPipeBarrel.z > 0 ? .green : .red)
            if aimingMode == .sights {
                Text("RAW XY — —")
                Text("AIM XY 0.50 0.50  SIGHTS")
            } else {
                Text(String(format: "RAW XY %.2f %.2f", aim?.rawScreenPoint.x ?? 0, aim?.rawScreenPoint.y ?? 0))
                Text(String(format: "AIM XY %.2f %.2f  %@", aim?.screenPoint.x ?? 0, aim?.screenPoint.y ?? 0, aimZone?.rawValue ?? "—"))
            }
            // Sights have no solver reticle by design, so a missing aim is only a
            // fault while unscoped.
            Text("AIMREJ \(aimRejectionReason ?? "—")")
                .foregroundStyle(
                    aimingMode == .unscoped && aim == nil && aimRejectionReason != nil ? .red : .white
                )
            Text(String(format: "VISION %.1f FPS  %.0f ms", metrics.framesPerSecond, metrics.latencyMilliseconds))
            Text("VN DROP \(metrics.visionDroppedFrames)")
                .foregroundStyle(metrics.visionDroppedFrames == 0 ? .white : .yellow)
            Text(String(format: "MP DIAG %.1f FPS  %.0f ms", metrics.mediaPipeFramesPerSecond, metrics.mediaPipeLatencyMilliseconds))
        }
        .font(.system(size: 10, design: .monospaced))
        .foregroundStyle(.white)
        .padding(9)
        .background(.black.opacity(0.62), in: RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(.white.opacity(0.35)))
    }

    private func drawSkeleton(
        _ hand: TrackedHand?,
        source: String,
        boneColor: Color,
        jointColor: Color,
        in context: inout GraphicsContext,
        mapping: PreviewGeometry
    ) {
        guard let hand else { return }
        if let bounds = hand.paddedBounds() {
            context.stroke(
                Path(mapping.rect(fromVisionNormalized: bounds)),
                with: .color(boneColor.opacity(0.75)),
                style: StrokeStyle(lineWidth: 1.3, dash: source == "MP" ? [5, 3] : [])
            )
        }
        for chain in Self.boneChains {
            var path = Path()
            var started = false
            for joint in chain {
                guard let landmark = hand[image: joint], landmark.confidence >= 0.3 else { started = false; continue }
                let point = mapping.point(fromVisionNormalized: landmark.location)
                if started { path.addLine(to: point) } else { path.move(to: point); started = true }
            }
            context.stroke(
                path,
                with: .color(boneColor.opacity(0.9)),
                style: StrokeStyle(lineWidth: source == "MP" ? 1.5 : 2, dash: source == "MP" ? [4, 3] : [])
            )
        }
        for (joint, landmark) in hand.imagePoints where landmark.confidence >= 0.3 {
            let point = mapping.point(fromVisionNormalized: landmark.location)
            context.fill(Path(ellipseIn: CGRect(x: point.x - 2.5, y: point.y - 2.5, width: 5, height: 5)), with: .color(jointColor))
            if let label = Self.jointLabels[joint] {
                context.draw(
                    Text("\(source):\(label)").font(.system(size: 7, design: .monospaced)).foregroundStyle(jointColor),
                    at: CGPoint(x: point.x + 10, y: point.y - 7),
                    anchor: .center
                )
            }
        }
    }

    private func drawAim(in context: inout GraphicsContext, mapping: PreviewGeometry) {
        guard aimingMode == .unscoped, let aim, aim.valid else { return }
        let center = mapping.point(fromVisionNormalized: CGPoint(x: 0.5, y: 0.5))
        let raw = mapping.point(fromVisionNormalized: aim.rawScreenPoint)
        let target = mapping.point(fromVisionNormalized: aim.screenPoint)
        var axis = Path(); axis.move(to: center); axis.addLine(to: target)
        context.stroke(axis, with: .color(.red.opacity(0.55)), style: StrokeStyle(lineWidth: 1.5, dash: [5, 4]))
        context.stroke(Path(ellipseIn: CGRect(x: raw.x - 5, y: raw.y - 5, width: 10, height: 10)), with: .color(.orange.opacity(0.9)), lineWidth: 1.5)
        context.fill(Path(ellipseIn: CGRect(x: target.x - 11, y: target.y - 11, width: 22, height: 22)), with: .color(.red.opacity(0.22)))
        let dot = CGRect(x: target.x - 4, y: target.y - 4, width: 8, height: 8)
        context.fill(Path(ellipseIn: dot), with: .color(.red))
        context.stroke(Path(ellipseIn: dot), with: .color(.white.opacity(0.8)), lineWidth: 1)
    }

    private func drawFlash(in context: inout GraphicsContext, mapping: PreviewGeometry) {
        guard let flash else { return }
        let center = mapping.point(fromVisionNormalized: flash.point)
        for index in 0..<10 {
            let angle = CGFloat(index) * (.pi * 2 / 10)
            let radius: CGFloat = index.isMultiple(of: 2) ? 52 : 36
            var ray = Path(); ray.move(to: center)
            ray.addLine(to: CGPoint(x: center.x + cos(angle) * radius, y: center.y + sin(angle) * radius))
            context.stroke(ray, with: .color(.orange), lineWidth: index.isMultiple(of: 2) ? 5 : 3)
        }
        context.fill(Path(ellipseIn: CGRect(x: center.x - 16, y: center.y - 16, width: 32, height: 32)), with: .color(.white))
    }

    private func short(_ state: FingerExtensionState?) -> String {
        switch state {
        case .straight: return "S"
        case .curled: return "C"
        case .ambiguous: return "?"
        case nil: return "—"
        }
    }

    private var aimZone: AimDirectionZone? {
        guard let aim, aim.valid else { return nil }
        return AimDirectionZone.allCases.min {
            hypot($0.point.x - aim.screenPoint.x, $0.point.y - aim.screenPoint.y) <
                hypot($1.point.x - aim.screenPoint.x, $1.point.y - aim.screenPoint.y)
        }
    }

    private var jointDelta: Double {
        guard let hand, let mediaPipeHand = freshMediaPipeHand else { return 0 }
        let distances = LandmarkJoint.allCases.compactMap { joint -> Double? in
            guard let mp = hand[image: joint], mp.confidence >= 0.3,
                  let vn = mediaPipeHand[image: joint], vn.confidence >= 0.3 else { return nil }
            return Double(hypot(mp.location.x - vn.location.x, mp.location.y - vn.location.y))
        }
        guard !distances.isEmpty else { return 0 }
        return distances.reduce(0, +) / Double(distances.count)
    }

    private var freshMediaPipeHand: TrackedHand? {
        guard let mediaPipeHand else { return nil }
        guard let hand else { return mediaPipeHand }
        return abs(mediaPipeHand.timestamp - hand.timestamp) <= 0.15 ? mediaPipeHand : nil
    }

    private static let jointLabels: [LandmarkJoint: String] = [
        .wrist: "W", .thumbTip: "T", .indexTip: "I",
        .middleTip: "M", .ringTip: "R", .littleTip: "L"
    ]

    private static let boneChains: [[LandmarkJoint]] = [
        [.wrist, .thumbCMC, .thumbMP, .thumbIP, .thumbTip],
        [.wrist, .indexMCP, .indexPIP, .indexDIP, .indexTip],
        [.wrist, .middleMCP, .middlePIP, .middleDIP, .middleTip],
        [.wrist, .ringMCP, .ringPIP, .ringDIP, .ringTip],
        [.wrist, .littleMCP, .littlePIP, .littleDIP, .littleTip],
        [.indexMCP, .middleMCP, .ringMCP, .littleMCP]
    ]
}
