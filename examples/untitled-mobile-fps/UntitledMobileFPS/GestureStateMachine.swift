import Foundation

struct GestureStateMachine: Sendable {
    private(set) var state: GestureState = .notDetected
    private var stableFrames = 0
    private var rearmFrames = 0
    private var lastSeenTimestamp: TimeInterval?
    private let tuning: GestureTuning

    init(tuning: GestureTuning = .default) { self.tuning = tuning }

    mutating func update(
        with observation: FingerGunObservation?,
        fallbackThumbState: ThumbState? = nil,
        timestamp: TimeInterval
    ) -> GestureUpdate {
        update(
            thumbState: observation?.thumbState,
            fallbackThumbState: fallbackThumbState,
            timestamp: timestamp
        )
    }

    mutating func update(
        with observation: VisionFingerGunObservation?,
        fallbackThumbState: ThumbState? = nil,
        timestamp: TimeInterval
    ) -> GestureUpdate {
        update(
            thumbState: observation?.thumbState,
            fallbackThumbState: fallbackThumbState,
            timestamp: timestamp
        )
    }

    private mutating func update(
        thumbState: ThumbState?,
        fallbackThumbState: ThumbState?,
        timestamp: TimeInterval
    ) -> GestureUpdate {
        let resolvedThumbState: ThumbState?
        if let thumbState {
            lastSeenTimestamp = timestamp
            resolvedThumbState = thumbState
        } else if state == .armed,
                  fallbackThumbState == .down,
                  let lastSeenTimestamp,
                  timestamp - lastSeenTimestamp <= tuning.armedPoseLatchSeconds {
            // The pose was already proven while arming. Permit only the
            // immediate thumb-down edge; a fallback can never arm or rearm.
            resolvedThumbState = .down
        } else {
            resolvedThumbState = nil
        }

        guard let thumbState = resolvedThumbState else {
            if let lastSeenTimestamp, timestamp - lastSeenTimestamp >= tuning.trackingResetSeconds { reset() }
            return GestureUpdate(state: state, fired: false)
        }

        switch state {
        case .notDetected:
            if thumbState == .up {
                stableFrames = 1
                state = .candidate
            }
        case .candidate:
            if thumbState == .up {
                stableFrames += 1
                if stableFrames >= tuning.stabilizationFrames { state = .armed }
            } else {
                stableFrames = 0
                state = .notDetected
            }
        case .armed:
            if thumbState == .down {
                state = .fired
                return GestureUpdate(state: state, fired: true)
            }
        case .fired:
            state = .waitingForRearm
            rearmFrames = thumbState == .up ? 1 : 0
        case .waitingForRearm:
            if thumbState == .up {
                rearmFrames += 1
                if rearmFrames >= tuning.rearmFrames {
                    state = .armed
                    rearmFrames = 0
                }
            } else if thumbState == .down {
                rearmFrames = 0
            }
        }
        return GestureUpdate(state: state, fired: false)
    }

    mutating func reset() {
        state = .notDetected
        stableFrames = 0
        rearmFrames = 0
        lastSeenTimestamp = nil
    }
}
