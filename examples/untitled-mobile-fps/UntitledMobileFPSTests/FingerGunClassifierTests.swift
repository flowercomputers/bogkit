import XCTest
@testable import UntitledMobileFPS

final class FingerGunClassifierTests: XCTestCase {
    private let classifier = FingerGunClassifier()

    func testSceneDirectedSingleBarrel() throws {
        let observation = try XCTUnwrap(classifier.classify(.fixture(middleStraight: false, thumbUp: true)))
        XCTAssertEqual(observation.variation, .singleBarrel)
        XCTAssertEqual(observation.thumbState, .up)
        XCTAssertGreaterThan(observation.barrelDirection.z, 0.9)
    }

    func testSceneDirectedDoubleBarrelUsesBisector() throws {
        var hand = TrackedHand.fixture(middleStraight: true, thumbUp: true)
        hand.worldPoints[.indexTip] = WorldLandmark(location: CameraSpaceVector(x: 0.04, y: 0.06, z: 0.09), confidence: 0.99)
        hand.worldPoints[.middleTip] = WorldLandmark(location: CameraSpaceVector(x: 0.00, y: 0.06, z: 0.09), confidence: 0.99)
        hand = hand.rebuildingPalmFrame()
        let observation = try XCTUnwrap(classifier.classify(hand))
        XCTAssertEqual(observation.variation, .doubleBarrel)
        XCTAssertGreaterThan(observation.barrelDirection.z, 0.9)
        XCTAssertEqual(observation.muzzlePoint.x, 0.5, accuracy: 0.001)
    }

    func testLeftHandCanonicalization() throws {
        let observation = try XCTUnwrap(classifier.classify(TrackedHand.fixture(middleStraight: false, thumbUp: true).mirrored()))
        XCTAssertEqual(observation.handedness, .left)
        XCTAssertEqual(observation.thumbState, .up)
    }

    func testThumbDownStillClassifies() throws {
        let observation = try XCTUnwrap(classifier.classify(.fixture(middleStraight: false, thumbUp: false)))
        XCTAssertEqual(observation.thumbState, .down)
    }

    func testCameraDirectedFingerIsRejected() {
        var hand = TrackedHand.fixture(middleStraight: false, thumbUp: true)
        for joint in [LandmarkJoint.indexPIP, .indexDIP, .indexTip] {
            let old = hand.worldPoints[joint]!
            hand.worldPoints[joint] = WorldLandmark(
                location: CameraSpaceVector(x: old.location.x, y: old.location.y, z: -abs(old.location.z)),
                confidence: old.confidence
            )
        }
        XCTAssertNil(classifier.classify(hand.rebuildingPalmFrame()))
    }

    func testOpenPalmIsRejected() {
        var hand = TrackedHand.fixture(middleStraight: true, thumbUp: true)
        hand.setStraight(.ring, x: -0.01)
        XCTAssertNil(classifier.classify(hand.rebuildingPalmFrame()))
    }

    func testCalibrationRayDoesNotRequireAcceptedFingerGunPose() throws {
        var hand = TrackedHand.fixture(middleStraight: true, thumbUp: true)
        hand.setStraight(.ring, x: -0.01)
        hand.setStraight(.little, x: -0.03)
        let analysis = classifier.analyze(hand.rebuildingPalmFrame())
        XCTAssertNil(analysis.observation)
        XCTAssertNotNil(analysis.rejectionReason)
        XCTAssertGreaterThan(try XCTUnwrap(analysis.calibrationSample).direction.z, 0.9)
    }

    func testAmbiguousMiddleFingerIsRejected() {
        var hand = TrackedHand.fixture(middleStraight: false, thumbUp: true)
        hand.worldPoints[.middlePIP] = world(0.01, 0.06, 0.025)
        hand.worldPoints[.middleDIP] = world(0.025, 0.075, 0.045)
        hand.worldPoints[.middleTip] = world(0.035, 0.08, 0.06)
        XCTAssertNil(classifier.classify(hand.rebuildingPalmFrame()))
    }

    func testMissingAndLowConfidenceWorldJointsAreRejected() {
        var missing = TrackedHand.fixture(middleStraight: false, thumbUp: true)
        missing.worldPoints.removeValue(forKey: .littleTip)
        XCTAssertNil(classifier.classify(missing))
        var low = TrackedHand.fixture(middleStraight: false, thumbUp: true)
        low.worldPoints[.indexTip] = WorldLandmark(location: low.worldPoints[.indexTip]!.location, confidence: 0.1)
        XCTAssertNil(classifier.classify(low))
    }
}

final class VisionFingerGunClassifierTests: XCTestCase {
    private let classifier = VisionFingerGunClassifier()

    func testSingleAndDoubleBarrelImagePoses() throws {
        let single = try XCTUnwrap(classifier.analyze(.visionFixture(middleStraight: false)).observation)
        let double = try XCTUnwrap(classifier.analyze(.visionFixture(middleStraight: true)).observation)
        XCTAssertEqual(single.variation, .singleBarrel)
        XCTAssertEqual(double.variation, .doubleBarrel)
        XCTAssertEqual(single.thumbState, .up)
    }

    func testOpenPalmCannotBecomeFiringObservation() {
        var hand = TrackedHand.visionFixture(middleStraight: false)
        hand.setVisionStraight([.ringMCP, .ringPIP, .ringDIP, .ringTip], x: 0.58)
        XCTAssertNil(classifier.analyze(hand).observation)
    }

    func testCalibrationFeatureSurvivesStrictPoseRejection() {
        var hand = TrackedHand.visionFixture(middleStraight: false)
        hand.setVisionStraight([.ringMCP, .ringPIP, .ringDIP, .ringTip], x: 0.58)
        let analysis = classifier.analyze(hand)
        XCTAssertNil(analysis.observation)
        XCTAssertNotNil(analysis.aimFeature)
        XCTAssertEqual(analysis.calibrationVariation, .singleBarrel)
    }

    func testSeparatedThumbCanRearmWhenVisionReportsModerateBend() {
        var hand = TrackedHand.visionFixture(middleStraight: false)
        // A scene-directed thumb is commonly foreshortened into a roughly
        // 112-degree distal angle even though its tip remains clearly away
        // from the index base. This is still the raised/rearm position.
        hand.imagePoints[.thumbTip] = image(0.18, 0.42)

        XCTAssertEqual(classifier.analyze(hand).thumbState, .up)
    }

    func testScopeFallbackAcceptsForeshortenedIndexAndIsEligibleForSights() throws {
        var hand = TrackedHand.visionFixture(middleStraight: false)
        hand.imagePoints[.indexDIP] = image(0.52, 0.49)
        hand.imagePoints[.indexTip] = image(0.50, 0.56)
        let analysis = classifier.analyze(hand)

        // The strict classifier rejects this pose because an index aimed along
        // the camera axis foreshortens into a CURLED label, but it is exactly
        // the pose a player scopes from, so it must stay sights-eligible.
        XCTAssertNil(analysis.observation)
        XCTAssertEqual(analysis.indexState, .curled)
        XCTAssertNotNil(ScopePosePolicy.observation(from: analysis, hand: hand))
        XCTAssertTrue(ScopePosePolicy.isEntryEligible(analysis, hand: hand))
    }

    func testScopeFallbackStillRejectsOpenPalm() {
        var hand = TrackedHand.visionFixture(middleStraight: false)
        hand.imagePoints[.indexDIP] = image(0.52, 0.49)
        hand.imagePoints[.indexTip] = image(0.50, 0.56)
        hand.setVisionStraight([.ringMCP, .ringPIP, .ringDIP, .ringTip], x: 0.58)
        let analysis = classifier.analyze(hand)

        XCTAssertNil(analysis.observation)
        XCTAssertNil(ScopePosePolicy.observation(from: analysis, hand: hand))
        XCTAssertFalse(ScopePosePolicy.isEntryEligible(analysis, hand: hand))
    }
}

private enum TestFinger { case index, middle, ring, little }

private extension TrackedHand {
    static func fixture(middleStraight: Bool, thumbUp: Bool) -> TrackedHand {
        let worldPoints: [LandmarkJoint: WorldLandmark] = [
            .wrist: world(0, 0, 0),
            .thumbCMC: world(0.035, 0.025, 0),
            .thumbMP: thumbUp ? world(0.055, 0.04, 0.005) : world(0.040, 0.040, 0.005),
            .thumbIP: thumbUp ? world(0.075, 0.055, 0.010) : world(0.025, 0.055, 0.010),
            .thumbTip: thumbUp ? world(0.105, 0.075, 0.015) : world(0.030, 0.050, 0.010)
        ]
        var hand = TrackedHand(
            imagePoints: Dictionary(uniqueKeysWithValues: LandmarkJoint.allCases.enumerated().map {
                ($0.element, ImageLandmark(location: CGPoint(x: 0.4 + Double($0.offset % 4) * 0.03, y: 0.42 + Double($0.offset / 4) * 0.02), confidence: 0.99))
            }),
            worldPoints: worldPoints,
            handedness: .right,
            confidence: 0.99,
            timestamp: 1,
            palmFrame: nil
        )
        hand.setStraight(.index, x: 0.03)
        if middleStraight { hand.setStraight(.middle, x: 0.01) } else { hand.setCurled(.middle, x: 0.01) }
        hand.setCurled(.ring, x: -0.01)
        hand.setCurled(.little, x: -0.03)
        hand.imagePoints[.indexTip] = ImageLandmark(location: CGPoint(x: 0.48, y: 0.52), confidence: 0.99)
        hand.imagePoints[.middleTip] = ImageLandmark(location: CGPoint(x: 0.52, y: 0.52), confidence: 0.99)
        return hand.rebuildingPalmFrame()
    }

    mutating func setStraight(_ finger: TestFinger, x: Double) {
        let joints = joints(finger)
        worldPoints[joints[0]] = world(x, 0.05, 0)
        worldPoints[joints[1]] = world(x, 0.054, 0.030)
        worldPoints[joints[2]] = world(x, 0.057, 0.060)
        worldPoints[joints[3]] = world(x, 0.060, 0.090)
    }

    mutating func setCurled(_ finger: TestFinger, x: Double) {
        let joints = joints(finger)
        worldPoints[joints[0]] = world(x, 0.05, 0)
        worldPoints[joints[1]] = world(x, 0.075, 0.008)
        worldPoints[joints[2]] = world(x + 0.012, 0.060, 0.012)
        worldPoints[joints[3]] = world(x, 0.045, 0.008)
    }

    func rebuildingPalmFrame() -> TrackedHand {
        TrackedHand(
            imagePoints: imagePoints,
            worldPoints: worldPoints,
            handedness: handedness,
            confidence: confidence,
            timestamp: timestamp,
            palmFrame: PalmCoordinateFrame.make(points: worldPoints, handedness: handedness)
        )
    }

    func mirrored() -> TrackedHand {
        let mirroredWorld = worldPoints.mapValues {
            WorldLandmark(location: CameraSpaceVector(x: -$0.location.x, y: $0.location.y, z: $0.location.z), confidence: $0.confidence)
        }
        let mirroredImage = imagePoints.mapValues {
            ImageLandmark(location: CGPoint(x: 1 - $0.location.x, y: $0.location.y), confidence: $0.confidence)
        }
        return TrackedHand(
            imagePoints: mirroredImage,
            worldPoints: mirroredWorld,
            handedness: .left,
            confidence: confidence,
            timestamp: timestamp,
            palmFrame: PalmCoordinateFrame.make(points: mirroredWorld, handedness: .left)
        )
    }

    static func visionFixture(middleStraight: Bool) -> TrackedHand {
        var points: [LandmarkJoint: ImageLandmark] = [
            .wrist: image(0.50, 0.18),
            .thumbCMC: image(0.44, 0.34), .thumbMP: image(0.36, 0.40),
            .thumbIP: image(0.28, 0.47), .thumbTip: image(0.19, 0.55),
            .indexMCP: image(0.46, 0.39), .indexPIP: image(0.46, 0.54),
            .indexDIP: image(0.46, 0.68), .indexTip: image(0.46, 0.82),
            .ringMCP: image(0.58, 0.38), .ringPIP: image(0.61, 0.48),
            .ringDIP: image(0.64, 0.42), .ringTip: image(0.60, 0.35),
            .littleMCP: image(0.64, 0.35), .littlePIP: image(0.67, 0.44),
            .littleDIP: image(0.69, 0.38), .littleTip: image(0.65, 0.31)
        ]
        if middleStraight {
            points[.middleMCP] = image(0.52, 0.40); points[.middlePIP] = image(0.52, 0.55)
            points[.middleDIP] = image(0.52, 0.69); points[.middleTip] = image(0.52, 0.82)
        } else {
            points[.middleMCP] = image(0.52, 0.40); points[.middlePIP] = image(0.55, 0.51)
            points[.middleDIP] = image(0.59, 0.46); points[.middleTip] = image(0.55, 0.39)
        }
        return TrackedHand(
            imagePoints: points,
            worldPoints: [:],
            handedness: .right,
            confidence: 0.99,
            timestamp: 1,
            palmFrame: nil
        )
    }

    mutating func setVisionStraight(_ joints: [LandmarkJoint], x: Double) {
        for (index, joint) in joints.enumerated() {
            imagePoints[joint] = image(x, 0.38 + Double(index) * 0.14)
        }
    }
}

private func joints(_ finger: TestFinger) -> [LandmarkJoint] {
    switch finger {
    case .index: return [.indexMCP, .indexPIP, .indexDIP, .indexTip]
    case .middle: return [.middleMCP, .middlePIP, .middleDIP, .middleTip]
    case .ring: return [.ringMCP, .ringPIP, .ringDIP, .ringTip]
    case .little: return [.littleMCP, .littlePIP, .littleDIP, .littleTip]
    }
}

private func world(_ x: Double, _ y: Double, _ z: Double) -> WorldLandmark {
    WorldLandmark(location: CameraSpaceVector(x: x, y: y, z: z), confidence: 0.99)
}

private func image(_ x: Double, _ y: Double) -> ImageLandmark {
    ImageLandmark(location: CGPoint(x: x, y: y), confidence: 0.99)
}
