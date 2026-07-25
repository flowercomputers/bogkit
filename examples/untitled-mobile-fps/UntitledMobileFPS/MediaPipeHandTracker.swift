import Foundation
import MediaPipeTasksVision
import UIKit

final class MediaPipeHandTracker: NSObject, HandTracking, @unchecked Sendable {
    let name = "MEDIAPIPE"

    private struct Pending {
        let completion: @Sendable (Result<HandTrackingResult, Error>) -> Void
        let imageSize: CGSize
        let timestamp: TimeInterval
    }

    private var landmarker: HandLandmarker!
    private let lock = NSLock()
    private var pending: [Int: Pending] = [:]
    private var lastTimestamp = -1

    init(modelPath: String, tuning: GestureTuning = .default) throws {
        let options = HandLandmarkerOptions()
        options.baseOptions.modelAssetPath = modelPath
        options.baseOptions.delegate = .CPU
        options.runningMode = .liveStream
        options.numHands = 2
        options.minHandDetectionConfidence = tuning.minimumTrackingConfidence
        options.minHandPresenceConfidence = tuning.minimumPresenceConfidence
        options.minTrackingConfidence = tuning.minimumTrackingConfidence
        super.init()
        options.handLandmarkerLiveStreamDelegate = self
        landmarker = try HandLandmarker(options: options)
    }

    static func bundled(tuning: GestureTuning = .default) throws -> MediaPipeHandTracker {
        guard let path = Bundle.main.path(forResource: "hand_landmarker", ofType: "task") else {
            throw TrackerError.modelMissing
        }
        return try MediaPipeHandTracker(modelPath: path, tuning: tuning)
    }

    func submit(
        _ frame: CameraFrame,
        completion: @escaping @Sendable (Result<HandTrackingResult, Error>) -> Void
    ) throws {
        let timestamp: Int = lock.withLock {
            let proposed = Int(CMTimeGetSeconds(frame.timestamp) * 1_000)
            let next = max(proposed, lastTimestamp + 1)
            lastTimestamp = next
            // Live-stream mode may intentionally drop frames. Bound retained callbacks
            // so a dropped result can never grow this dictionary indefinitely.
            let staleKeys = pending.keys.filter { next - $0 > 1_000 }
            for stale in staleKeys { pending.removeValue(forKey: stale) }
            pending[next] = Pending(
                completion: completion,
                imageSize: frame.orientedImageSize,
                timestamp: CMTimeGetSeconds(frame.timestamp)
            )
            return next
        }
        do {
            let image = try MPImage(pixelBuffer: frame.pixelBuffer, orientation: .right)
            try landmarker.detectAsync(image: image, timestampInMilliseconds: timestamp)
        } catch {
            let callback = lock.withLock { pending.removeValue(forKey: timestamp)?.completion }
            callback?(.failure(error))
            throw error
        }
    }

    private func convert(_ result: HandLandmarkerResult, timestamp: TimeInterval) -> [TrackedHand] {
        let count = min(result.landmarks.count, result.worldLandmarks.count)
        return (0..<count).compactMap { handIndex in
            let image = result.landmarks[handIndex]
            let world = result.worldLandmarks[handIndex]
            guard image.count >= Self.jointOrder.count, world.count >= Self.jointOrder.count else { return nil }
            let category = result.handedness.indices.contains(handIndex) ? result.handedness[handIndex].first : nil
            let handedness: Handedness
            // MediaPipe labels assume mirrored/selfie input. The rear camera is
            // unmirrored, so its reported labels are the physical opposite hand.
            switch category?.categoryName?.lowercased() {
            case "left": handedness = .right
            case "right": handedness = .left
            default: handedness = .unknown
            }
            let confidence = category?.score ?? 0.5
            var imagePoints: [LandmarkJoint: ImageLandmark] = [:]
            var worldPoints: [LandmarkJoint: WorldLandmark] = [:]
            for (index, joint) in Self.jointOrder.enumerated() {
                let imagePoint = image[index]
                let worldPoint = world[index]
                let pointConfidence = Float(imagePoint.presence?.doubleValue ?? Double(confidence))
                imagePoints[joint] = ImageLandmark(
                    location: CGPoint(x: CGFloat(imagePoint.x), y: CGFloat(1 - imagePoint.y)),
                    confidence: pointConfidence
                )
                worldPoints[joint] = WorldLandmark(
                    location: CameraSpaceVector(
                        x: Double(worldPoint.x),
                        y: Double(-worldPoint.y),
                        z: Double(worldPoint.z)
                    ),
                    confidence: Float(worldPoint.presence?.doubleValue ?? Double(confidence))
                )
            }
            return TrackedHand(
                imagePoints: imagePoints,
                worldPoints: worldPoints,
                handedness: handedness,
                confidence: confidence,
                timestamp: timestamp,
                palmFrame: PalmCoordinateFrame.make(points: worldPoints, handedness: handedness)
            )
        }
    }

    private enum TrackerError: LocalizedError {
        case modelMissing
        var errorDescription: String? { "The bundled MediaPipe hand_landmarker.task model is missing." }
    }

    private static let jointOrder: [LandmarkJoint] = [
        .wrist,
        .thumbCMC, .thumbMP, .thumbIP, .thumbTip,
        .indexMCP, .indexPIP, .indexDIP, .indexTip,
        .middleMCP, .middlePIP, .middleDIP, .middleTip,
        .ringMCP, .ringPIP, .ringDIP, .ringTip,
        .littleMCP, .littlePIP, .littleDIP, .littleTip
    ]
}

extension MediaPipeHandTracker: HandLandmarkerLiveStreamDelegate {
    func handLandmarker(
        _ handLandmarker: HandLandmarker,
        didFinishDetection result: HandLandmarkerResult?,
        timestampInMilliseconds: Int,
        error: Error?
    ) {
        guard let pending = lock.withLock({ self.pending.removeValue(forKey: timestampInMilliseconds) }) else { return }
        if let error {
            pending.completion(.failure(error))
            return
        }
        let hands = result.map { convert($0, timestamp: pending.timestamp) } ?? []
        pending.completion(.success(HandTrackingResult(
            hands: hands,
            timestamp: pending.timestamp,
            orientedImageSize: pending.imageSize
        )))
    }
}

private extension NSLock {
    func withLock<T>(_ operation: () -> T) -> T {
        lock()
        defer { unlock() }
        return operation()
    }
}
