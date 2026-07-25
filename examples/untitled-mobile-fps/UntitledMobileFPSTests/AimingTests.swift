import XCTest
@testable import UntitledMobileFPS

final class AimingTests: XCTestCase {
    func testCalibrationCentersNeutralDirection() throws {
        var collector = AimCalibrationCollector(cameraIdentifier: "camera")
        collector.begin(handedness: .right)
        var calibration: AimCalibration?
        let sample = BarrelCalibrationSample(direction: .init(x: 0, y: 0, z: 1), handedness: .right, confidence: 0.99)
        for _ in 0..<30 { calibration = collector.ingest(sample) ?? calibration }
        let value = try XCTUnwrap(calibration)
        var solver = AngularAimSolver()
        let solution = try XCTUnwrap(solver.solve(
            observation: .observation(direction: .init(x: 0, y: 0, z: 1)),
            calibration: value,
            timestamp: 1,
            horizontalFieldOfView: 40,
            verticalFieldOfView: 65
        ))
        XCTAssertEqual(solution.screenPoint.x, 0.5, accuracy: 0.001)
        XCTAssertEqual(solution.screenPoint.y, 0.5, accuracy: 0.001)
    }

    func testYawMovesReticleWithoutUsingMuzzlePosition() throws {
        let calibration = calibration()
        var leftMuzzleSolver = AngularAimSolver()
        var rightMuzzleSolver = AngularAimSolver()
        var first = FingerGunObservation.observation(direction: CameraSpaceVector(x: 0.18, y: 0, z: 0.98))
        var second = first
        first = FingerGunObservation(variation: first.variation, muzzlePoint: CGPoint(x: 0.1, y: 0.2), barrelDirection: first.barrelDirection, confidence: first.confidence, poseMargin: first.poseMargin, thumbState: first.thumbState, handedness: first.handedness)
        second = FingerGunObservation(variation: second.variation, muzzlePoint: CGPoint(x: 0.9, y: 0.8), barrelDirection: second.barrelDirection, confidence: second.confidence, poseMargin: second.poseMargin, thumbState: second.thumbState, handedness: second.handedness)
        let a = try XCTUnwrap(leftMuzzleSolver.solve(observation: first, calibration: calibration, timestamp: 1, horizontalFieldOfView: 40, verticalFieldOfView: 65))
        let b = try XCTUnwrap(rightMuzzleSolver.solve(observation: second, calibration: calibration, timestamp: 1, horizontalFieldOfView: 40, verticalFieldOfView: 65))
        XCTAssertEqual(a.screenPoint, b.screenPoint)
        XCTAssertGreaterThan(a.screenPoint.x, 0.5)
    }

    func testTowardCameraAndNearSidewaysDirectionsAreRejected() {
        var solver = AngularAimSolver()
        XCTAssertNil(solver.solve(observation: .observation(direction: .init(x: 0, y: 0, z: -1)), calibration: calibration(), timestamp: 1, horizontalFieldOfView: 40, verticalFieldOfView: 65))
        XCTAssertNil(solver.solve(observation: .observation(direction: .init(x: 0.99, y: 0, z: 0.05)), calibration: calibration(), timestamp: 1, horizontalFieldOfView: 40, verticalFieldOfView: 65))
    }

    func testReplayIsDeterministicAndTimestampOrdered() throws {
        let later = LandmarkRecordingFrame(timestamp: 2, hand: nil, observation: nil, aim: nil, calibration: nil)
        let earlier = LandmarkRecordingFrame(timestamp: 1, hand: nil, observation: nil, aim: nil, calibration: nil)
        let recording = LandmarkRecording(schemaVersion: 1, modelVersion: AimCalibration.modelVersion, startedAt: Date(), frames: [later, earlier])
        let decoded = try LandmarkReplay.load(data: JSONEncoder().encode(recording))
        var timestamps: [TimeInterval] = []
        LandmarkReplay.replay(decoded) { timestamps.append($0.timestamp) }
        XCTAssertEqual(timestamps, [1, 2])
    }

    func testVersionTwoRecordingPreservesIndependentTrackerSamples() throws {
        let sample = TrackerLandmarkSample(
            timestamp: 1.25,
            source: .vision,
            hands: [],
            latencyMilliseconds: 12.5
        )
        let recording = LandmarkRecording(
            schemaVersion: 2,
            modelVersion: AimCalibration.modelVersion,
            startedAt: Date(),
            frames: [],
            trackerSamples: [sample]
        )
        let decoded = try LandmarkReplay.load(data: JSONEncoder().encode(recording))
        XCTAssertEqual(decoded.trackerSamples, [sample])
    }

    func testVersionTwoRecordingPreservesOptionalGameplayShotDiagnostic() throws {
        let targeting = GameplayTargetingState(
            gameplayPoint: CGPoint(x: 0.75, y: 0.53),
            zonePoint: CGPoint(x: 0.8, y: 0.2),
            targetBoundingBox: CGRect(x: 0.3, y: 0.35, width: 0.2, height: 0.4),
            targetAgeSeconds: 0.2,
            targetScore: 0.72,
            maskCoverage: 0.4,
            maskContainsReticle: true,
            status: .ready
        )
        let frame = LandmarkRecordingFrame(
            timestamp: 1,
            hand: nil,
            observation: nil,
            aim: nil,
            calibration: nil,
            fired: true,
            gameplayShot: GameplayShotDiagnostic(targeting),
            nearbyInteraction: NearbyInteractionRecordingDiagnostic(
                status: "UWB ranging",
                distanceMeters: 1.25,
                direction: [0, 0, -1],
                sampledAtMs: 42
            )
        )
        let recording = LandmarkRecording(
            schemaVersion: 2,
            modelVersion: AimCalibration.modelVersion,
            startedAt: Date(),
            frames: [frame],
            trackerSamples: []
        )

        let decoded = try LandmarkReplay.load(data: JSONEncoder().encode(recording))

        XCTAssertEqual(decoded.frames.first?.gameplayShot, GameplayShotDiagnostic(targeting))
        XCTAssertEqual(
            decoded.frames.first?.nearbyInteraction,
            NearbyInteractionRecordingDiagnostic(
                status: "UWB ranging",
                distanceMeters: 1.25,
                direction: [0, 0, -1],
                sampledAtMs: 42
            )
        )
    }

    func testDiagnosticRecorderFindsTheLatestSavedExport() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("finger-gun-recordings-\(UUID().uuidString)")
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: directory) }

        let older = directory.appendingPathComponent(
            "finger-gun-older-landmarks.json"
        )
        let newer = directory.appendingPathComponent(
            "finger-gun-newer-landmarks.json"
        )
        let unrelated = directory.appendingPathComponent("notes.json")
        for url in [older, newer, unrelated] {
            XCTAssertTrue(FileManager.default.createFile(
                atPath: url.path,
                contents: Data()
            ))
        }
        try FileManager.default.setAttributes(
            [.modificationDate: Date(timeIntervalSince1970: 1)],
            ofItemAtPath: older.path
        )
        try FileManager.default.setAttributes(
            [.modificationDate: Date(timeIntervalSince1970: 2)],
            ofItemAtPath: newer.path
        )
        try FileManager.default.setAttributes(
            [.modificationDate: Date(timeIntervalSince1970: 3)],
            ofItemAtPath: unrelated.path
        )

        XCTAssertEqual(
            DiagnosticRecorder.latestRecordingURL(in: directory)?
                .resolvingSymlinksInPath(),
            newer.resolvingSymlinksInPath()
        )
    }

    func testFiveAxisTargetVisionCalibrationMapsKnownFeatures() throws {
        var tuning = GestureTuning.default
        tuning.visionCalibrationFramesPerTarget = 3
        tuning.visionCalibrationSettlingFrames = 1
        tuning.visionCalibrationFeatureJumpLimit = 10
        var collector = VisionAimCalibrationCollector(tuning: tuning, cameraIdentifier: "camera")
        collector.begin()
        var calibration: VisionAimCalibration?

        for target in VisionCalibrationTarget.allCases {
            let feature = visionFeature(for: target.point)
            for _ in 0..<6 {
                calibration = collector.ingest(
                    feature: feature,
                    variation: .singleBarrel,
                    thumbState: .up,
                    confidence: 0.99
                ) ?? calibration
            }
        }

        let fitted = try XCTUnwrap(calibration)
        XCTAssertLessThan(fitted.rootMeanSquareError, 0.08)
        for (index, target) in VisionCalibrationTarget.allCases.enumerated() {
            var solver = VisionAimSolver(tuning: tuning)
            let observation = VisionFingerGunObservation(
                variation: .singleBarrel,
                muzzlePoint: .zero,
                aimFeature: visionFeature(for: target.point),
                confidence: 0.99,
                poseMargin: 0.5,
                thumbState: .up
            )
            let solution = try XCTUnwrap(solver.solve(observation: observation, calibration: fitted, timestamp: Double(index + 1)))
            XCTAssertEqual(solution.rawScreenPoint.x, target.point.x, accuracy: 0.06)
            XCTAssertEqual(solution.rawScreenPoint.y, target.point.y, accuracy: 0.06)
        }

        var diagonalSolver = VisionAimSolver(tuning: tuning)
        let lowerLeftObservation = VisionFingerGunObservation(
            variation: .singleBarrel,
            muzzlePoint: .zero,
            aimFeature: visionFeature(for: CGPoint(x: 0.22, y: 0.24)),
            confidence: 0.99,
            poseMargin: 0.5,
            thumbState: .up
        )
        var lowerLeftSolution: AimSolution?
        for frame in 0..<tuning.visionDirectionStabilizationFrames {
            lowerLeftSolution = diagonalSolver.solve(
                observation: lowerLeftObservation,
                calibration: fitted,
                timestamp: 100 + Double(frame) / 30
            )
        }
        XCTAssertEqual(lowerLeftSolution?.screenPoint, AimDirectionZone.downLeft.point)
    }

    func testVisionCalibrationDoesNotAdvanceWithoutMovingToNextTarget() {
        var tuning = GestureTuning.default
        tuning.visionCalibrationFramesPerTarget = 2
        tuning.visionCalibrationSettlingFrames = 0
        tuning.visionCalibrationTargetChangeMinimum = 0.05
        var collector = VisionAimCalibrationCollector(tuning: tuning, cameraIdentifier: "camera")
        let unchanged = visionFeature(for: CGPoint(x: 0.5, y: 0.76))

        for _ in 0..<2 {
            _ = collector.ingest(feature: unchanged, variation: .singleBarrel, thumbState: .up, confidence: 0.99)
        }
        XCTAssertEqual(collector.currentTarget, .left)
        XCTAssertTrue(collector.awaitingTargetMovement)

        for _ in 0..<30 {
            _ = collector.ingest(feature: unchanged, variation: .singleBarrel, thumbState: .up, confidence: 0.99)
        }
        XCTAssertEqual(collector.currentTarget, .left)
        XCTAssertEqual(collector.targetProgress, 0)
        XCTAssertNil(collector.failureReason)
    }

    func testDefaultCalibrationRequiresAFullCenterHoldBeforeShowingLeft() {
        let tuning = GestureTuning.default
        var collector = VisionAimCalibrationCollector(tuning: tuning, cameraIdentifier: "camera")
        let center = visionFeature(for: VisionCalibrationTarget.center.point)
        let framesBeforeCompletion = tuning.visionCalibrationSettlingFrames
            + tuning.visionCalibrationFramesPerTarget - 1

        for _ in 0..<framesBeforeCompletion {
            _ = collector.ingest(feature: center, variation: .singleBarrel, thumbState: .up, confidence: 0.99)
        }
        XCTAssertEqual(collector.currentTarget, .center)

        _ = collector.ingest(feature: center, variation: .singleBarrel, thumbState: .up, confidence: 0.99)
        XCTAssertEqual(collector.currentTarget, .left)
        XCTAssertTrue(collector.awaitingTargetMovement)
    }

    func testVisionCalibrationRejectsDegenerateFit() {
        var tuning = GestureTuning.default
        tuning.visionCalibrationFramesPerTarget = 1
        tuning.visionCalibrationSettlingFrames = 0
        tuning.visionCalibrationTargetChangeMinimum = 0
        tuning.visionCalibrationTargetSeparationMinimum = 0
        tuning.visionCalibrationMaximumRMSE = 0.05
        var collector = VisionAimCalibrationCollector(tuning: tuning, cameraIdentifier: "camera")
        let unchanged = visionFeature(for: CGPoint(x: 0.5, y: 0.5))

        for _ in 0..<17 {
            _ = collector.ingest(feature: unchanged, variation: .singleBarrel, thumbState: .up, confidence: 0.99)
        }
        XCTAssertNil(collector.currentTarget)
        XCTAssertNotNil(collector.failureReason)
    }

    func testFieldCalibrationWithCollapsedRightAxisNeverSolves() throws {
        // Captured from a device recording where the app stayed ARMED for 224 of
        // 420 frames yet produced zero aim solutions and zero shots. The CENTER
        // hold landed on top of the RIGHT hold, so the right anchor projects to
        // +0.01 and the solver's anchor gate rejects every frame forever.
        let calibration = Self.collapsedRightAxisCalibration
        let basis = try XCTUnwrap(calibration.directionalBasis)

        XCTAssertLessThan(basis.leftAnchor, -VisionAimDirectionalBasis.minimumAnchorSeparation)
        XCTAssertLessThan(basis.rightAnchor, VisionAimDirectionalBasis.minimumAnchorSeparation)
        XCTAssertEqual(basis.degenerateAxis, "left and right")
        XCTAssertFalse(calibration.producesUsableAim)

        var solver = VisionAimSolver()
        for (index, target) in VisionCalibrationTarget.allCases.enumerated() {
            let observation = VisionFingerGunObservation(
                variation: .singleBarrel,
                muzzlePoint: .zero,
                aimFeature: visionFeature(for: target.point),
                confidence: 0.99,
                poseMargin: 0.5,
                thumbState: .up
            )
            XCTAssertNil(solver.solve(observation: observation, calibration: calibration, timestamp: Double(index + 1)))
        }
    }

    func testCollectorRejectsAFitWhoseCenterCollapsesOntoARequiredDirection() {
        var tuning = GestureTuning.default
        tuning.visionCalibrationFramesPerTarget = 3
        tuning.visionCalibrationSettlingFrames = 1
        tuning.visionCalibrationFeatureJumpLimit = 10
        tuning.visionCalibrationTargetSeparationMinimum = 0
        var collector = VisionAimCalibrationCollector(tuning: tuning, cameraIdentifier: "camera")
        collector.begin()
        // Holding the center target at the right pose passes raw separation and
        // RMSE, but leaves the solver with no usable horizontal axis.
        let poses: [CGPoint] = [
            VisionCalibrationTarget.right.point,
            VisionCalibrationTarget.left.point,
            VisionCalibrationTarget.right.point,
            VisionCalibrationTarget.up.point,
            VisionCalibrationTarget.down.point
        ]
        var calibration: VisionAimCalibration?

        for pose in poses {
            let feature = visionFeature(for: pose)
            for _ in 0..<6 {
                calibration = collector.ingest(
                    feature: feature,
                    variation: .singleBarrel,
                    thumbState: .up,
                    confidence: 0.99
                ) ?? calibration
            }
        }

        XCTAssertNil(calibration)
        XCTAssertEqual(
            collector.failureReason,
            "Calibration could not separate left and right aim. Exaggerate the left and right poses and try again."
        )
    }

    func testStoreReportsAnUnsolvableSavedCalibrationAsMissing() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "vision-aim-store-tests"))
        defaults.removePersistentDomain(forName: "vision-aim-store-tests")
        let store = VisionAimCalibrationStore(defaults: defaults)
        let camera = Self.collapsedRightAxisCalibration.cameraIdentifier

        store.save(Self.collapsedRightAxisCalibration)

        XCTAssertNil(store.calibration(cameraIdentifier: camera))
        defaults.removePersistentDomain(forName: "vision-aim-store-tests")
    }

    private static let collapsedRightAxisCalibration = VisionAimCalibration(
        featureMeans: [0.976423, 0.764251, 0.637617, 0.681935, 0.849385, 0.726486, 1.066584],
        featureScales: [0.734654, 0.826578, 0.405238, 0.578213, 0.541839, 0.706179, 0.415354],
        coefficientsX: [0.500000, 0.167715, 0.374115, 0.129332, -0.344521, -0.025827, 0.073099, -0.166514],
        coefficientsY: [0.500000, -0.123708, 0.435078, 0.088715, -0.401484, 0.025195, 0.168436, -0.039263],
        zoneCentroids: [
            VisionAimFeature(tipX: 1.638802, tipY: 1.284197, pipX: 0.913266, pipY: 1.064078, dipX: 1.304998, dipY: 1.182108, projectedLength: 1.511208),
            VisionAimFeature(tipX: -0.262867, tipY: 0.752750, pipX: -0.032613, pipY: 0.777894, dipX: -0.044445, dipY: 0.749406, projectedLength: 0.495740),
            VisionAimFeature(tipX: 1.782623, tipY: 0.782181, pipX: 1.087890, pipY: 0.680854, dipX: 1.456883, dipY: 0.731104, projectedLength: 1.380990),
            VisionAimFeature(tipX: 0.835141, tipY: 1.725433, pipX: 0.443296, pipY: 1.279678, dipX: 0.691668, dipY: 1.524595, projectedLength: 1.287678),
            VisionAimFeature(tipX: 0.888419, tipY: -0.723304, pipX: 0.776245, pipY: -0.392829, dipX: 0.837818, dipY: -0.554783, projectedLength: 0.657305)
        ],
        zoneRMS: [0.165028, 0.310283, 0.085834, 0.051910, 0.055780],
        templateMeans: [0.338807, 0.082316, 0.211768, 0.044551, 1.066584],
        templateScales: [0.364667, 0.258933, 0.168486, 0.137513, 0.415354],
        rootMeanSquareError: 0.074758,
        variation: .singleBarrel,
        cameraIdentifier: "com.apple.avfoundation.avcapturedevice.built-in_video:0",
        modelVersion: VisionAimCalibration.modelVersion,
        createdAt: Date(timeIntervalSinceReferenceDate: 806654285.508769)
    )

    func testDirectionalReticleCoversCenterCardinalsAndDiagonals() {
        var tuning = GestureTuning.default
        tuning.visionDirectionStabilizationFrames = 2
        var quantizer = DirectionalAimQuantizer(tuning: tuning)

        for zone in AimDirectionZone.allCases {
            _ = quantizer.filter(zone.point)
            let point = quantizer.filter(zone.point)
            XCTAssertEqual(quantizer.zone, zone)
            XCTAssertEqual(point, zone.point)
        }
    }

    func testVisionAimCalibrationIsSharedAcrossBarrelVariations() throws {
        let calibration = VisionAimCalibration(
            featureMeans: Array(repeating: 0, count: 7),
            featureScales: Array(repeating: 1, count: 7),
            coefficientsX: [0.5, 0, 0, 0, 0, 0, 0, 0],
            coefficientsY: [0.5, 0, 0, 0, 0, 0, 0, 0],
            zoneCentroids: VisionCalibrationTarget.allCases.map { visionFeature(for: $0.point) },
            zoneRMS: Array(repeating: 0, count: VisionCalibrationTarget.allCases.count),
            templateMeans: Array(repeating: 0, count: 5),
            templateScales: Array(repeating: 1, count: 5),
            rootMeanSquareError: 0,
            variation: .singleBarrel,
            cameraIdentifier: "camera",
            modelVersion: VisionAimCalibration.modelVersion,
            createdAt: Date()
        )
        let observation = VisionFingerGunObservation(
            variation: .doubleBarrel,
            muzzlePoint: .zero,
            aimFeature: visionFeature(for: CGPoint(x: 0.5, y: 0.5)),
            confidence: 0.99,
            poseMargin: 0.5,
            thumbState: .up
        )
        var solver = VisionAimSolver()
        XCTAssertNotNil(solver.solve(observation: observation, calibration: calibration, timestamp: 1))
    }

    func testVisionContinuousAimUsesTheFittedRegressionMapping() throws {
        let calibration = VisionAimCalibration(
            featureMeans: Array(repeating: 0, count: 7),
            featureScales: Array(repeating: 1, count: 7),
            coefficientsX: [0.37, 0, 0, 0, 0, 0, 0, 0],
            coefficientsY: [0.63, 0, 0, 0, 0, 0, 0, 0],
            zoneCentroids: VisionCalibrationTarget.allCases.map { visionFeature(for: $0.point) },
            zoneRMS: Array(repeating: 0, count: VisionCalibrationTarget.allCases.count),
            templateMeans: Array(repeating: 0, count: 5),
            templateScales: Array(repeating: 1, count: 5),
            rootMeanSquareError: 0,
            variation: .singleBarrel,
            cameraIdentifier: "camera",
            modelVersion: VisionAimCalibration.modelVersion,
            createdAt: Date()
        )
        let observation = VisionFingerGunObservation(
            variation: .singleBarrel,
            muzzlePoint: CGPoint(x: 0.9, y: 0.1),
            aimFeature: visionFeature(for: CGPoint(x: 0.78, y: 0.5)),
            confidence: 0.99,
            poseMargin: 0.5,
            thumbState: .up
        )
        var solver = VisionAimSolver()

        let solution = try XCTUnwrap(
            solver.solve(observation: observation, calibration: calibration, timestamp: 1)
        )

        XCTAssertEqual(solution.gameplayScreenPoint.x, 0.37, accuracy: 0.001)
        XCTAssertEqual(solution.gameplayScreenPoint.y, 0.63, accuracy: 0.001)
    }

    private func calibration() -> AimCalibration {
        AimCalibration(neutralDirection: .init(x: 0, y: 0, z: 1), neutralYaw: 0, neutralPitch: 0, neutralRoll: 0, angularVariance: 0, handedness: .right, cameraIdentifier: "camera", modelVersion: AimCalibration.modelVersion, createdAt: Date())
    }
}

private func visionFeature(for point: CGPoint) -> VisionAimFeature {
    let x = Double(point.x - 0.5)
    let y = Double(point.y - 0.5)
    return VisionAimFeature(
        tipX: 1.2 * x,
        tipY: 1.2 * y,
        pipX: 0.8 * x,
        pipY: 0.8 * y,
        dipX: x,
        dipY: y,
        projectedLength: 1 + 0.1 * x - 0.1 * y
    )
}

private extension FingerGunObservation {
    static func observation(direction: CameraSpaceVector) -> FingerGunObservation {
        FingerGunObservation(variation: .singleBarrel, muzzlePoint: CGPoint(x: 0.5, y: 0.5), barrelDirection: direction.normalized, confidence: 0.99, poseMargin: 0.5, thumbState: .up, handedness: .right)
    }
}
