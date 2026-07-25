import AVFoundation
import Combine
import UIKit

enum CameraStatus: Equatable {
    case idle
    case requestingPermission
    case running
    case denied
    case unavailable(String)
    case interrupted(String)

    var message: String? {
        switch self {
        case .idle: return "Camera idle"
        case .requestingPermission: return "Requesting camera access…"
        case .running: return nil
        case .denied: return "Camera access is required. Enable it in Settings."
        case .unavailable(let reason), .interrupted(let reason): return reason
        }
    }
}

struct AnalysisMetrics: Equatable {
    var framesPerSecond: Double = 0
    var latencyMilliseconds: Double = 0
    var mediaPipeFramesPerSecond: Double = 0
    var mediaPipeLatencyMilliseconds: Double = 0
    var visionDroppedFrames: Int = 0
}

struct FlashEvent: Equatable {
    let id: Int
    let point: CGPoint
}

final class CameraService: NSObject, ObservableObject, @unchecked Sendable {
    let session = AVCaptureSession()

    @Published private(set) var status: CameraStatus = .idle
    @Published private(set) var trackedHand: TrackedHand?
    @Published private(set) var mediaPipeHand: TrackedHand?
    @Published private(set) var mediaPipeAnalysis: FingerGunAnalysis?
    @Published private(set) var visionAnalysis: VisionFingerGunAnalysis?
    @Published private(set) var observation: VisionFingerGunObservation?
    @Published private(set) var aim: AimSolution?
    @Published private(set) var aimRejectionReason: String?
    @Published private(set) var aimingMode: AimingMode = .unscoped
    @Published private(set) var scopeProximity: ScopeProximityDiagnostic?
    /// How close the current hold is to engaging sights, 0...1. Drives the
    /// on-screen ring so the gesture is discoverable instead of guessed at.
    var scopeEntryProgress: Double { scopeProximity?.progress ?? 0 }
    @Published private(set) var gestureState: GestureState = .notDetected
    @Published private(set) var calibrationState: CalibrationState = .required(nil)
    @Published private(set) var metrics = AnalysisMetrics()
    @Published private(set) var flash: FlashEvent?
    @Published private(set) var orientedImageSize = CGSize(width: 1080, height: 1920)
    @Published private(set) var isRecording = false
    @Published private(set) var lastRecordingURL: URL?
    @Published private(set) var currentCalibrationTarget: VisionCalibrationTarget?
    @Published private(set) var calibrationTargetProgress: Double = 0
    @Published private(set) var calibrationInstruction: String?
    @Published private(set) var personTarget: PersonTargetingResult?
    @Published private(set) var gameplayTargetingState: GameplayTargetingState?
    @Published private(set) var shotEvent: GameplayShotEvent?
    let trackerName = "VISION 2D"

    private let tuning: GestureTuning
    private let tracker: (any HandTracking)?
    private let mediaPipeDiagnosticsEnabled: Bool
    private let diagnosticVision = VisionDiagnosticRunner()
    private let classifier: any FingerGunClassifying
    private let visionClassifier: VisionFingerGunClassifier
    private let sessionQueue = DispatchQueue(label: "camera.session.queue")
    private let videoQueue = DispatchQueue(label: "camera.capture.queue", qos: .userInitiated)
    private let analysisQueue = DispatchQueue(label: "camera.analysis.queue", qos: .userInitiated)
    private let videoOutput = AVCaptureVideoDataOutput()
    private let calibrationStore: VisionAimCalibrationStore
    private let recorder = DiagnosticRecorder()
    private let personTargeting = PersonTargetingRunner()
    private let targetLock = NSLock()
    private var configured = false
    private var stateMachine: GestureStateMachine
    private var aimSolver: VisionAimSolver
    private var scopeModeDetector: ScopeModeDetector
    private var calibrationCollector: VisionAimCalibrationCollector?
    private var calibrationRequested = false
    private var calibrationFailureMessage: String?
    private var selector = HandSelector()
    private var mediaPipeSelector = HandSelector()
    private var visionSmoother = VisionLandmarkSmoother()
    private var frameCounter = 0
    private var metricWindowStart = CACurrentMediaTime()
    private var lastFramesPerSecond: Double = 0
    private var mediaPipeFrameCounter = 0
    private var mediaPipeMetricWindowStart = CACurrentMediaTime()
    private var lastMediaPipeFramesPerSecond: Double = 0
    private var lastMediaPipeLatencyMilliseconds: Double = 0
    private var visionDroppedFrames = 0
    private var flashID = 0
    private var lastValidTimestamp: TimeInterval?
    private var lastAimTimestamp: TimeInterval?
    private var lastAimSolution: AimSolution?
    private var cameraIdentifier = "rear-wide"
    private var targetProfile: AppearanceProfile?
    private var personTargetingEnabled = false
    private var lastPersonTarget: PersonTargetingResult?
    private var shotEventID = 0
    private var captureDevice: AVCaptureDevice?
    private var activeAimingMode: AimingMode = .unscoped
    private var nearbyInteractionDiagnostic: NearbyInteractionRecordingDiagnostic?

    init(
        tuning: GestureTuning = .default,
        tracker suppliedTracker: (any HandTracking)? = nil,
        classifier suppliedClassifier: (any FingerGunClassifying)? = nil,
        calibrationStore: VisionAimCalibrationStore = VisionAimCalibrationStore(),
        mediaPipeDiagnosticsEnabled: Bool = false
    ) {
        self.tuning = tuning
        let shouldEnableMediaPipe = mediaPipeDiagnosticsEnabled || suppliedTracker != nil
        self.mediaPipeDiagnosticsEnabled = shouldEnableMediaPipe
        if !shouldEnableMediaPipe {
            tracker = nil
        } else if let suppliedTracker {
            tracker = suppliedTracker
        } else {
            do {
                tracker = try MediaPipeHandTracker.bundled(tuning: tuning)
            } catch {
                tracker = UnavailableHandTracker(error: error)
            }
        }
        classifier = suppliedClassifier ?? FingerGunClassifier(tuning: tuning)
        visionClassifier = VisionFingerGunClassifier(tuning: tuning)
        self.calibrationStore = calibrationStore
        stateMachine = GestureStateMachine(tuning: tuning)
        aimSolver = VisionAimSolver(tuning: tuning)
        scopeModeDetector = ScopeModeDetector(tuning: tuning)
        super.init()
        lastRecordingURL = DiagnosticRecorder.latestRecordingURL()
        if let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) {
            cameraIdentifier = camera.uniqueID
            if calibrationStore.calibration(cameraIdentifier: cameraIdentifier) != nil {
                calibrationState = .calibrated(.unknown)
            }
        }
        observeSessionNotifications()
    }

    deinit { NotificationCenter.default.removeObserver(self) }

    func start() {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: configureAndStart()
        case .notDetermined:
            publish { self.status = .requestingPermission }
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                granted ? self?.configureAndStart() : self?.publish { self?.status = .denied }
            }
        case .denied, .restricted: publish { self.status = .denied }
        @unknown default: publish { self.status = .unavailable("Unknown camera authorization state.") }
        }
    }

    func stop() {
        analysisQueue.async { [weak self] in
            self?.resetGameplayForUnscopedMode()
        }
        sessionQueue.async { [weak self] in
            guard let self, session.isRunning else { return }
            session.stopRunning()
            publish { self.status = .idle }
        }
    }

    func beginCalibration() {
        analysisQueue.async { [weak self] in
            guard let self else { return }
            calibrationRequested = true
            calibrationFailureMessage = nil
            var collector = VisionAimCalibrationCollector(tuning: tuning, cameraIdentifier: cameraIdentifier)
            collector.begin()
            calibrationCollector = collector
            scopeModeDetector.reset()
            activeAimingMode = .unscoped
            setCameraZoom(for: .unscoped)
            aimSolver.reset()
            lastAimTimestamp = nil
            lastAimSolution = nil
            lastValidTimestamp = nil
            stateMachine.reset()
            publish {
                self.calibrationState = .collecting(progress: 0, handedness: nil)
                self.currentCalibrationTarget = .center
                self.calibrationTargetProgress = 0
                self.calibrationInstruction = "Hold steady on CENTER"
                self.aim = nil
                self.aimingMode = .unscoped
                self.gestureState = .notDetected
            }
        }
    }

    func resetCalibration() {
        analysisQueue.async { [weak self] in
            guard let self else { return }
            calibrationStore.reset(cameraIdentifier: cameraIdentifier)
            calibrationRequested = false
            calibrationFailureMessage = nil
            calibrationCollector?.cancel()
            calibrationCollector = nil
            scopeModeDetector.reset()
            activeAimingMode = .unscoped
            setCameraZoom(for: .unscoped)
            aimSolver.reset()
            lastAimTimestamp = nil
            lastAimSolution = nil
            lastValidTimestamp = nil
            stateMachine.reset()
            publish {
                self.calibrationState = .required(nil)
                self.currentCalibrationTarget = nil
                self.calibrationTargetProgress = 0
                self.calibrationInstruction = nil
                self.aim = nil
                self.aimingMode = .unscoped
                self.gestureState = .notDetected
            }
        }
    }

    func toggleRecording() {
        analysisQueue.async { [weak self] in
            guard let self else { return }
            if recorder.isRecording {
                finalizeRecordingOnAnalysisQueue()
            } else {
                recorder.start()
                publish { self.isRecording = true }
            }
        }
    }

    /// Completes an in-progress diagnostic recording without requiring the
    /// camera view to stay on screen. Calls are serialized with frame appends.
    func finalizeRecording() {
        analysisQueue.async { [weak self] in
            guard let self, recorder.isRecording else { return }
            finalizeRecordingOnAnalysisQueue()
        }
    }

    func setNearbyInteractionDiagnostic(
        status: String,
        distanceMeters: Float?,
        direction: [Float]?,
        sampledAtMs: UInt64?
    ) {
        analysisQueue.async { [weak self] in
            self?.nearbyInteractionDiagnostic = NearbyInteractionRecordingDiagnostic(
                status: status,
                distanceMeters: distanceMeters,
                direction: direction,
                sampledAtMs: sampledAtMs
            )
        }
    }

    private func finalizeRecordingOnAnalysisQueue() {
        let savedURL: URL?
        do {
            savedURL = try recorder.stop()
        } catch {
            savedURL = nil
        }
        publish {
            self.isRecording = false
            if let savedURL {
                self.lastRecordingURL = savedURL
            }
        }
    }

    func setTargetAppearance(_ profile: AppearanceProfile?) {
        targetLock.withLock {
            personTargetingEnabled = profile != nil
            targetProfile = profile
        }
        if profile == nil {
            personTargeting.reset()
            analysisQueue.async { [weak self] in
                self?.lastPersonTarget = nil
                self?.publish {
                    self?.personTarget = nil
                    self?.gameplayTargetingState = nil
                }
            }
        }
    }

    private func configureAndStart() {
        sessionQueue.async { [weak self] in
            guard let self else { return }
            do {
                if !configured { try configureSession() }
                guard !session.isRunning else { return }
                session.startRunning()
                publish { self.status = .running }
            } catch {
                publish { self.status = .unavailable("Camera unavailable: \(error.localizedDescription)") }
            }
        }
    }

    private func configureSession() throws {
        session.beginConfiguration()
        defer { session.commitConfiguration() }
        session.sessionPreset = .high
        guard let camera = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back) else {
            throw CameraSetupError.noRearCamera
        }
        let input = try AVCaptureDeviceInput(device: camera)
        guard session.canAddInput(input) else { throw CameraSetupError.cannotAddInput }
        session.addInput(input)
        captureDevice = camera
        cameraIdentifier = camera.uniqueID
        calibrationCollector = VisionAimCalibrationCollector(tuning: tuning, cameraIdentifier: cameraIdentifier)
        let hasStoredCalibration = calibrationStore.calibration(cameraIdentifier: cameraIdentifier) != nil
        publish {
            self.calibrationState = hasStoredCalibration ? .calibrated(.unknown) : .required(nil)
        }

        videoOutput.alwaysDiscardsLateVideoFrames = true
        videoOutput.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA]
        videoOutput.setSampleBufferDelegate(self, queue: videoQueue)
        guard session.canAddOutput(videoOutput) else { throw CameraSetupError.cannotAddOutput }
        session.addOutput(videoOutput)
        configured = true
    }

    private func processVisionPrimary(_ result: HandTrackingResult, latency: Double) {
        let selectedHand = selector.select(result.hands, timestamp: result.timestamp)
        let hand = visionSmoother.smooth(selectedHand, timestamp: result.timestamp)
        let freshAnalysis = hand.map { visionClassifier.analyze($0) }
        let freshObservation = freshAnalysis?.observation
        let freshScopeObservation: VisionFingerGunObservation? = if let freshAnalysis, let hand {
            ScopePosePolicy.observation(from: freshAnalysis, hand: hand, tuning: tuning)
        } else {
            nil
        }
        let freshAimObservation: VisionFingerGunObservation? = if let freshAnalysis, let hand {
            makeAimObservation(from: freshAnalysis, hand: hand)
        } else {
            nil
        }
        var visibleObservation = freshObservation
        var solution: AimSolution?
        var calibration: VisionAimCalibration?
        // Why no reticle this frame. A missing aim solution used to be entirely
        // invisible: the HUD still read CALIBRATED and ARMED while nothing could
        // ever be drawn or fired.
        var aimRejection: String?

        let detectedMode = scopeModeDetector.update(
            hand: hand,
            imageSize: result.orientedImageSize,
            timestamp: result.timestamp,
            zoomFactor: currentZoomFactor(),
            enabled: !calibrationRequested,
            entryEligible: freshScopeObservation != nil
        )
        let scopeDiagnostic = scopeModeDetector.diagnostic
        if detectedMode != activeAimingMode {
            transitionAimingMode(to: detectedMode)
        }
        let mode = activeAimingMode
        if mode == .sights, visibleObservation == nil {
            visibleObservation = freshScopeObservation
        }

        if calibrationRequested,
           let freshAnalysis,
           let feature = freshAnalysis.aimFeature,
           let hand,
           var collector = calibrationCollector {
            if let completed = collector.ingest(
                feature: feature,
                variation: .singleBarrel,
                thumbState: freshAnalysis.thumbState,
                confidence: hand.confidence
            ) {
                calibrationStore.save(completed)
                calibration = completed
                calibrationRequested = false
                calibrationFailureMessage = nil
                aimSolver.reset()
                lastAimTimestamp = nil
                lastAimSolution = nil
            } else if let failure = collector.failureReason {
                calibrationFailureMessage = failure
                calibrationRequested = false
            }
            calibrationCollector = collector
        }

        if let freshAimObservation {
            lastValidTimestamp = result.timestamp
            if !calibrationRequested {
                calibration = calibration ?? calibrationStore.calibration(
                    for: freshAimObservation.variation,
                    cameraIdentifier: cameraIdentifier
                )
            }
            if mode == .unscoped, let calibration {
                let solved = aimSolver.solve(
                    observation: freshAimObservation,
                    calibration: calibration,
                    timestamp: result.timestamp
                )
                if let solved {
                    solution = solved
                    lastAimSolution = solved
                    lastAimTimestamp = result.timestamp
                } else if let lastAimTimestamp,
                          result.timestamp - lastAimTimestamp <= tuning.visionAimHoldSeconds {
                    solution = lastAimSolution
                    aimRejection = "HOLD"
                } else if let axis = calibration.directionalBasis?.degenerateAxis {
                    aimRejection = "CAL AXIS \(axis)"
                } else {
                    aimRejection = "SOLVER"
                }
            } else if mode == .sights {
                aimSolver.reset()
                lastAimTimestamp = nil
                lastAimSolution = nil
            } else {
                aimRejection = "NO CAL"
            }
        } else {
            if let lastValidTimestamp,
               result.timestamp - lastValidTimestamp <= tuning.trackingGraceSeconds {
                visibleObservation = observation
            }
            if mode == .unscoped,
               let lastAimTimestamp,
               result.timestamp - lastAimTimestamp <= tuning.visionAimHoldSeconds {
                solution = lastAimSolution
            } else {
                aimSolver.reset()
                lastAimTimestamp = nil
                lastAimSolution = nil
            }
        }

        let triggerObservation: VisionFingerGunObservation?
        if let freshObservation {
            triggerObservation = freshObservation
        } else if mode == .sights, let freshScopeObservation {
            triggerObservation = freshScopeObservation
        } else if solution != nil,
                  let freshAimObservation,
                  let freshAnalysis,
                  !(freshAnalysis.ringState == .straight && freshAnalysis.littleState == .straight) {
            // A calibrated directional template supplies the missing evidence
            // when an end-on index is mislabeled CURLED by 2D foreshortening.
            triggerObservation = VisionFingerGunObservation(
                variation: freshAnalysis.calibrationVariation ?? .singleBarrel,
                muzzlePoint: freshAimObservation.muzzlePoint,
                aimFeature: freshAimObservation.aimFeature,
                confidence: freshAimObservation.confidence,
                poseMargin: 0,
                thumbState: freshAnalysis.thumbState
            )
        } else {
            triggerObservation = nil
        }
        let armedObservation = AimingModePolicy.triggerObservation(
            mode: mode,
            observation: triggerObservation,
            hasCalibration: calibration != nil,
            hasAimSolution: solution != nil
        )
        let latchedThumbState: ThumbState? = if armedObservation == nil,
                                               freshAnalysis?.thumbState == .down,
                                               freshAimObservation != nil,
                                               mode == .sights || (calibration != nil && solution != nil) {
            // HandSelector continuity keeps this tied to the same selected hand.
            // GestureStateMachine accepts it only immediately after a fully
            // validated ARMED pose, so a fresh open palm cannot arm or fire.
            .down
        } else {
            nil
        }
        let gesture = stateMachine.update(
            with: armedObservation,
            fallbackThumbState: latchedThumbState,
            timestamp: result.timestamp
        )
        let fps = updateMetrics()
        let mediaPipeFPS = lastMediaPipeFramesPerSecond
        let mediaPipeLatency = lastMediaPipeLatencyMilliseconds
        let droppedFrames = visionDroppedFrames
        let currentState: CalibrationState
        if let calibrationFailure = calibrationFailureMessage {
            currentState = .failed(calibrationFailure)
        } else if calibrationRequested {
            currentState = .collecting(progress: calibrationCollector?.overallProgress ?? 0, handedness: nil)
        } else if calibration != nil {
            currentState = .calibrated(.unknown)
        } else if calibrationStore.calibration(cameraIdentifier: cameraIdentifier) != nil {
            currentState = .calibrated(.unknown)
        } else {
            currentState = .required(nil)
        }
        let target = calibrationRequested ? calibrationCollector?.currentTarget : nil
        let targetProgress = calibrationRequested ? calibrationCollector?.targetProgress ?? 0 : 0
        let instruction = calibrationRequested ? calibrationCollector?.instruction : nil
        let visibleAim = AimingModePolicy.visibleAim(mode: mode, solution: solution)
        // Unscoped shots resolve where the reticle points; scoped shots always
        // resolve to the fixed centre. Both still run the same gameplay
        // targeting evaluation, so a scoped hit registers like any other.
        let gameplayAimPoint = AimingModePolicy.gameplayPoint(mode: mode, aim: solution)
        let targetingState = gameplayAimPoint.map { point -> GameplayTargetingState in
            let zonePoint = mode == .sights ? point : (solution?.screenPoint ?? point)
            if let lastPersonTarget {
                return lastPersonTarget.targetingState(
                    gameplayPoint: point,
                    zonePoint: zonePoint,
                    frameTimestamp: result.timestamp
                )
            }
            return GameplayTargetEvaluator.evaluate(
                gameplayPoint: point,
                zonePoint: zonePoint,
                targetBoundingBox: nil,
                collisionMask: nil,
                targetScore: 0,
                targetTimestamp: nil,
                frameTimestamp: result.timestamp
            )
        }
        let flashPoint = gesture.fired ? targetingState?.gameplayPoint : nil
        let gameplayShot: GameplayShotEvent?
        if gesture.fired, let targetingState {
            shotEventID += 1
            gameplayShot = GameplayShotEvent(id: shotEventID, targeting: targetingState)
        } else {
            gameplayShot = nil
        }

        recorder.append(LandmarkRecordingFrame(
            timestamp: result.timestamp,
            hand: hand,
            observation: nil,
            aim: visibleAim,
            calibration: nil,
            analysis: nil,
            visionAnalysis: freshAnalysis,
            visionCalibration: calibration,
            gestureState: gesture.state,
            fired: gesture.fired,
            flashPoint: flashPoint,
            gameplayShot: gameplayShot.map { GameplayShotDiagnostic($0.targeting) },
            aimingMode: mode,
            scopeProximity: scopeModeDetector.diagnostic,
            nearbyInteraction: nearbyInteractionDiagnostic
        ))
        recorder.appendTrackerSample(TrackerLandmarkSample(
            timestamp: result.timestamp,
            source: .vision,
            hands: result.hands,
            latencyMilliseconds: latency
        ))

        publish { [weak self] in
            guard let self else { return }
            trackedHand = hand
            visionAnalysis = freshAnalysis
            observation = visibleObservation
            aim = visibleAim
            aimingMode = mode
            scopeProximity = scopeDiagnostic
            // Sights never run the solver, so no unscoped reason applies.
            aimRejectionReason = mode == .sights
                ? "SIGHTS"
                : (solution == nil ? (aimRejection ?? "NO POSE") : aimRejection)
            gestureState = gesture.state
            calibrationState = currentState
            currentCalibrationTarget = target
            calibrationTargetProgress = targetProgress
            calibrationInstruction = instruction
            gameplayTargetingState = targetingState
            metrics = AnalysisMetrics(
                framesPerSecond: fps,
                latencyMilliseconds: latency,
                mediaPipeFramesPerSecond: mediaPipeFPS,
                mediaPipeLatencyMilliseconds: mediaPipeLatency,
                visionDroppedFrames: droppedFrames
            )
            orientedImageSize = result.orientedImageSize
            if gesture.fired {
                shotEvent = gameplayShot
                if let flashPoint {
                    triggerFlash(at: flashPoint)
                } else if let muzzle = triggerObservation?.muzzlePoint {
                    triggerFlash(at: muzzle)
                }
            }
        }
    }

    private func makeAimObservation(
        from analysis: VisionFingerGunAnalysis,
        hand: TrackedHand
    ) -> VisionFingerGunObservation? {
        guard let feature = analysis.aimFeature,
              let indexTip = hand[image: .indexTip],
              indexTip.confidence >= tuning.visionMinimumJointConfidence else { return nil }
        return VisionFingerGunObservation(
            variation: .singleBarrel,
            muzzlePoint: indexTip.location,
            aimFeature: feature,
            confidence: hand.confidence,
            poseMargin: 0,
            thumbState: analysis.thumbState
        )
    }

    private func transitionAimingMode(to mode: AimingMode) {
        guard mode != activeAimingMode else { return }
        activeAimingMode = mode
        stateMachine.reset()
        aimSolver.reset()
        lastValidTimestamp = nil
        lastAimTimestamp = nil
        lastAimSolution = nil
        setCameraZoom(for: mode)
    }

    private func resetGameplayForUnscopedMode() {
        scopeModeDetector.reset()
        activeAimingMode = .unscoped
        stateMachine.reset()
        aimSolver.reset()
        lastValidTimestamp = nil
        lastAimTimestamp = nil
        lastAimSolution = nil
        setCameraZoom(for: .unscoped)
        publish {
            self.aimingMode = .unscoped
            self.scopeProximity = nil
            self.aim = nil
            self.gestureState = .notDetected
            self.flash = nil
        }
    }

    /// The zoom ramp is asynchronous, so proximity is normalised by the zoom
    /// actually in effect for this frame rather than the requested target.
    private func currentZoomFactor() -> Double {
        Double(captureDevice?.videoZoomFactor ?? 1)
    }

    private func setCameraZoom(for mode: AimingMode) {
        let requestedZoom = CGFloat(mode == .sights ? tuning.scopeZoomFactor : 1)
        sessionQueue.async { [weak self] in
            guard let device = self?.captureDevice else { return }
            do {
                try device.lockForConfiguration()
                defer { device.unlockForConfiguration() }
                device.cancelVideoZoomRamp()
                let zoom = min(
                    max(requestedZoom, device.minAvailableVideoZoomFactor),
                    device.maxAvailableVideoZoomFactor
                )
                device.ramp(toVideoZoomFactor: zoom, withRate: 2)
            } catch {
                // Sights remain usable with a fixed center reticle if zoom cannot
                // be configured on this camera or during an interruption.
            }
        }
    }

    private func updateMetrics() -> Double {
        frameCounter += 1
        let now = CACurrentMediaTime()
        let elapsed = now - metricWindowStart
        guard elapsed >= 0.5 else { return lastFramesPerSecond }
        lastFramesPerSecond = Double(frameCounter) / elapsed
        frameCounter = 0
        metricWindowStart = now
        return lastFramesPerSecond
    }

    private func processMediaPipeDiagnostic(_ result: HandTrackingResult, latency: Double) {
        let hand = mediaPipeSelector.select(result.hands, timestamp: result.timestamp)
        let analysis = hand.map { classifier.analyze($0) }
        mediaPipeFrameCounter += 1
        let now = CACurrentMediaTime()
        let elapsed = now - mediaPipeMetricWindowStart
        if elapsed >= 0.5 {
            lastMediaPipeFramesPerSecond = Double(mediaPipeFrameCounter) / elapsed
            mediaPipeFrameCounter = 0
            mediaPipeMetricWindowStart = now
        }
        lastMediaPipeLatencyMilliseconds = latency
        let currentFPS = lastMediaPipeFramesPerSecond
        recorder.appendTrackerSample(TrackerLandmarkSample(
            timestamp: result.timestamp,
            source: .mediaPipe,
            hands: result.hands,
            latencyMilliseconds: latency
        ))
        publish { [weak self] in
            guard let self else { return }
            mediaPipeHand = hand
            mediaPipeAnalysis = analysis
            metrics.mediaPipeFramesPerSecond = currentFPS
            metrics.mediaPipeLatencyMilliseconds = latency
        }
    }

    private func submitMediaPipeDiagnostic(_ frame: CameraFrame) {
        guard mediaPipeDiagnosticsEnabled, let tracker else { return }
        let submittedAt = CACurrentMediaTime()
        do {
            try tracker.submit(frame) { [weak self] result in
                guard let self else { return }
                analysisQueue.async {
                    guard case .success(let tracking) = result else { return }
                    self.processMediaPipeDiagnostic(
                        tracking,
                        latency: (CACurrentMediaTime() - submittedAt) * 1_000
                    )
                }
            }
        } catch { /* MediaPipe is diagnostics-only. Vision remains operational. */ }
    }

    private func triggerFlash(at point: CGPoint) {
        flashID += 1
        let event = FlashEvent(id: flashID, point: point)
        flash = event
        DispatchQueue.main.asyncAfter(deadline: .now() + tuning.flashDuration) { [weak self] in
            guard self?.flash?.id == event.id else { return }
            self?.flash = nil
        }
    }

    private func publish(_ changes: @escaping () -> Void) { DispatchQueue.main.async(execute: changes) }

    private func observeSessionNotifications() {
        NotificationCenter.default.addObserver(self, selector: #selector(sessionWasInterrupted), name: AVCaptureSession.wasInterruptedNotification, object: session)
        NotificationCenter.default.addObserver(self, selector: #selector(sessionInterruptionEnded), name: AVCaptureSession.interruptionEndedNotification, object: session)
        NotificationCenter.default.addObserver(self, selector: #selector(sessionRuntimeError(_:)), name: AVCaptureSession.runtimeErrorNotification, object: session)
    }

    @objc private func sessionWasInterrupted() {
        analysisQueue.async { [weak self] in self?.resetGameplayForUnscopedMode() }
        publish { self.status = .interrupted("Camera interrupted. The demo will resume automatically.") }
    }
    @objc private func sessionInterruptionEnded() { start() }
    @objc private func sessionRuntimeError(_ notification: Notification) {
        analysisQueue.async { [weak self] in self?.resetGameplayForUnscopedMode() }
        let error = notification.userInfo?[AVCaptureSessionErrorKey] as? AVError
        if error?.code == .mediaServicesWereReset {
            publish { self.status = .interrupted("Camera services restarted. Reconnecting…") }
            configureAndStart()
        } else {
            publish { self.status = .unavailable(error?.localizedDescription ?? "Camera runtime error.") }
        }
    }

    private enum CameraSetupError: LocalizedError {
        case noRearCamera, cannotAddInput, cannotAddOutput
        var errorDescription: String? {
            switch self {
            case .noRearCamera: return "No rear wide-angle camera was found."
            case .cannotAddInput: return "The rear camera input could not be configured."
            case .cannotAddOutput: return "Video frame processing could not be configured."
            }
        }
    }
}

extension CameraService: AVCaptureVideoDataOutputSampleBufferDelegate {
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let frame = CameraFrame(
            pixelBuffer: pixelBuffer,
            timestamp: CMSampleBufferGetPresentationTimeStamp(sampleBuffer),
            orientation: .right,
            orientedImageSize: CGSize(width: CVPixelBufferGetHeight(pixelBuffer), height: CVPixelBufferGetWidth(pixelBuffer))
        )
        diagnosticVision.submit(frame) { [weak self] result, latency in
            guard let self else { return }
            analysisQueue.async {
                switch result {
                case .success(let tracking): self.processVisionPrimary(tracking, latency: latency)
                case .failure:
                    // A capture frame can occasionally be dropped while the system
                    // is under camera/ML load. This is not a camera-session failure.
                    self.visionDroppedFrames += 1
                    let dropped = self.visionDroppedFrames
                    self.publish { self.metrics.visionDroppedFrames = dropped }
                }
            }
            // Optional consumers are chained after Vision so no service reads the
            // same CVPixelBuffer concurrently.
            let target = self.targetLock.withLock { (self.personTargetingEnabled, self.targetProfile) }
            let submitted = target.0 && self.personTargeting.submit(
                frame,
                targetProfile: target.1
            ) { [weak self] target in
                guard let self else { return }
                self.analysisQueue.async {
                    self.lastPersonTarget = target
                    self.publish { self.personTarget = target }
                }
                self.submitMediaPipeDiagnostic(frame)
            }
            if !submitted {
                self.submitMediaPipeDiagnostic(frame)
            }
        }
    }
}

private struct VisionLandmarkSmoother: Sendable {
    private var previous: [LandmarkJoint: CGPoint] = [:]
    private var lastTimestamp: TimeInterval?

    mutating func smooth(_ hand: TrackedHand?, timestamp: TimeInterval) -> TrackedHand? {
        guard let hand else {
            if let lastTimestamp, timestamp - lastTimestamp > 0.2 { reset() }
            return nil
        }
        if let lastTimestamp, timestamp - lastTimestamp > 0.2 { reset() }
        var smoothed = hand.imagePoints
        for (joint, landmark) in hand.imagePoints {
            guard let old = previous[joint] else {
                previous[joint] = landmark.location
                continue
            }
            let movement = hypot(landmark.location.x - old.x, landmark.location.y - old.y)
            // Suppress small frame-to-frame landmark noise while allowing deliberate
            // hand motion to catch up quickly.
            let alpha = min(max(0.28 + movement * 7, 0.28), 0.78)
            let point = CGPoint(
                x: old.x + (landmark.location.x - old.x) * alpha,
                y: old.y + (landmark.location.y - old.y) * alpha
            )
            previous[joint] = point
            smoothed[joint] = ImageLandmark(location: point, confidence: landmark.confidence)
        }
        self.lastTimestamp = timestamp
        return TrackedHand(
            imagePoints: smoothed,
            worldPoints: hand.worldPoints,
            handedness: hand.handedness,
            confidence: hand.confidence,
            timestamp: hand.timestamp,
            palmFrame: hand.palmFrame
        )
    }

    mutating func reset() {
        previous.removeAll(keepingCapacity: true)
        lastTimestamp = nil
    }
}

private final class VisionDiagnosticRunner: @unchecked Sendable {
    private let tracker = VisionHandPoseDetector()
    private let queue = DispatchQueue(label: "camera.vision.diagnostic.queue", qos: .utility)
    private let lock = NSLock()
    private var busy = false

    func submit(
        _ frame: CameraFrame,
        completion: @escaping @Sendable (Result<HandTrackingResult, Error>, Double) -> Void
    ) {
        let accepted = lock.withLock {
            guard !busy else { return false }
            busy = true
            return true
        }
        guard accepted else { return }
        queue.async { [weak self] in
            guard let self else { return }
            let started = CACurrentMediaTime()
            do {
                try tracker.submit(frame) { [weak self] result in
                    let latency = (CACurrentMediaTime() - started) * 1_000
                    self?.lock.withLock { self?.busy = false }
                    completion(result, latency)
                }
            } catch {
                lock.withLock { self.busy = false }
                completion(.failure(error), (CACurrentMediaTime() - started) * 1_000)
            }
        }
    }
}

private struct HandSelector: Sendable {
    private var lockedHandedness: Handedness?
    private var lastWrist: CGPoint?
    private var lastTimestamp: TimeInterval?

    mutating func select(
        _ candidates: [TrackedHand],
        timestamp: TimeInterval
    ) -> TrackedHand? {
        guard !candidates.isEmpty else {
            if let lastTimestamp, timestamp - lastTimestamp > 0.2 { reset() }
            return nil
        }
        let sameHand = candidates.filter { lockedHandedness == nil || $0.handedness == lockedHandedness }
        let pool = sameHand.isEmpty ? candidates : sameHand
        let selected = pool.max { lhs, rhs in score(lhs) < score(rhs) }
        if var selected {
            let wrist = selected[image: .wrist]?.location
            let timeIsContinuous = lastTimestamp.map { timestamp - $0 <= 0.2 } ?? false
            let wristIsContinuous: Bool
            if let lastWrist, let wrist {
                wristIsContinuous = hypot(lastWrist.x - wrist.x, lastWrist.y - wrist.y) <= 0.25
            } else {
                wristIsContinuous = false
            }
            let isContinuous = timeIsContinuous && wristIsContinuous

            // End-on hands can make MediaPipe's handedness label flicker. Keep
            // the identity chosen for a spatially continuous track so calibration
            // and its palm-local geometry do not switch sides frame-to-frame.
            if isContinuous, let lockedHandedness, selected.handedness != lockedHandedness {
                selected = selected.relabelled(as: lockedHandedness)
            } else {
                lockedHandedness = selected.handedness
            }
            lastWrist = wrist
            lastTimestamp = timestamp
            return selected
        }
        return nil
    }

    private func score(_ candidate: TrackedHand) -> Double {
        var value = Double(candidate.confidence)
        if let bounds = candidate.bounds { value += Double(bounds.width * bounds.height) }
        if let lastWrist, let wrist = candidate[image: .wrist]?.location {
            value -= Double(hypot(wrist.x - lastWrist.x, wrist.y - lastWrist.y))
        }
        return value
    }

    private mutating func reset() { lockedHandedness = nil; lastWrist = nil; lastTimestamp = nil }
}

private extension TrackedHand {
    func relabelled(as handedness: Handedness) -> TrackedHand {
        TrackedHand(
            imagePoints: imagePoints,
            worldPoints: worldPoints,
            handedness: handedness,
            confidence: confidence,
            timestamp: timestamp,
            palmFrame: PalmCoordinateFrame.make(points: worldPoints, handedness: handedness)
        )
    }
}

private final class UnavailableHandTracker: HandTracking, @unchecked Sendable {
    let name = "MEDIAPIPE ERROR"
    let error: Error
    init(error: Error) { self.error = error }
    func submit(_ frame: CameraFrame, completion: @escaping @Sendable (Result<HandTrackingResult, Error>) -> Void) throws { throw error }
}
