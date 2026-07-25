import CoreGraphics
import Foundation

struct FingerGunClassifier: FingerGunClassifying {
    let tuning: GestureTuning

    init(tuning: GestureTuning = .default) { self.tuning = tuning }

    func analyze(_ hand: TrackedHand) -> FingerGunAnalysis {
        let required: [LandmarkJoint] = [
            .wrist, .thumbCMC, .thumbMP, .thumbIP, .thumbTip,
            .indexMCP, .indexPIP, .indexDIP, .indexTip,
            .middleMCP, .middlePIP, .middleDIP, .middleTip,
            .ringMCP, .ringPIP, .ringDIP, .ringTip,
            .littleMCP, .littlePIP, .littleDIP, .littleTip
        ]
        let palm = hand.palmFrame
        let indexDirection = fittedDirection(for: .index, in: hand)
        let middleDirection = fittedDirection(for: .middle, in: hand)
        let index = palm.map { fingerState(.index, hand: hand, palm: $0) } ?? .ambiguous
        let middle = palm.map { fingerState(.middle, hand: hand, palm: $0) } ?? .ambiguous
        let ring = palm.map { fingerState(.ring, hand: hand, palm: $0) } ?? .ambiguous
        let little = palm.map { fingerState(.little, hand: hand, palm: $0) } ?? .ambiguous
        let thumb = palm.map { classifyThumb(hand, palm: $0) } ?? .ambiguous

        // Calibration deliberately uses the raw fitted index ray, not the full
        // finger-gun classification. End-on fingers are the hardest case for
        // extension geometry, but their 3D ray is exactly what neutral aim needs.
        let calibrationSample: BarrelCalibrationSample?
        if hand.confidence >= tuning.minimumJointConfidence,
           hasConfidentWorldPoints(for: .index, in: hand),
           let indexDirection,
           indexDirection.z >= tuning.minimumAimDepthComponent {
            calibrationSample = BarrelCalibrationSample(
                direction: indexDirection,
                handedness: hand.handedness,
                confidence: hand.confidence
            )
        } else {
            calibrationSample = nil
        }

        func result(_ observation: FingerGunObservation? = nil, rejection: String? = nil) -> FingerGunAnalysis {
            FingerGunAnalysis(
                observation: observation,
                calibrationSample: calibrationSample,
                indexDirection: indexDirection,
                middleDirection: middleDirection,
                indexState: index,
                middleState: middle,
                ringState: ring,
                littleState: little,
                thumbState: thumb,
                rejectionReason: rejection
            )
        }

        guard hand.confidence >= tuning.minimumJointConfidence else { return result(rejection: "LOW HAND CONF") }
        guard required.allSatisfy({ (hand[world: $0]?.confidence ?? 0) >= tuning.minimumJointConfidence }) else {
            return result(rejection: "LOW/MISSING 3D")
        }
        guard palm != nil else { return result(rejection: "NO PALM FRAME") }
        guard let indexDirection, let middleDirection else { return result(rejection: "NO BARREL FIT") }
        guard let indexTip = hand[image: .indexTip]?.location,
              let middleTip = hand[image: .middleTip]?.location else { return result(rejection: "NO IMAGE TIP") }
        guard index == .straight else { return result(rejection: "INDEX \(index.rawValue)") }
        guard ring == .curled else { return result(rejection: "RING \(ring.rawValue)") }
        guard little == .curled else { return result(rejection: "LITTLE \(little.rawValue)") }

        let variation: FingerGunVariation
        let direction: CameraSpaceVector
        let muzzle: CGPoint
        switch middle {
        case .curled:
            variation = .singleBarrel
            direction = indexDirection
            muzzle = indexTip
        case .straight:
            let divergence = angleDegrees(indexDirection, middleDirection)
            guard divergence <= tuning.maximumDoubleBarrelDivergenceDegrees else {
                return result(rejection: "BARRELS DIVERGE")
            }
            variation = .doubleBarrel
            direction = (indexDirection + middleDirection).normalized
            muzzle = midpoint(indexTip, middleTip)
        case .ambiguous:
            return result(rejection: "MIDDLE AMBIG")
        }
        guard direction.z >= tuning.minimumSceneDepthComponent else { return result(rejection: "BARREL Z \(String(format: "%+.2f", direction.z))") }

        let confidence = required.compactMap { hand[world: $0]?.confidence }.reduce(0, +) / Float(required.count)
        let depthMargin = direction.z - tuning.minimumSceneDepthComponent
        let extensionMargin = max(0, fingerExtensionMargin(.index, hand: hand))
        let observation = FingerGunObservation(
            variation: variation,
            muzzlePoint: muzzle,
            barrelDirection: direction,
            confidence: min(confidence, hand.confidence),
            poseMargin: min(depthMargin, extensionMargin),
            thumbState: thumb,
            handedness: hand.handedness
        )
        return result(observation, rejection: nil)
    }

    private enum Finger { case thumb, index, middle, ring, little }

    private func joints(for finger: Finger) -> [LandmarkJoint] {
        switch finger {
        case .thumb: return [.thumbCMC, .thumbMP, .thumbIP, .thumbTip]
        case .index: return [.indexMCP, .indexPIP, .indexDIP, .indexTip]
        case .middle: return [.middleMCP, .middlePIP, .middleDIP, .middleTip]
        case .ring: return [.ringMCP, .ringPIP, .ringDIP, .ringTip]
        case .little: return [.littleMCP, .littlePIP, .littleDIP, .littleTip]
        }
    }

    private func fingerState(_ finger: Finger, hand: TrackedHand, palm: PalmCoordinateFrame) -> FingerExtensionState {
        let joints = joints(for: finger)
        guard let mcp = hand[world: joints[0]]?.location,
              let pip = hand[world: joints[1]]?.location,
              let dip = hand[world: joints[2]]?.location,
              let tip = hand[world: joints[3]]?.location,
              let wrist = hand[world: .wrist]?.location else { return .ambiguous }
        let proximal = angleDegrees(mcp, pip, dip)
        let distal = angleDegrees(pip, dip, tip)
        let path = distance(mcp, pip) + distance(pip, dip) + distance(dip, tip)
        let ratio = path > 0 ? distance(mcp, tip) / path : 0
        if proximal >= tuning.straightJointAngleDegrees,
           distal >= tuning.straightJointAngleDegrees,
           ratio >= tuning.straightLengthRatio {
            return .straight
        }
        let localTip = palm.local(tip)
        let localMCP = palm.local(mcp)
        let foldsTowardPalm = localTip.y <= localMCP.y + 0.35
        if proximal <= tuning.curledJointAngleDegrees || distal <= tuning.curledJointAngleDegrees ||
            ratio <= tuning.curledLengthRatio ||
            distance(tip, wrist) <= distance(pip, wrist) * tuning.curledTipToWristRatio || foldsTowardPalm {
            return .curled
        }
        return .ambiguous
    }

    private func hasConfidentWorldPoints(for finger: Finger, in hand: TrackedHand) -> Bool {
        joints(for: finger).allSatisfy {
            (hand[world: $0]?.confidence ?? 0) >= tuning.minimumJointConfidence
        }
    }

    private func fingerExtensionMargin(_ finger: Finger, hand: TrackedHand) -> Double {
        let joints = joints(for: finger)
        guard let mcp = hand[world: joints[0]]?.location,
              let pip = hand[world: joints[1]]?.location,
              let dip = hand[world: joints[2]]?.location,
              let tip = hand[world: joints[3]]?.location else { return 0 }
        let angleMargin = min(angleDegrees(mcp, pip, dip), angleDegrees(pip, dip, tip)) - tuning.straightJointAngleDegrees
        return angleMargin / 180
    }

    private func classifyThumb(_ hand: TrackedHand, palm: PalmCoordinateFrame) -> ThumbState {
        guard let cmc = hand[world: .thumbCMC]?.location,
              let mp = hand[world: .thumbMP]?.location,
              let ip = hand[world: .thumbIP]?.location,
              let tip = hand[world: .thumbTip]?.location,
              let indexMCP = hand[world: .indexMCP]?.location else { return .ambiguous }
        let distanceRatio = distance(tip, indexMCP) / max(palm.scale, 0.000_001)
        let angle = min(angleDegrees(cmc, mp, ip), angleDegrees(mp, ip, tip))
        let separation = abs(palm.local(tip).x - palm.local(indexMCP).x)
        if distanceRatio >= tuning.thumbUpDistanceRatio,
           angle >= tuning.thumbUpAngleDegrees,
           separation >= 0.45 {
            return .up
        }
        if distanceRatio <= tuning.thumbDownDistanceRatio || angle <= tuning.thumbDownAngleDegrees {
            return .down
        }
        return .ambiguous
    }

    private func fittedDirection(for finger: Finger, in hand: TrackedHand) -> CameraSpaceVector? {
        let points = joints(for: finger).compactMap { hand[world: $0]?.location }
        guard points.count == 4 else { return nil }
        let center = points.reduce(.zero, +) / Double(points.count)
        var axis = (points.last! - points.first!).normalized
        guard axis.length > 0 else { return nil }
        // Power iteration on the 3x3 covariance gives a small total-least-squares line fit.
        for _ in 0..<8 {
            var next = CameraSpaceVector.zero
            for point in points {
                let offset = point - center
                next = next + offset * offset.dot(axis)
            }
            axis = next.normalized
        }
        if axis.dot(points.last! - points.first!) < 0 { axis = -axis }
        return axis.normalized
    }
}

func distance(_ a: CameraSpaceVector, _ b: CameraSpaceVector) -> Double { (a - b).length }
func midpoint(_ a: CGPoint, _ b: CGPoint) -> CGPoint { CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2) }

func angleDegrees(_ a: CameraSpaceVector, _ vertex: CameraSpaceVector, _ c: CameraSpaceVector) -> Double {
    angleDegrees(a - vertex, c - vertex)
}

func angleDegrees(_ a: CameraSpaceVector, _ b: CameraSpaceVector) -> Double {
    let denominator = max(a.length * b.length, 0.000_001)
    return acos(min(max(a.dot(b) / denominator, -1), 1)) * 180 / .pi
}

func clampedToViewport(_ point: CGPoint) -> CGPoint {
    CGPoint(x: min(max(point.x, 0.02), 0.98), y: min(max(point.y, 0.02), 0.98))
}

struct VisionFingerGunClassifier: Sendable {
    let tuning: GestureTuning

    init(tuning: GestureTuning = .default) { self.tuning = tuning }

    func analyze(_ hand: TrackedHand) -> VisionFingerGunAnalysis {
        let index = fingerState(.index, hand: hand)
        let middle = fingerState(.middle, hand: hand)
        let ring = fingerState(.ring, hand: hand)
        let little = fingerState(.little, hand: hand)
        let thumb = thumbState(hand)

        func output(
            observation: VisionFingerGunObservation? = nil,
            feature: VisionAimFeature? = nil,
            calibrationVariation: FingerGunVariation? = nil,
            rejection: String? = nil
        ) -> VisionFingerGunAnalysis {
            VisionFingerGunAnalysis(
                observation: observation,
                aimFeature: feature,
                calibrationVariation: calibrationVariation,
                indexState: index,
                middleState: middle,
                ringState: ring,
                littleState: little,
                thumbState: thumb,
                rejectionReason: rejection
            )
        }

        guard let indexFeature = aimFeature(hand: hand, variation: .singleBarrel) else {
            return output(rejection: "LOW/MISSING 2D")
        }
        let calibrationVariation: FingerGunVariation?
        switch middle {
        case .curled: calibrationVariation = .singleBarrel
        case .straight: calibrationVariation = .doubleBarrel
        case .ambiguous: calibrationVariation = nil
        }
        // Aim is deliberately index-based for both gesture variations. Vision's
        // middle-finger state is unreliable when curled fingers overlap end-on;
        // allowing that label to change the feature space invalidated otherwise
        // good calibrations whenever SINGLE/DOUBLE flickered.
        let calibrationFeature = indexFeature
        guard index == .straight else {
            return output(feature: calibrationFeature, calibrationVariation: calibrationVariation, rejection: "INDEX \(index.rawValue)")
        }
        guard ring == .curled else {
            return output(feature: calibrationFeature, calibrationVariation: calibrationVariation, rejection: "RING \(ring.rawValue)")
        }
        guard little == .curled else {
            return output(feature: calibrationFeature, calibrationVariation: calibrationVariation, rejection: "LITTLE \(little.rawValue)")
        }
        guard let variation = calibrationVariation else {
            return output(feature: indexFeature, rejection: "MIDDLE AMBIG")
        }
        guard let indexTip = point(.indexTip, in: hand) else {
            return output(feature: indexFeature, calibrationVariation: variation, rejection: "NO BARREL FEATURE")
        }
        let muzzle: CGPoint
        if variation == .doubleBarrel, let middleTip = point(.middleTip, in: hand) {
            muzzle = midpoint(indexTip, middleTip)
        } else {
            muzzle = indexTip
        }
        let required = joints(for: .index) + joints(for: .middle) + joints(for: .ring) + joints(for: .little) + [.wrist]
        let confidence = required.compactMap { hand[image: $0]?.confidence }.reduce(0, +) / Float(required.count)
        let margin = max(0, straightMargin(.index, hand: hand))
        let observation = VisionFingerGunObservation(
            variation: variation,
            muzzlePoint: muzzle,
            aimFeature: indexFeature,
            confidence: min(confidence, hand.confidence),
            poseMargin: margin,
            thumbState: thumb
        )
        return output(observation: observation, feature: indexFeature, calibrationVariation: variation)
    }

    private enum Finger { case thumb, index, middle, ring, little }

    private func joints(for finger: Finger) -> [LandmarkJoint] {
        switch finger {
        case .thumb: return [.thumbCMC, .thumbMP, .thumbIP, .thumbTip]
        case .index: return [.indexMCP, .indexPIP, .indexDIP, .indexTip]
        case .middle: return [.middleMCP, .middlePIP, .middleDIP, .middleTip]
        case .ring: return [.ringMCP, .ringPIP, .ringDIP, .ringTip]
        case .little: return [.littleMCP, .littlePIP, .littleDIP, .littleTip]
        }
    }

    private func point(_ joint: LandmarkJoint, in hand: TrackedHand) -> CGPoint? {
        guard let value = hand[image: joint], value.confidence >= tuning.visionMinimumJointConfidence else { return nil }
        return value.location
    }

    private func fingerState(_ finger: Finger, hand: TrackedHand) -> FingerExtensionState {
        let names = joints(for: finger)
        guard let mcp = point(names[0], in: hand),
              let pip = point(names[1], in: hand),
              let dip = point(names[2], in: hand),
              let tip = point(names[3], in: hand),
              let wrist = point(.wrist, in: hand) else { return .ambiguous }
        let proximal = imageAngle(mcp, pip, dip)
        let distal = imageAngle(pip, dip, tip)
        let path = imageDistance(mcp, pip) + imageDistance(pip, dip) + imageDistance(dip, tip)
        let ratio = path > 0.000_001 ? imageDistance(mcp, tip) / path : 0
        if proximal >= tuning.visionStraightJointAngleDegrees,
           distal >= tuning.visionStraightJointAngleDegrees,
           ratio >= tuning.visionStraightLengthRatio {
            return .straight
        }
        if proximal <= tuning.visionCurledJointAngleDegrees ||
            distal <= tuning.visionCurledJointAngleDegrees ||
            ratio <= tuning.visionCurledLengthRatio ||
            imageDistance(tip, wrist) <= imageDistance(pip, wrist) * 1.02 {
            return .curled
        }
        return .ambiguous
    }

    private func straightMargin(_ finger: Finger, hand: TrackedHand) -> Double {
        let names = joints(for: finger)
        guard let mcp = point(names[0], in: hand),
              let pip = point(names[1], in: hand),
              let dip = point(names[2], in: hand),
              let tip = point(names[3], in: hand) else { return 0 }
        return (min(imageAngle(mcp, pip, dip), imageAngle(pip, dip, tip)) - tuning.visionStraightJointAngleDegrees) / 180
    }

    private func thumbState(_ hand: TrackedHand) -> ThumbState {
        guard let cmc = point(.thumbCMC, in: hand),
              let mp = point(.thumbMP, in: hand),
              let ip = point(.thumbIP, in: hand),
              let tip = point(.thumbTip, in: hand),
              let indexMCP = point(.indexMCP, in: hand),
              let littleMCP = point(.littleMCP, in: hand) else { return .ambiguous }
        let scale = max(imageDistance(indexMCP, littleMCP), 0.000_001)
        let distanceRatio = imageDistance(tip, indexMCP) / scale
        let angle = min(imageAngle(cmc, mp, ip), imageAngle(mp, ip, tip))
        if distanceRatio >= tuning.visionThumbUpDistanceRatio,
           angle >= tuning.visionThumbUpAngleDegrees { return .up }
        if distanceRatio <= tuning.visionThumbDownDistanceRatio ||
            angle <= tuning.visionThumbDownAngleDegrees { return .down }
        return .ambiguous
    }

    private func aimFeature(hand: TrackedHand, variation: FingerGunVariation) -> VisionAimFeature? {
        guard let wrist = point(.wrist, in: hand),
              let indexMCP = point(.indexMCP, in: hand),
              let littleMCP = point(.littleMCP, in: hand),
              let indexPIP = point(.indexPIP, in: hand),
              let indexDIP = point(.indexDIP, in: hand),
              let indexTip = point(.indexTip, in: hand) else { return nil }
        let scale = imageDistance(indexMCP, littleMCP)
        guard scale > 0.005 else { return nil }
        let palm = CGPoint(
            x: (wrist.x + indexMCP.x + littleMCP.x) / 3,
            y: (wrist.y + indexMCP.y + littleMCP.y) / 3
        )
        var tip = indexTip
        var pip = indexPIP
        var dip = indexDIP
        var length = imageDistance(indexMCP, indexPIP) + imageDistance(indexPIP, indexDIP) + imageDistance(indexDIP, indexTip)
        if variation == .doubleBarrel,
           let middleMCP = point(.middleMCP, in: hand),
           let middlePIP = point(.middlePIP, in: hand),
           let middleDIP = point(.middleDIP, in: hand),
           let middleTip = point(.middleTip, in: hand) {
            tip = midpoint(indexTip, middleTip)
            pip = midpoint(indexPIP, middlePIP)
            dip = midpoint(indexDIP, middleDIP)
            let middleLength = imageDistance(middleMCP, middlePIP) + imageDistance(middlePIP, middleDIP) + imageDistance(middleDIP, middleTip)
            length = (length + middleLength) / 2
        }
        return VisionAimFeature(
            tipX: Double((tip.x - palm.x) / scale),
            tipY: Double((tip.y - palm.y) / scale),
            pipX: Double((pip.x - palm.x) / scale),
            pipY: Double((pip.y - palm.y) / scale),
            dipX: Double((dip.x - palm.x) / scale),
            dipY: Double((dip.y - palm.y) / scale),
            projectedLength: Double(length / scale)
        )
    }
}

private func imageDistance(_ a: CGPoint, _ b: CGPoint) -> CGFloat { hypot(a.x - b.x, a.y - b.y) }

private func imageAngle(_ a: CGPoint, _ vertex: CGPoint, _ c: CGPoint) -> Double {
    let lhs = CGVector(dx: a.x - vertex.x, dy: a.y - vertex.y)
    let rhs = CGVector(dx: c.x - vertex.x, dy: c.y - vertex.y)
    let denominator = max(hypot(lhs.dx, lhs.dy) * hypot(rhs.dx, rhs.dy), 0.000_001)
    let cosine = min(max((lhs.dx * rhs.dx + lhs.dy * rhs.dy) / denominator, -1), 1)
    return acos(Double(cosine)) * 180 / .pi
}
