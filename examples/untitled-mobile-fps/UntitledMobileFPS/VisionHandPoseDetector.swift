import Vision

final class VisionHandPoseDetector: HandTracking, @unchecked Sendable {
    let name = "VISION 2D"

    func submit(
        _ frame: CameraFrame,
        completion: @escaping @Sendable (Result<HandTrackingResult, Error>) -> Void
    ) throws {
        let request = VNDetectHumanHandPoseRequest()
        request.maximumHandCount = 2
        let handler = VNImageRequestHandler(cvPixelBuffer: frame.pixelBuffer, orientation: frame.orientation)
        do {
            try handler.perform([request])
            let hands = try (request.results ?? []).compactMap { try convert($0, timestamp: CMTimeGetSeconds(frame.timestamp)) }
            completion(.success(HandTrackingResult(
                hands: hands,
                timestamp: CMTimeGetSeconds(frame.timestamp),
                orientedImageSize: frame.orientedImageSize
            )))
        } catch {
            throw error
        }
    }

    private func convert(_ observation: VNHumanHandPoseObservation, timestamp: TimeInterval) throws -> TrackedHand? {
        let recognized = try observation.recognizedPoints(.all)
        var points: [LandmarkJoint: ImageLandmark] = [:]
        for (joint, visionJoint) in Self.jointMap {
            guard let point = recognized[visionJoint] else { continue }
            points[joint] = ImageLandmark(location: point.location, confidence: point.confidence)
        }
        guard !points.isEmpty else { return nil }
        let confidence = points.values.reduce(0) { $0 + $1.confidence } / Float(points.count)
        return TrackedHand(
            imagePoints: points,
            worldPoints: [:],
            handedness: .unknown,
            confidence: confidence,
            timestamp: timestamp,
            palmFrame: nil
        )
    }

    private static let jointMap: [LandmarkJoint: VNHumanHandPoseObservation.JointName] = [
        .wrist: .wrist,
        .thumbCMC: .thumbCMC, .thumbMP: .thumbMP, .thumbIP: .thumbIP, .thumbTip: .thumbTip,
        .indexMCP: .indexMCP, .indexPIP: .indexPIP, .indexDIP: .indexDIP, .indexTip: .indexTip,
        .middleMCP: .middleMCP, .middlePIP: .middlePIP, .middleDIP: .middleDIP, .middleTip: .middleTip,
        .ringMCP: .ringMCP, .ringPIP: .ringPIP, .ringDIP: .ringDIP, .ringTip: .ringTip,
        .littleMCP: .littleMCP, .littlePIP: .littlePIP, .littleDIP: .littleDIP, .littleTip: .littleTip
    ]
}
