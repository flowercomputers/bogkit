import XCTest
@testable import UntitledMobileFPS

final class MultiplayerTests: XCTestCase {
    func testWarmNeutralColorsAreNotClassifiedAsPurple() {
        XCTAssertEqual(
            PerceptualColorClassifier.name(red: 0.67, green: 0.61, blue: 0.56),
            "light gray"
        )
        XCTAssertEqual(
            PerceptualColorClassifier.name(red: 0.88, green: 0.83, blue: 0.77),
            "white"
        )
        // Median torso sample measured from the reported warm-lit white shirt.
        XCTAssertEqual(
            PerceptualColorClassifier.name(red: 0.749, green: 0.681, blue: 0.610),
            "white"
        )
        XCTAssertEqual(
            PerceptualColorClassifier.name(red: 0.14, green: 0.13, blue: 0.14),
            "black"
        )
    }

    func testPurpleRemainsPurpleWhenChromaIsMeaningful() {
        XCTAssertEqual(
            PerceptualColorClassifier.name(red: 0.47, green: 0.24, blue: 0.62),
            "purple"
        )
    }

    func testDescriptionComesFromDetectedAttributes() {
        let attributes = ImageAppearanceAttributes(
            dominantColors: ["Blue", "black", "blue"],
            upperGarment: "hoodie",
            lowerGarment: "jeans",
            footwear: "white shoes"
        )

        XCTAssertEqual(
            AutomaticAppearanceDescriber.describe(attributes),
            "Person wearing hoodie, jeans, white shoes, mainly blue, black."
        )
    }

    func testOutfitBaseAnchorsScoreWithConfirmatoryNudge() {
        // Whole-body (discriminative) sets the base 0.8; a below-average silhouette
        // (confirmatory) nudges it down within the band rather than being averaged in.
        let sparse = AppearanceSignalScores(wholeBody: 0.8, outfitText: nil, silhouette: 0.4)
        XCTAssertEqual(AppearanceScoreFusion.score(sparse, scope: .activeMatch), 0.764, accuracy: 0.0001)
    }

    func testFaceAndGeometryAreExcludedFromGlobalSearch() {
        let scores = AppearanceSignalScores(wholeBody: 0.5, face: 1, bodyGeometry: 1)
        XCTAssertEqual(AppearanceScoreFusion.score(scores, scope: .globalSearch), 0.5, accuracy: 0.0001)
        XCTAssertGreaterThan(AppearanceScoreFusion.score(scores, scope: .activeMatch), 0.5)
    }

    func testConfirmatorySignalsAloneCannotClearTheGate() {
        // Perfect face + silhouette + head with no outfit visible must stay below the
        // 0.5 accept gate: face/silhouette can confirm a lock but never create one.
        let confirmatoryOnly = AppearanceSignalScores(
            headAccessory: 1,
            silhouette: 1,
            face: 1,
            bodyGeometry: 1
        )
        let fused = AppearanceScoreFusion.score(confirmatoryOnly, scope: .activeMatch)
        XCTAssertLessThan(fused, 0.5)
        XCTAssertEqual(fused, AppearanceScoreFusion.confirmatoryOnlyCap, accuracy: 0.0001)
    }

    func testPerfectFaceCannotRescueAPoorOutfitMatch() {
        // A weak outfit agreement (0.30) with perfect confirmatory signals is lifted by
        // at most the confirmatory band, so it still cannot reach the accept gate.
        let poorOutfit = AppearanceSignalScores(
            wholeBody: 0.30,
            upperBody: 0.30,
            lowerBody: 0.30,
            silhouette: 1,
            face: 1,
            bodyGeometry: 1
        )
        let fused = AppearanceScoreFusion.score(poorOutfit, scope: .activeMatch)
        XCTAssertEqual(fused, 0.30 + AppearanceScoreFusion.confirmatoryBand, accuracy: 0.0001)
        XCTAssertLessThan(fused, 0.5)
    }

    func testStrongOutfitMatchStillClearsTheGate() {
        // The common good case: a confident outfit match fires even if confirmatory
        // signals are merely neutral.
        let strong = AppearanceSignalScores(
            wholeBody: 0.82,
            upperBody: 0.80,
            lowerBody: 0.78,
            silhouette: 0.5,
            face: 0.5
        )
        XCTAssertGreaterThan(AppearanceScoreFusion.score(strong, scope: .activeMatch), 0.5)
    }

    func testShotProtocolMatchesRustTaggedEncoding() throws {
        let commandId = UUID(uuidString: "00000000-0000-0000-0000-000000000001")!
        let message = MultiplayerClientMessage.shot(
            commandId: commandId,
            matchId: "match-1",
            targetId: "target-1",
            reticle: [0.25, 0.75],
            maskContainsReticle: true,
            targetScore: 0.9,
            firedAtMs: 42
        )
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(message)) as? [String: Any])

        XCTAssertEqual(object["type"] as? String, "shot")
        XCTAssertEqual(object["matchId"] as? String, "match-1")
        XCTAssertEqual(object["maskContainsReticle"] as? Bool, true)
    }

    func testMultiplayerUsesContinuousAimInsteadOfLaggingZone() {
        let solution = AimSolution(
            rawYaw: 0.25,
            rawPitch: 0.03,
            filteredYaw: 0.3,
            filteredPitch: -0.3,
            rawScreenPoint: CGPoint(x: 0.75, y: 0.53),
            screenPoint: CGPoint(x: 0.8, y: 0.2),
            confidence: 0.9,
            valid: true
        )

        XCTAssertEqual(solution.gameplayScreenPoint, CGPoint(x: 0.75, y: 0.53))
        XCTAssertNotEqual(solution.gameplayScreenPoint, solution.screenPoint)
    }

    func testPersonTargetSelectorRejectsLargeForegroundHand() {
        let person = CGRect(x: 0.55, y: 0.47, width: 0.11, height: 0.18)
        let foregroundHand = CGRect(x: 0, y: 0.03, width: 0.57, height: 0.67)
        var selector = PersonTargetSelector()

        XCTAssertNil(selector.select(
            candidates: [candidate(foregroundHand), candidate(person)],
            faceBoxes: []
        ))
        assertRect(selector.select(
            candidates: [candidate(foregroundHand), candidate(person)],
            faceBoxes: []
        )?.box, equals: person)
    }

    func testPersonTargetSelectorKeepsTrackedPersonWhenHandAppears() {
        let firstPerson = CGRect(x: 0.54, y: 0.47, width: 0.11, height: 0.18)
        let movedPerson = CGRect(x: 0.57, y: 0.48, width: 0.11, height: 0.18)
        let foregroundHand = CGRect(x: 0, y: 0.03, width: 0.57, height: 0.67)
        var selector = PersonTargetSelector()

        XCTAssertNil(selector.select(candidates: [candidate(firstPerson)], faceBoxes: []))
        assertRect(
            selector.select(candidates: [candidate(firstPerson)], faceBoxes: [])?.box,
            equals: firstPerson
        )
        assertRect(selector.select(
            candidates: [candidate(foregroundHand), candidate(movedPerson)],
            faceBoxes: []
        )?.box, equals: movedPerson)
    }

    func testPersonTargetSelectorAcquiresIdentityBeforeGeometricFavorite() {
        let opponent = CGRect(x: 0.12, y: 0.18, width: 0.28, height: 0.68)
        let slenderBystander = CGRect(x: 0.70, y: 0.35, width: 0.10, height: 0.35)
        let faces = [
            CGRect(x: 0.20, y: 0.72, width: 0.08, height: 0.08),
            CGRect(x: 0.72, y: 0.62, width: 0.05, height: 0.05)
        ]
        let candidates = [
            candidate(opponent, score: 0.81),
            candidate(slenderBystander, score: 0.64)
        ]
        var selector = PersonTargetSelector()

        XCTAssertNil(selector.select(candidates: candidates, faceBoxes: faces))
        assertRect(
            selector.select(candidates: candidates, faceBoxes: faces)?.box,
            equals: opponent
        )
    }

    func testPersonTargetSelectorDoesNotAcquireAmbiguousIdentityTie() {
        let left = CGRect(x: 0.10, y: 0.20, width: 0.25, height: 0.65)
        let right = CGRect(x: 0.62, y: 0.20, width: 0.25, height: 0.65)
        var selector = PersonTargetSelector()
        let candidates = [
            candidate(left, score: 0.78),
            candidate(right, score: 0.76)
        ]

        XCTAssertNil(selector.select(candidates: candidates, faceBoxes: []))
        XCTAssertNil(selector.select(candidates: candidates, faceBoxes: []))
        XCTAssertNil(selector.selectedBox)
    }

    func testPersonTargetSelectorRequiresPersistentIdentityAdvantageToSwitch() {
        let original = CGRect(x: 0.08, y: 0.18, width: 0.28, height: 0.68)
        let movedOriginal = CGRect(x: 0.10, y: 0.18, width: 0.28, height: 0.68)
        // Close enough to satisfy the geometric continuity radius, as happens
        // when two people stand beside each other. Identity hysteresis must
        // still prevent an immediate handoff.
        let challenger = CGRect(x: 0.37, y: 0.20, width: 0.24, height: 0.64)
        var selector = PersonTargetSelector()

        XCTAssertNil(selector.select(
            candidates: [candidate(original, score: 0.72)],
            faceBoxes: []
        ))
        _ = selector.select(candidates: [candidate(original, score: 0.72)], faceBoxes: [])

        for _ in 0..<2 {
            assertRect(selector.select(
                candidates: [
                    candidate(movedOriginal, score: 0.72),
                    candidate(challenger, score: 0.88)
                ],
                faceBoxes: []
            )?.box, equals: movedOriginal)
        }
        assertRect(selector.select(
            candidates: [
                candidate(movedOriginal, score: 0.72),
                candidate(challenger, score: 0.88)
            ],
            faceBoxes: []
        )?.box, equals: challenger)
    }

    func testShotWireUsesFrozenContinuousGameplayPoint() throws {
        let targeting = GameplayTargetingState(
            gameplayPoint: CGPoint(x: 0.75, y: 0.53),
            zonePoint: CGPoint(x: 0.8, y: 0.2),
            targetBoundingBox: CGRect(x: 0.3, y: 0.3, width: 0.3, height: 0.5),
            targetAgeSeconds: 0.1,
            targetScore: 0.72,
            maskCoverage: 0.4,
            maskContainsReticle: true,
            status: .ready
        )
        let event = GameplayShotEvent(id: 1, targeting: targeting)
        let message = MultiplayerClientMessage.shot(
            commandId: UUID(),
            matchId: "match",
            targetId: "target",
            reticle: [
                Float(event.targeting.gameplayPoint.x),
                Float(event.targeting.gameplayPoint.y)
            ],
            maskContainsReticle: event.targeting.maskContainsReticle,
            targetScore: event.targeting.targetScore,
            firedAtMs: 1
        )
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(message)) as? [String: Any]
        )
        let reticle = try XCTUnwrap(object["reticle"] as? [NSNumber])

        XCTAssertEqual(reticle[0].doubleValue, 0.75, accuracy: 0.0001)
        XCTAssertEqual(reticle[1].doubleValue, 0.53, accuracy: 0.0001)
        XCTAssertEqual(object["maskContainsReticle"] as? Bool, true)
    }

    func testCollisionMaskUsesVisionLowerLeftCoordinatesAndBoundingBoxClip() {
        let width = 100
        let height = 100
        let pixels = Array(repeating: UInt8.max, count: width * height)
        let mask = PersonCollisionMask(
            width: width,
            height: height,
            pixels: pixels,
            clippingTo: CGRect(x: 0.25, y: 0.5, width: 0.5, height: 0.4)
        )

        XCTAssertGreaterThan(mask.reticleCoverage(at: CGPoint(x: 0.5, y: 0.75)), 0.9)
        XCTAssertEqual(mask.reticleCoverage(at: CGPoint(x: 0.5, y: 0.25)), 0)
        XCTAssertEqual(mask.reticleCoverage(at: CGPoint(x: 0.1, y: 0.75)), 0)
    }

    func testLocalPersonMaskCompositesIntoTargetCameraCoordinates() throws {
        let localMask = PersonCollisionMask(
            width: 10,
            height: 20,
            pixels: Array(repeating: .max, count: 10 * 20)
        )
        let targetBox = CGRect(x: 0.5, y: 0.3, width: 0.2, height: 0.5)
        let composited = try XCTUnwrap(
            localMask.composited(
                in: CGSize(width: 100, height: 200),
                targetBox: targetBox,
                maximumDimension: 200
            )
        )

        XCTAssertGreaterThan(
            composited.reticleCoverage(at: CGPoint(x: 0.6, y: 0.55)),
            0.9
        )
        XCTAssertEqual(composited.reticleCoverage(at: CGPoint(x: 0.4, y: 0.55)), 0)
        XCTAssertEqual(composited.reticleCoverage(at: CGPoint(x: 0.6, y: 0.2)), 0)
    }

    func testReticleFootprintHitsNarrowSilhouetteThatFailsCoarseCellAverage() {
        let width = 128
        let height = 128
        var pixels = Array(repeating: UInt8.zero, count: width * height)
        for row in 0..<height {
            pixels[row * width + 63] = .max
            pixels[row * width + 64] = .max
        }
        let mask = PersonCollisionMask(width: width, height: height, pixels: pixels)
        let point = CGPoint(x: 0.5, y: 0.5)
        let oldCellAverage = mask.occupancyDescriptor()[4 * 8 + 4]

        XCTAssertLessThan(oldCellAverage, 0.18)
        XCTAssertGreaterThanOrEqual(
            mask.reticleCoverage(at: point),
            GameplayTargetingTuning.default.minimumReticleCoverage
        )
    }

    func testCollisionMaskHonorsForegroundThreshold() {
        let below = PersonCollisionMask(
            width: 32,
            height: 32,
            pixels: Array(repeating: 89, count: 32 * 32)
        )
        let atThreshold = PersonCollisionMask(
            width: 32,
            height: 32,
            pixels: Array(repeating: 90, count: 32 * 32)
        )

        XCTAssertEqual(below.reticleCoverage(at: CGPoint(x: 0.5, y: 0.5)), 0)
        XCTAssertEqual(atThreshold.reticleCoverage(at: CGPoint(x: 0.5, y: 0.5)), 1)
    }

    func testTargetEvaluationRejectsStaleMaskAndAcceptsReticleOverlap() {
        let mask = PersonCollisionMask(
            width: 64,
            height: 64,
            pixels: Array(repeating: .max, count: 64 * 64),
            clippingTo: CGRect(x: 0.2, y: 0.2, width: 0.6, height: 0.6)
        )
        let ready = GameplayTargetEvaluator.evaluate(
            gameplayPoint: CGPoint(x: 0.5, y: 0.5),
            zonePoint: CGPoint(x: 0.8, y: 0.2),
            targetBoundingBox: CGRect(x: 0.2, y: 0.2, width: 0.6, height: 0.6),
            collisionMask: mask,
            targetScore: 0.72,
            targetTimestamp: 10,
            frameTimestamp: 10.5
        )
        let stale = GameplayTargetEvaluator.evaluate(
            gameplayPoint: CGPoint(x: 0.5, y: 0.5),
            zonePoint: CGPoint(x: 0.8, y: 0.2),
            targetBoundingBox: CGRect(x: 0.2, y: 0.2, width: 0.6, height: 0.6),
            collisionMask: mask,
            targetScore: 0.72,
            targetTimestamp: 10,
            frameTimestamp: 11
        )

        XCTAssertEqual(ready.status, .ready)
        XCTAssertTrue(ready.maskContainsReticle)
        XCTAssertEqual(stale.status, .stale)
        XCTAssertFalse(stale.maskContainsReticle)
    }

    func testMaskImageBytesUseMaskLuminanceAsAlpha() {
        let mask = PersonCollisionMask(
            width: 2,
            height: 1,
            pixels: [0, 128]
        )
        let rgba = mask.premultipliedWhiteRGBA()

        XCTAssertEqual(Array(rgba[0..<4]), [0, 0, 0, 0])
        XCTAssertEqual(Array(rgba[4..<8]), [128, 128, 128, 128])
    }

    func testShotFeedbackSurvivesSameActiveMatchSnapshot() {
        let active = MultiplayerMatchSnapshot(
            protocolVersion: multiplayerProtocolVersion,
            revision: 1,
            matchId: "match",
            inviteCode: "CODE",
            status: .active,
            players: [],
            winner: nil,
            updatedAtMs: 1
        )
        let updated = MultiplayerMatchSnapshot(
            protocolVersion: multiplayerProtocolVersion,
            revision: 2,
            matchId: "match",
            inviteCode: "CODE",
            status: .active,
            players: [],
            winner: nil,
            updatedAtMs: 2
        )

        XCTAssertFalse(MultiplayerShotFeedback.shouldClear(previous: active, next: updated))
        XCTAssertEqual(
            MultiplayerShotFeedback.message(accepted: false, reason: "reticle_outside_target"),
            "MISS · AIM OUTSIDE TARGET"
        )
        XCTAssertEqual(
            MultiplayerShotFeedback.message(accepted: false, reason: "target_lock_too_weak"),
            "MISS · IDENTITY TOO WEAK"
        )
        XCTAssertEqual(
            MultiplayerShotFeedback.message(accepted: false, reason: "missing_reciprocal_proximity"),
            "MISS · PROXIMITY NOT READY"
        )
    }

    func testDecodesServerSnapshot() throws {
        let json = #"{"type":"match_snapshot","snapshot":{"protocolVersion":1,"revision":4,"matchId":"m1","inviteCode":"CODE","status":"active","players":[{"playerId":"a","health":3,"ready":true,"eliminated":false},{"playerId":"b","health":2,"ready":true,"eliminated":false}],"winner":null,"updatedAtMs":8}}"#.data(using: .utf8)!
        let message = try JSONDecoder().decode(MultiplayerServerMessage.self, from: json)

        guard case .matchSnapshot(let snapshot) = message else {
            return XCTFail("Expected a match snapshot")
        }
        XCTAssertEqual(snapshot.player("b")?.health, 2)
    }

    func testEmbeddingNormalizationPadsToBackendDimension() {
        let normalized = EmbeddingMath.normalized([3, 4])
        XCTAssertEqual(normalized.count, appearanceEmbeddingDimensions)
        XCTAssertEqual(normalized[0], 0.6, accuracy: 0.0001)
        XCTAssertEqual(normalized[1], 0.8, accuracy: 0.0001)
    }

    private func assertRect(
        _ actual: CGRect?,
        equals expected: CGRect,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        guard let actual else {
            return XCTFail("Expected a rectangle", file: file, line: line)
        }
        XCTAssertEqual(actual.minX, expected.minX, accuracy: 0.0001, file: file, line: line)
        XCTAssertEqual(actual.minY, expected.minY, accuracy: 0.0001, file: file, line: line)
        XCTAssertEqual(actual.width, expected.width, accuracy: 0.0001, file: file, line: line)
        XCTAssertEqual(actual.height, expected.height, accuracy: 0.0001, file: file, line: line)
    }
}

private func candidate(
    _ box: CGRect,
    score: Float = 0.9
) -> PersonTargetCandidate {
    PersonTargetCandidate(box: box, identityScore: score)
}
