import XCTest
@testable import UntitledMobileFPS

final class GestureStateMachineTests: XCTestCase {
    func testArmsAndFiresExactlyOnceWhileHeld() {
        var machine = GestureStateMachine()
        XCTAssertEqual(update(&machine, .fixture(thumb: .up), at: 0).state, .candidate)
        XCTAssertEqual(update(&machine, .fixture(thumb: .up), at: 0.03).state, .candidate)
        XCTAssertEqual(update(&machine, .fixture(thumb: .up), at: 0.06).state, .armed)
        XCTAssertTrue(update(&machine, .fixture(thumb: .down), at: 0.09).fired)
        XCTAssertFalse(update(&machine, .fixture(thumb: .down), at: 0.12).fired)
        XCTAssertFalse(update(&machine, .fixture(thumb: .down), at: 0.15).fired)
    }

    func testRequiresThumbUpBeforeSecondShot() {
        var machine = armedMachine()
        XCTAssertTrue(update(&machine, .fixture(thumb: .down), at: 0.10).fired)
        _ = update(&machine, .fixture(thumb: .down), at: 0.13)
        _ = update(&machine, .fixture(thumb: .up), at: 0.16)
        XCTAssertEqual(update(&machine, .fixture(thumb: .up), at: 0.19).state, .armed)
        XCTAssertTrue(update(&machine, .fixture(thumb: .down), at: 0.22).fired)
    }

    func testVariationSwitchDoesNotFire() {
        var machine = armedMachine()
        let result = update(&machine, .fixture(variation: .doubleBarrel, thumb: .up), at: 0.10)
        XCTAssertEqual(result.state, .armed)
        XCTAssertFalse(result.fired)
    }

    func testTimeBasedTrackingLossGraceAndReset() {
        var machine = armedMachine()
        XCTAssertEqual(update(&machine, nil, at: 0.15).state, .armed)
        XCTAssertEqual(update(&machine, nil, at: 0.27).state, .notDetected)
    }

    func testArmedPoseLatchesImmediateThumbDownAcrossFingerLabelLoss() {
        var machine = armedMachine()

        let result = machine.update(
            with: Optional<FingerGunObservation>.none,
            fallbackThumbState: .down,
            timestamp: 0.09
        )

        XCTAssertEqual(result.state, .fired)
        XCTAssertTrue(result.fired)
    }

    func testPoseLatchCannotArmOrFireAfterItExpires() {
        var tuning = GestureTuning.default
        tuning.armedPoseLatchSeconds = 0.05
        var machine = GestureStateMachine(tuning: tuning)

        XCTAssertFalse(
            machine.update(
                with: Optional<FingerGunObservation>.none,
                fallbackThumbState: .down,
                timestamp: 0
            ).fired
        )
        _ = update(&machine, .fixture(thumb: .up), at: 0.01)
        _ = update(&machine, .fixture(thumb: .up), at: 0.04)
        _ = update(&machine, .fixture(thumb: .up), at: 0.07)
        let expired = machine.update(
            with: Optional<FingerGunObservation>.none,
            fallbackThumbState: .down,
            timestamp: 0.13
        )

        XCTAssertEqual(expired.state, .armed)
        XCTAssertFalse(expired.fired)
    }

    func testFallbackThumbUpCannotRearm() {
        var machine = armedMachine()
        XCTAssertTrue(update(&machine, .fixture(thumb: .down), at: 0.09).fired)
        _ = update(&machine, .fixture(thumb: .down), at: 0.12)

        let fallback = machine.update(
            with: Optional<FingerGunObservation>.none,
            fallbackThumbState: .up,
            timestamp: 0.15
        )

        XCTAssertEqual(fallback.state, .waitingForRearm)
        XCTAssertFalse(fallback.fired)
    }

    private func armedMachine() -> GestureStateMachine {
        var machine = GestureStateMachine()
        _ = update(&machine, .fixture(thumb: .up), at: 0)
        _ = update(&machine, .fixture(thumb: .up), at: 0.03)
        _ = update(&machine, .fixture(thumb: .up), at: 0.06)
        return machine
    }

    private func update(
        _ machine: inout GestureStateMachine,
        _ observation: FingerGunObservation?,
        at timestamp: TimeInterval
    ) -> GestureUpdate {
        machine.update(with: observation, timestamp: timestamp)
    }
}

private extension FingerGunObservation {
    static func fixture(
        variation: FingerGunVariation = .singleBarrel,
        thumb: ThumbState
    ) -> FingerGunObservation {
        FingerGunObservation(
            variation: variation,
            muzzlePoint: CGPoint(x: 0.5, y: 0.5),
            barrelDirection: CameraSpaceVector(x: 0, y: 0, z: 1),
            confidence: 0.99,
            poseMargin: 0.5,
            thumbState: thumb,
            handedness: .right
        )
    }
}
