import XCTest
@testable import UntitledMobileFPS

final class SightsAimingTests: XCTestCase {
    private let frameInterval = 1.0 / 30

    // MARK: - Proximity measurement

    func testSpanIsUnchangedByRollBecauseNormalisedCoordinatesAreAspectCorrected() throws {
        // A 16:9 frame stretches normalised x relative to y. Without the aspect
        // correction, simply rotating the hand would change the measured span by
        // up to the aspect ratio and swamp the proximity signal.
        let size = CGSize(width: 1080, height: 1920)
        let aspect = 1080.0 / 1920
        let upright = try XCTUnwrap(span(of: hand(scale: 0.3, aspect: aspect), imageSize: size))
        let rolled = try XCTUnwrap(span(of: hand(scale: 0.3, rotationDegrees: 90, aspect: aspect), imageSize: size))
        XCTAssertEqual(upright, rolled, accuracy: 0.001)

        let diagonal = try XCTUnwrap(span(of: hand(scale: 0.3, rotationDegrees: 37, aspect: aspect), imageSize: size))
        XCTAssertEqual(upright, diagonal, accuracy: 0.001)
    }

    func testSpanScalesWithApparentHandSizeAndDividesOutZoom() throws {
        let near = try XCTUnwrap(span(of: hand(scale: 0.4)))
        let far = try XCTUnwrap(span(of: hand(scale: 0.2)))
        XCTAssertEqual(near / far, 2, accuracy: 0.001)

        // A 2x zoom doubles every apparent distance without the hand moving, so
        // it must be divided out or the zoom ramp would feed back into entry.
        let zoomed = try XCTUnwrap(span(of: hand(scale: 0.4), zoomFactor: 2))
        XCTAssertEqual(zoomed, near / 2, accuracy: 0.001)
    }

    func testSpanIgnoresTheWristEntirely() throws {
        // Device recordings showed the wrist is both the first landmark lost as
        // the hand approaches (median confidence 0.14 on failed frames) and
        // geometrically unstable (IQR 0.29-0.34 of knuckle width vs 0.02-0.09
        // for knuckle pairs). Requiring it cost 24% of frames, biased toward
        // exactly the close poses sights need.
        let full = try XCTUnwrap(span(of: hand(scale: 0.3)))
        let noWrist = try XCTUnwrap(span(of: hand(scale: 0.3, omitting: [.wrist])))
        XCTAssertEqual(full, noWrist, accuracy: 0.000_001)

        let deadWrist = try XCTUnwrap(span(of: hand(scale: 0.3, confidences: [.wrist: 0.01])))
        XCTAssertEqual(full, deadWrist, accuracy: 0.000_001)
    }

    func testSpanSurvivesLosingOneKnuckle() throws {
        let full = try XCTUnwrap(span(of: hand(scale: 0.3)))
        // Losing a knuckle leaves fewer pairs, but each surviving pair still
        // estimates the same quantity, so the result stays comparable with a
        // baseline built from full frames rather than silently changing meaning.
        for joint in [LandmarkJoint.indexMCP, .middleMCP, .ringMCP, .littleMCP] {
            let partial = try XCTUnwrap(span(of: hand(scale: 0.3, omitting: [joint])), "lost \(joint)")
            XCTAssertEqual(partial, full, accuracy: full * 0.06, "lost \(joint)")
        }
    }

    func testSpanNeedsTwoAgreeingPairsAndRejectsASingleOutlier() throws {
        // Only index+middle confident: one pair, below the minimum.
        let onePair = hand(
            scale: 0.3,
            confidences: [.ringMCP: 0.01, .littleMCP: 0.01]
        )
        XCTAssertNil(span(of: onePair))

        let clean = try XCTUnwrap(span(of: hand(scale: 0.3)))

        func withMiddleKnuckleMoved(to location: CGPoint) -> TrackedHand {
            var points = hand(scale: 0.3).imagePoints
            points[.middleMCP] = ImageLandmark(location: location, confidence: 0.99)
            return TrackedHand(
                imagePoints: points,
                worldPoints: [:],
                handedness: .unknown,
                confidence: 0.99,
                timestamp: 0,
                palmFrame: nil
            )
        }

        // Ordinary landmark jitter: the pairs that do not involve the jittered
        // knuckle outvote the ones that do.
        let jittered = try XCTUnwrap(span(of: withMiddleKnuckleMoved(to: CGPoint(x: 0.52, y: 0.47))))
        XCTAssertEqual(jittered, clean, accuracy: clean * 0.12)

        // A grossly misplaced knuckle corrupts three of the six pairs, so no
        // majority survives. The measurement may then be refused — which is
        // safe, because a missing frame cannot trigger anything — but it must
        // never report a confidently wrong scale.
        if let gross = span(of: withMiddleKnuckleMoved(to: CGPoint(x: 0.95, y: 0.95))) {
            XCTAssertEqual(gross, clean, accuracy: clean * 0.20)
        }
    }

    // MARK: - Entry

    func testBaselineMustWarmUpBeforeProximityCanEngageSights() {
        var detector = ScopeModeDetector()
        // A hand that is already close on the very first frame has nothing to be
        // close *relative to*, so it must not scope.
        XCTAssertEqual(advance(&detector, scale: 0.5, frames: 3, from: 0), .unscoped)
        XCTAssertEqual(detector.diagnostic?.warm, false)
    }

    func testDrawingTheGunCloserEntersSights() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        XCTAssertEqual(detector.mode, .unscoped)
        XCTAssertEqual(detector.diagnostic?.warm, true)

        timestamp = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp).1
        XCTAssertEqual(detector.mode, .sights)
    }

    func testEntryRequiresTheApproachToBeHeldForTheDwell() {
        var detector = ScopeModeDetector()
        let settled = settleBaseline(&detector)
        // A single close frame is not an entry; the dwell is 0.15 s.
        _ = detector.update(
            hand: hand(scale: 0.45),
            imageSize: .init(width: 1, height: 1),
            timestamp: settled + frameInterval
        )
        XCTAssertEqual(detector.mode, .unscoped)
        XCTAssertGreaterThan(detector.diagnostic?.progress ?? 0, 0.9)
    }

    func testProgressReportsHowCloseTheHoldIsToEngaging() {
        var detector = ScopeModeDetector()
        let settled = settleBaseline(&detector)

        // Baseline is scale 0.30 and entry needs 1.40x, i.e. scale 0.42.
        // Halfway in ratio terms is 1.20x, i.e. scale 0.36.
        _ = detector.update(
            hand: hand(scale: 0.36),
            imageSize: .init(width: 1, height: 1),
            timestamp: settled + frameInterval
        )
        XCTAssertEqual(detector.diagnostic?.progress ?? 0, 0.5, accuracy: 0.05)
        XCTAssertEqual(detector.mode, .unscoped)
    }

    func testBriefMeasurementDropoutDoesNotRestartTheApproach() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)

        // Two close frames, one dropped frame inside the grace window, then
        // enough close frames to complete the dwell.
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 2, from: timestamp)
        XCTAssertEqual(detector.mode, .unscoped)
        timestamp += frameInterval
        _ = detector.update(hand: nil, imageSize: .init(width: 1, height: 1), timestamp: timestamp)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 3, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)
    }

    func testSustainedReleaseRestartsTheApproach() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)

        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 2, from: timestamp)
        // Hand pulls back for longer than the entry grace, so the dwell restarts.
        (_, timestamp) = advanceTime(&detector, scale: 0.30, frames: 8, from: timestamp)
        XCTAssertEqual(detector.mode, .unscoped)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 2, from: timestamp)
        XCTAssertEqual(detector.mode, .unscoped, "the restarted dwell should not be complete yet")
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 4, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)
    }

    func testAnIneligiblePoseCannotScopeNoMatterHowCloseItIs() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)

        // An open palm pushed at the lens: proximity alone must not be enough.
        for _ in 0..<20 {
            timestamp += frameInterval
            _ = detector.update(
                hand: hand(scale: 0.6),
                imageSize: .init(width: 1, height: 1),
                timestamp: timestamp,
                entryEligible: false
            )
        }
        XCTAssertEqual(detector.mode, .unscoped)
        XCTAssertEqual(detector.diagnostic?.progress, 0)
    }

    func testAbsurdProximitySpikeIsRejectedAsBrokenLandmarks() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)

        // 10x the baseline is not a hand near the lens, it is bad tracking.
        for _ in 0..<10 {
            timestamp += frameInterval
            _ = detector.update(
                hand: hand(scale: 3.0),
                imageSize: .init(width: 1, height: 1),
                timestamp: timestamp
            )
        }
        XCTAssertEqual(detector.mode, .unscoped)
    }

    func testThumbStateIsIrrelevantToEntry() {
        // Entry deliberately consumes no thumb signal, so pulling the gun in
        // can never be confused with a trigger pull. The detector is not even
        // given a thumb to read; the trigger keeps sole ownership of it.
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)
    }

    func testBaselineRelearnsALargerHoldWhileUnscopedInsteadOfLatching() {
        // Regression test for the failure seen on device. The old baseline froze
        // whenever the current sample was elevated, so it could only ever move
        // *down*: one low reading pinned it permanently, every later frame read
        // as "close", and sights were engaged for 81% of a 25-second session.
        //
        // The scenario that produced it is a hand held nearer than the learned
        // reference while the pose gate is failing, which is common on device.
        // The baseline must climb to that new normal.
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector, scale: 0.20)

        for _ in 0..<200 {
            timestamp += frameInterval
            _ = detector.update(
                hand: hand(scale: 0.45),
                imageSize: .init(width: 1, height: 1),
                timestamp: timestamp,
                entryEligible: false
            )
        }
        XCTAssertEqual(detector.mode, .unscoped)

        // Now the pose becomes eligible. Because the baseline re-learned the
        // nearer hold, this must NOT read as an approach.
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 10, from: timestamp)
        XCTAssertEqual(
            detector.mode, .unscoped,
            "a re-learned hold must read as normal, not as a permanent approach"
        )

        // And a genuine pull-in from that new hold must still scope.
        (_, timestamp) = advanceTime(&detector, scale: 0.68, frames: 8, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)
    }

    func testLeavingSightsDoesNotImmediatelyReEnter() {
        // The device symptom was rapid mode flipping: 9 transitions in 25 s.
        // Returning to the relaxed hold must settle, not oscillate.
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        (_, timestamp) = advanceTime(&detector, scale: 0.30, frames: 15, from: timestamp)
        XCTAssertEqual(detector.mode, .unscoped)

        var flips = 0
        var previous = detector.mode
        for _ in 0..<120 {
            timestamp += frameInterval
            let mode = detector.update(
                hand: hand(scale: 0.30),
                imageSize: .init(width: 1, height: 1),
                timestamp: timestamp
            )
            if mode != previous { flips += 1; previous = mode }
        }
        XCTAssertEqual(flips, 0, "a steady relaxed hold must not chatter between modes")
    }

    func testHoldingTheGunCloseKeepsSightsEngaged() {
        // The counterpart to the re-learning above, and the reason samples are
        // only taken while unscoped: holding the gun up must keep you scoped for
        // as long as you hold it, exactly like holding real sights to the eye.
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 300, from: timestamp)
        XCTAssertEqual(detector.mode, .sights, "sights must not time out while the gun is held up")
    }

    func testAVerySlowDriftIsNotTreatedAsAnApproach() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)

        // The baseline tracks slow changes in hold or posture on purpose, so a
        // gradual creep over several seconds is absorbed rather than scoping.
        // Sights are meant to need a deliberate movement.
        var scale = 0.30
        for _ in 0..<180 {
            timestamp += frameInterval
            scale += 0.0006
            _ = detector.update(
                hand: hand(scale: scale),
                imageSize: .init(width: 1, height: 1),
                timestamp: timestamp
            )
        }
        XCTAssertEqual(detector.mode, .unscoped)
    }

    // MARK: - Retention and exit

    func testHysteresisHoldsSightsBetweenTheEntryAndExitThresholds() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        // 1.25x is below the 1.40x entry ratio but above the 1.15x release, so
        // a hand that relaxes slightly stays scoped.
        (_, timestamp) = advanceTime(&detector, scale: 0.375, frames: 30, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)
    }

    func testLoweringTheGunLeavesSightsAfterTheExitDelay() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        (_, timestamp) = advanceTime(&detector, scale: 0.30, frames: 3, from: timestamp)
        XCTAssertEqual(detector.mode, .sights, "a brief dip should not drop out of sights")
        (_, timestamp) = advanceTime(&detector, scale: 0.30, frames: 12, from: timestamp)
        XCTAssertEqual(detector.mode, .unscoped)
    }

    func testScopedZoomDoesNotHoldSightsOpenOnItsOwn() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        // The camera is now at 1.25x, which inflates every apparent distance.
        // A hand back at its baseline distance therefore *looks* 1.25x larger.
        // Normalising by zoom is what lets the exit test still see a release.
        for _ in 0..<20 {
            timestamp += frameInterval
            _ = detector.update(
                hand: hand(scale: 0.30 * 1.25),
                imageSize: .init(width: 1, height: 1),
                timestamp: timestamp,
                zoomFactor: 1.25
            )
        }
        XCTAssertEqual(detector.mode, .unscoped)
    }

    func testMeasurementGapRetainsSightsThenReleasesAfterTheGrace() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        // Lost tracking inside the retention grace holds the mode.
        timestamp += 0.2
        _ = detector.update(hand: nil, imageSize: .init(width: 1, height: 1), timestamp: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        // Beyond the grace, plus the exit delay, it releases.
        timestamp += 0.5
        _ = detector.update(hand: nil, imageSize: .init(width: 1, height: 1), timestamp: timestamp)
        timestamp += 0.5
        _ = detector.update(hand: nil, imageSize: .init(width: 1, height: 1), timestamp: timestamp)
        XCTAssertEqual(detector.mode, .unscoped)
    }

    func testResetImmediatelyReturnsToUnscoped() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        detector.reset()
        XCTAssertEqual(detector.mode, .unscoped)
        XCTAssertNil(detector.diagnostic)
        // The baseline is cleared too, so the next session re-learns the hold.
        XCTAssertEqual(advance(&detector, scale: 0.45, frames: 3, from: 0), .unscoped)
    }

    func testDisablingDetectionForCalibrationReturnsToUnscoped() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        XCTAssertEqual(
            detector.update(
                hand: hand(scale: 0.45),
                imageSize: .init(width: 1, height: 1),
                timestamp: timestamp + frameInterval,
                enabled: false
            ),
            .unscoped
        )
    }

    func testNonMonotonicTimestampsDoNotStrandTheMode() {
        var detector = ScopeModeDetector()
        var timestamp = settleBaseline(&detector)
        (_, timestamp) = advanceTime(&detector, scale: 0.45, frames: 6, from: timestamp)
        XCTAssertEqual(detector.mode, .sights)

        // A replayed recording can step backwards; the exit dwell must restart
        // rather than latch on a negative interval.
        _ = detector.update(
            hand: hand(scale: 0.30),
            imageSize: .init(width: 1, height: 1),
            timestamp: timestamp - 5
        )
        XCTAssertEqual(detector.mode, .sights)
    }

    // MARK: - Mode policy

    func testModePolicyGatesCalibrationAndCentersScopedShots() throws {
        let observation = scopeObservation()
        XCTAssertNil(AimingModePolicy.triggerObservation(mode: .unscoped, observation: observation, hasCalibration: false))
        XCTAssertNotNil(AimingModePolicy.triggerObservation(mode: .unscoped, observation: observation, hasCalibration: true))
        XCTAssertNil(
            AimingModePolicy.triggerObservation(
                mode: .unscoped,
                observation: observation,
                hasCalibration: true,
                hasAimSolution: false
            )
        )
        XCTAssertNotNil(AimingModePolicy.triggerObservation(mode: .sights, observation: observation, hasCalibration: false))
        XCTAssertNotNil(
            AimingModePolicy.triggerObservation(
                mode: .sights,
                observation: observation,
                hasCalibration: false,
                hasAimSolution: false
            )
        )
        XCTAssertNil(AimingModePolicy.visibleAim(mode: .sights, solution: aimSolution()))
        XCTAssertEqual(AimingModePolicy.flashPoint(mode: .sights, aim: nil), CGPoint(x: 0.5, y: 0.5))

        var machine = GestureStateMachine()
        for timestamp in [0.0, 0.03, 0.06] {
            let input = AimingModePolicy.triggerObservation(
                mode: .sights,
                observation: scopeObservation(thumb: .up),
                hasCalibration: false
            )
            _ = machine.update(with: input, timestamp: timestamp)
        }
        let down = AimingModePolicy.triggerObservation(
            mode: .sights,
            observation: scopeObservation(thumb: .down),
            hasCalibration: false
        )
        XCTAssertTrue(machine.update(with: down, timestamp: 0.09).fired)
        XCTAssertFalse(machine.update(with: down, timestamp: 0.12).fired)

        let up = AimingModePolicy.triggerObservation(
            mode: .sights,
            observation: scopeObservation(thumb: .up),
            hasCalibration: false
        )
        _ = machine.update(with: up, timestamp: 0.15)
        XCTAssertEqual(machine.update(with: up, timestamp: 0.18).state, .armed)
        XCTAssertTrue(machine.update(with: down, timestamp: 0.21).fired)
    }

    func testScopedShotsResolveToCentreForGameplayTargeting() {
        // Sights publishes no directional solution, so gameplay targeting has to
        // be told the centre point explicitly or a scoped shot could never hit.
        XCTAssertEqual(AimingModePolicy.gameplayPoint(mode: .sights, aim: nil), AimingModePolicy.sightsPoint)
        XCTAssertEqual(
            AimingModePolicy.gameplayPoint(mode: .sights, aim: aimSolution()),
            AimingModePolicy.sightsPoint
        )
        XCTAssertEqual(
            AimingModePolicy.gameplayPoint(mode: .unscoped, aim: aimSolution()),
            aimSolution().gameplayScreenPoint
        )
        XCTAssertNil(AimingModePolicy.gameplayPoint(mode: .unscoped, aim: nil))
    }

    func testVersionTwoRecordingPreservesAimingModeAndLegacyFramesDecode() throws {
        let scopedFrame = LandmarkRecordingFrame(
            timestamp: 1,
            hand: nil,
            observation: nil,
            aim: nil,
            calibration: nil,
            fired: true,
            flashPoint: AimingModePolicy.sightsPoint,
            aimingMode: .sights,
            scopeProximity: ScopeProximityDiagnostic(
                span: 0.42,
                baseline: 0.3,
                ratio: 1.4,
                progress: 1,
                warm: true
            )
        )
        let recording = LandmarkRecording(
            schemaVersion: 2,
            modelVersion: AimCalibration.modelVersion,
            startedAt: Date(),
            frames: [scopedFrame]
        )
        let decoded = try LandmarkReplay.load(data: JSONEncoder().encode(recording))
        XCTAssertEqual(decoded.frames.first?.aimingMode, .sights)
        XCTAssertEqual(decoded.frames.first?.flashPoint, AimingModePolicy.sightsPoint)
        XCTAssertEqual(decoded.frames.first?.scopeProximity?.ratio, 1.4)

        let legacyJSON = """
        {
          "schemaVersion": 2,
          "modelVersion": "\(AimCalibration.modelVersion)",
          "startedAt": 0,
          "frames": [{"timestamp": 1}]
        }
        """
        let legacy = try LandmarkReplay.load(data: Data(legacyJSON.utf8))
        XCTAssertNil(legacy.frames.first?.aimingMode)
        XCTAssertNil(legacy.frames.first?.scopeProximity)
    }

    // MARK: - Helpers

    private func span(
        of hand: TrackedHand,
        imageSize: CGSize = CGSize(width: 1, height: 1),
        zoomFactor: Double = 1
    ) -> Double? {
        HandProximityMeasure.span(
            of: hand,
            imageSize: imageSize,
            zoomFactor: zoomFactor,
            minimumConfidence: GestureTuning.default.visionMinimumJointConfidence
        )
    }

    /// The four MCP knuckles, spaced in the measured nominal proportions, with
    /// index-to-little width equal to `scale`. `aspect` is the frame's
    /// width/height, used to place points so that the aspect-corrected geometry
    /// is a fixed shape regardless of frame proportions.
    private func hand(
        scale: Double,
        rotationDegrees: Double = 0,
        aspect: Double = 1,
        center: CGPoint = CGPoint(x: 0.5, y: 0.45),
        confidences: [LandmarkJoint: Float] = [:],
        omitting: Set<LandmarkJoint> = []
    ) -> TrackedHand {
        // Fractions along the knuckle line, normalised so index->little is 1.
        let layout: [(LandmarkJoint, Double)] = [
            (.indexMCP, 0.0),
            (.middleMCP, 0.3394),
            (.ringMCP, 0.6605),
            (.littleMCP, 1.0)
        ]
        let radians = rotationDegrees * .pi / 180
        var points: [LandmarkJoint: ImageLandmark] = [:]
        // A wrist is included so the fixture resembles a real hand, but nothing
        // in the measurement may depend on it.
        for (joint, fraction) in layout + [(.wrist, -0.9)] {
            guard !omitting.contains(joint) else { continue }
            let along = (fraction - 0.5) * scale
            let x = joint == .wrist ? 0 : along
            let y = joint == .wrist ? 0.6 * scale : 0
            let rotatedX = x * cos(radians) - y * sin(radians)
            let rotatedY = x * sin(radians) + y * cos(radians)
            points[joint] = ImageLandmark(
                location: CGPoint(
                    x: center.x + CGFloat(rotatedX / max(aspect, 0.000_001)),
                    y: center.y + CGFloat(rotatedY)
                ),
                confidence: confidences[joint] ?? 0.99
            )
        }
        return TrackedHand(
            imagePoints: points,
            worldPoints: [:],
            handedness: .unknown,
            confidence: 0.99,
            timestamp: 0,
            palmFrame: nil
        )
    }

    /// Feeds a steady relaxed hold until the baseline is warm, returning the
    /// last timestamp used.
    private func settleBaseline(
        _ detector: inout ScopeModeDetector,
        scale: Double = 0.30,
        frames: Int = 40
    ) -> TimeInterval {
        advanceTime(&detector, scale: scale, frames: frames, from: 0).1
    }

    private func advance(
        _ detector: inout ScopeModeDetector,
        scale: Double,
        frames: Int,
        from start: TimeInterval
    ) -> AimingMode {
        advanceTime(&detector, scale: scale, frames: frames, from: start).0
    }

    private func advanceTime(
        _ detector: inout ScopeModeDetector,
        scale: Double,
        frames: Int,
        from start: TimeInterval
    ) -> (AimingMode, TimeInterval) {
        var timestamp = start
        var mode = detector.mode
        for _ in 0..<frames {
            timestamp += frameInterval
            mode = detector.update(
                hand: hand(scale: scale),
                imageSize: CGSize(width: 1, height: 1),
                timestamp: timestamp
            )
        }
        return (mode, timestamp)
    }

    private func scopeObservation(
        muzzle: CGPoint = CGPoint(x: 0.5, y: 0.82),
        thumb: ThumbState = .up,
        variation: FingerGunVariation = .singleBarrel
    ) -> VisionFingerGunObservation {
        VisionFingerGunObservation(
            variation: variation,
            muzzlePoint: muzzle,
            aimFeature: VisionAimFeature(
                tipX: 0,
                tipY: 1,
                pipX: 0,
                pipY: 0.7,
                dipX: 0,
                dipY: 0.85,
                projectedLength: 1
            ),
            confidence: 0.99,
            poseMargin: 0.5,
            thumbState: thumb
        )
    }

    private func aimSolution() -> AimSolution {
        AimSolution(
            rawYaw: 0,
            rawPitch: 0,
            filteredYaw: 0,
            filteredPitch: 0,
            rawScreenPoint: CGPoint(x: 0.7, y: 0.5),
            screenPoint: CGPoint(x: 0.8, y: 0.5),
            confidence: 0.99,
            valid: true
        )
    }
}
