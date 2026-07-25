import SwiftUI
import UIKit
import Combine

struct GameplayCameraView: View {
    @ObservedObject var camera: CameraService
    @ObservedObject var game: GameplayCoordinator
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var showMatchDiagnostics = false

    var body: some View {
        ZStack {
            let matchIsActive = game.match?.status == .active
            Color.black.ignoresSafeArea()
            CameraPreview(session: camera.session).ignoresSafeArea()

            if let target = camera.personTarget, matchIsActive {
                TargetSilhouetteOverlay(
                    result: target,
                    imageSize: camera.orientedImageSize,
                    eliminated: game.opponentState?.eliminated == true,
                    skin: game.opponentProfile?.silhouetteSkin ?? .fallback
                )
                .ignoresSafeArea()
            }

            if !matchIsActive || showMatchDiagnostics {
                DebugOverlay(
                    hand: camera.trackedHand,
                    mediaPipeHand: camera.mediaPipeHand,
                    analysis: camera.visionAnalysis,
                    mediaPipeAnalysis: camera.mediaPipeAnalysis,
                    observation: camera.observation,
                    aim: camera.aim,
                    aimRejectionReason: camera.aimRejectionReason,
                    aimingMode: camera.aimingMode,
                    scopeProximity: camera.scopeProximity,
                    state: camera.gestureState,
                    calibrationState: camera.calibrationState,
                    metrics: camera.metrics,
                    flash: camera.flash,
                    imageSize: camera.orientedImageSize,
                    trackerName: camera.trackerName
                )
                .ignoresSafeArea()
            }

            if matchIsActive {
                GameplayReticleOverlay(
                    state: camera.gameplayTargetingState,
                    imageSize: camera.orientedImageSize
                )
                .ignoresSafeArea()
            }

            if game.match != nil { MultiplayerHUD(game: game).ignoresSafeArea() }

            // The scoped frame carries no crosshair of its own, so it layers
            // over the gameplay reticle during a match without stacking two.
            // It must show in a match: that is where telling the modes apart
            // actually matters, and the debug HUD is hidden there.
            if camera.aimingMode == .sights {
                SightsFrameOverlay()
                    .ignoresSafeArea()
                    .allowsHitTesting(false)
                    .transition(.opacity.combined(with: .scale(scale: 1.04)))

                // Outside a match nothing else draws a reticle, so the plain
                // crosshair still supplies the aim point.
                if !matchIsActive {
                    SightsReticle()
                        .ignoresSafeArea()
                        .allowsHitTesting(false)
                }
            }

            if camera.aimingMode == .unscoped, camera.scopeEntryProgress > 0.02 {
                ScopeEntryIndicator(progress: camera.scopeEntryProgress)
                    .ignoresSafeArea()
                    .allowsHitTesting(false)
            }

            VStack {
                AimingModeBadge(mode: camera.aimingMode)
                    .padding(.top, 8)
                Spacer()
            }
            .frame(maxWidth: .infinity)
            .allowsHitTesting(false)

            if let target = camera.currentCalibrationTarget {
                calibrationTargetOverlay(target)
                    .ignoresSafeArea()
                    .allowsHitTesting(false)
            }

            VStack {
                HStack {
                    Spacer()
                    controls
                }
                Spacer()
                calibrationPrompt
            }
            .padding(12)

            if let message = camera.status.message, camera.status != .idle { statusCard(message) }
        }
        .onAppear { camera.start() }
        .onDisappear {
            camera.finalizeRecording()
            camera.stop()
        }
        .onReceive(camera.$shotEvent.compactMap { $0 }) { game.handleShot($0) }
        .onReceive(game.$opponentProfile) { profile in
            camera.setTargetAppearance(game.match?.status == .active ? profile : nil)
        }
        .onReceive(
            game.nearby.$status.combineLatest(game.nearby.$reading)
        ) { status, reading in
            camera.setNearbyInteractionDiagnostic(
                status: status,
                distanceMeters: reading?.distanceMeters,
                direction: reading?.direction,
                sampledAtMs: reading?.sampledAtMs
            )
        }
        .onReceive(game.$match) { match in
            camera.setTargetAppearance(match?.status == .active ? game.opponentProfile : nil)
            if match?.status != .active {
                showMatchDiagnostics = false
            }
        }
        .animation(.easeOut(duration: 0.16), value: camera.aimingMode)
        .onChange(of: camera.aimingMode) { _, mode in
            // A mode change you can feel. Visual chrome alone still has to be
            // noticed; the tap confirms the transition even while the player is
            // watching the target rather than the edges of the screen.
            let generator = UIImpactFeedbackGenerator(style: mode == .sights ? .rigid : .soft)
            generator.impactOccurred(intensity: mode == .sights ? 1.0 : 0.6)
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active:
                camera.start()
            case .background: camera.stop()
            default: break
            }
        }
        .statusBarHidden()
    }

    private var controls: some View {
        VStack(spacing: 8) {
            Button {
                camera.finalizeRecording()
                camera.stop()
                game.leaveMatch()
                dismiss()
            } label: {
                Label("Leave", systemImage: "xmark")
            }
            .tint(.red.opacity(0.82))
            if game.match?.status == .active {
                Button {
                    showMatchDiagnostics.toggle()
                } label: {
                    Label(
                        showMatchDiagnostics ? "Hide debug" : "Debug",
                        systemImage: showMatchDiagnostics ? "ladybug.fill" : "ladybug"
                    )
                }
            }
#if DEBUG
            Button { camera.toggleRecording() } label: {
                Label(camera.isRecording ? "Stop data" : "Record data", systemImage: camera.isRecording ? "stop.circle.fill" : "record.circle")
            }
            if let url = camera.lastRecordingURL {
                ShareLink(item: url) { Label("Export data", systemImage: "square.and.arrow.up") }
            }
#endif
        }
        .font(.caption.bold())
        .buttonStyle(.borderedProminent)
        .tint(.black.opacity(0.72))
    }

    @ViewBuilder private var calibrationPrompt: some View {
        if camera.aimingMode != .sights {
            switch camera.calibrationState {
            case .required:
                Text("Five-point calibration required\nUse a natural thumb-up finger gun for center, left, right, up, and down, then tap Calibrate.")
                    .foregroundStyle(.white)
                    .multilineTextAlignment(.center)
                    .padding(12)
                    .background(.black.opacity(0.72), in: RoundedRectangle(cornerRadius: 10))
            case .collecting(let progress, _):
                VStack(spacing: 8) {
                    Image(systemName: "scope").font(.system(size: 34))
                    Text(camera.calibrationInstruction ?? "Hold the finger gun steady")
                    ProgressView(value: camera.calibrationTargetProgress).tint(.green)
                    Text("Overall \(Int(progress * 100))%").font(.caption.monospacedDigit())
                }
                .foregroundStyle(.white)
                .padding(14)
                .background(.black.opacity(0.72), in: RoundedRectangle(cornerRadius: 10))
            case .failed(let message):
                Text(message).foregroundStyle(.white).padding(12).background(.red.opacity(0.75), in: RoundedRectangle(cornerRadius: 10))
            case .calibrated:
                EmptyView()
            }
        }
    }

    private func calibrationTargetOverlay(_ target: VisionCalibrationTarget) -> some View {
        GeometryReader { proxy in
            let point = CGPoint(x: target.point.x * proxy.size.width, y: (1 - target.point.y) * proxy.size.height)
            ZStack {
                // Stays white rather than laser red: this is an instruction
                // marker, not a sight. The tactical stroke keeps a 1.5pt white
                // ring readable over a bright frame without thickening it.
                Canvas { context, size in
                    let style = ReticleStyle.default
                    let centre = CGPoint(x: size.width / 2, y: size.height / 2)
                    context.strokeTactical(
                        Path(ellipseIn: CGRect(x: centre.x - 23, y: centre.y - 23, width: 46, height: 46)),
                        color: .white,
                        width: 1.5,
                        style: style
                    )
                    var arms = Path()
                    arms.move(to: CGPoint(x: centre.x - 32, y: centre.y))
                    arms.addLine(to: CGPoint(x: centre.x + 32, y: centre.y))
                    arms.move(to: CGPoint(x: centre.x, y: centre.y - 32))
                    arms.addLine(to: CGPoint(x: centre.x, y: centre.y + 32))
                    context.strokeTactical(arms, color: .white, width: style.hairline, style: style)
                    context.fillTactical(dotAt: centre, radius: 2.5, color: style.laser, style: style)
                }
                .frame(width: 88, height: 88)
                Text(target.rawValue).font(.caption2.bold().monospaced()).foregroundStyle(.white).offset(y: 42)
            }
            .position(point)
        }
    }

    private func statusCard(_ message: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: camera.status == .denied ? "camera.fill.badge.xmark" : "camera.fill").font(.system(size: 34))
            Text(message).multilineTextAlignment(.center)
            if camera.status == .denied {
                Button("Open Settings") {
                    guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
                    UIApplication.shared.open(url)
                }
                .buttonStyle(.borderedProminent)
            }
        }
        .font(.callout)
        .foregroundStyle(.white)
        .padding(22)
        .frame(maxWidth: 310)
        .background(.black.opacity(0.82), in: RoundedRectangle(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).stroke(.white.opacity(0.3)))
    }
}
