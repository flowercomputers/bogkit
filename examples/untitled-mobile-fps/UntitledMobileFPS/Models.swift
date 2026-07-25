import AVFoundation
import CoreGraphics
import ImageIO

enum LandmarkJoint: String, CaseIterable, Codable, Hashable, Sendable {
    case wrist
    case thumbCMC, thumbMP, thumbIP, thumbTip
    case indexMCP, indexPIP, indexDIP, indexTip
    case middleMCP, middlePIP, middleDIP, middleTip
    case ringMCP, ringPIP, ringDIP, ringTip
    case littleMCP, littlePIP, littleDIP, littleTip
}

struct CameraSpaceVector: Codable, Equatable, Sendable {
    // Camera contract: +x screen-right, +y screen-up, +z away from the rear camera.
    var x: Double
    var y: Double
    var z: Double

    static let zero = CameraSpaceVector(x: 0, y: 0, z: 0)
    var length: Double { sqrt(x * x + y * y + z * z) }

    var normalized: CameraSpaceVector {
        let magnitude = length
        guard magnitude > 0.000_001 else { return .zero }
        return self / magnitude
    }

    func dot(_ other: CameraSpaceVector) -> Double { x * other.x + y * other.y + z * other.z }
    func cross(_ other: CameraSpaceVector) -> CameraSpaceVector {
        CameraSpaceVector(
            x: y * other.z - z * other.y,
            y: z * other.x - x * other.z,
            z: x * other.y - y * other.x
        )
    }

    static prefix func - (value: CameraSpaceVector) -> CameraSpaceVector {
        CameraSpaceVector(x: -value.x, y: -value.y, z: -value.z)
    }
    static func + (lhs: CameraSpaceVector, rhs: CameraSpaceVector) -> CameraSpaceVector {
        CameraSpaceVector(x: lhs.x + rhs.x, y: lhs.y + rhs.y, z: lhs.z + rhs.z)
    }
    static func - (lhs: CameraSpaceVector, rhs: CameraSpaceVector) -> CameraSpaceVector {
        CameraSpaceVector(x: lhs.x - rhs.x, y: lhs.y - rhs.y, z: lhs.z - rhs.z)
    }
    static func * (lhs: CameraSpaceVector, rhs: Double) -> CameraSpaceVector {
        CameraSpaceVector(x: lhs.x * rhs, y: lhs.y * rhs, z: lhs.z * rhs)
    }
    static func / (lhs: CameraSpaceVector, rhs: Double) -> CameraSpaceVector {
        CameraSpaceVector(x: lhs.x / rhs, y: lhs.y / rhs, z: lhs.z / rhs)
    }
}

struct ImageLandmark: Codable, Equatable, Sendable {
    // Vision-normalized coordinates: origin lower-left, +y up.
    let location: CGPoint
    let confidence: Float
}

struct WorldLandmark: Codable, Equatable, Sendable {
    let location: CameraSpaceVector
    let confidence: Float
}

enum Handedness: String, Codable, Equatable, Sendable {
    case left = "LEFT"
    case right = "RIGHT"
    case unknown = "UNKNOWN"
}

struct PalmCoordinateFrame: Codable, Equatable, Sendable {
    let origin: CameraSpaceVector
    let lateral: CameraSpaceVector
    let distal: CameraSpaceVector
    let normal: CameraSpaceVector
    let scale: Double

    static func make(points: [LandmarkJoint: WorldLandmark], handedness: Handedness) -> PalmCoordinateFrame? {
        guard let wrist = points[.wrist]?.location,
              let index = points[.indexMCP]?.location,
              let little = points[.littleMCP]?.location else { return nil }
        let width = index - little
        guard width.length > 0.000_001 else { return nil }
        var lateral = width.normalized
        if handedness == .left { lateral = -lateral }
        let centroid = (index + little) / 2
        let rawDistal = centroid - wrist
        let distal = (rawDistal - lateral * rawDistal.dot(lateral)).normalized
        guard distal.length > 0 else { return nil }
        let normal = lateral.cross(distal).normalized
        return PalmCoordinateFrame(origin: wrist, lateral: lateral, distal: distal, normal: normal, scale: width.length)
    }

    func local(_ point: CameraSpaceVector) -> CameraSpaceVector {
        let offset = (point - origin) / max(scale, 0.000_001)
        return CameraSpaceVector(x: offset.dot(lateral), y: offset.dot(distal), z: offset.dot(normal))
    }
}

struct TrackedHand: Codable, Equatable, Sendable {
    var imagePoints: [LandmarkJoint: ImageLandmark]
    var worldPoints: [LandmarkJoint: WorldLandmark]
    let handedness: Handedness
    let confidence: Float
    let timestamp: TimeInterval
    let palmFrame: PalmCoordinateFrame?

    subscript(image joint: LandmarkJoint) -> ImageLandmark? { imagePoints[joint] }
    subscript(world joint: LandmarkJoint) -> WorldLandmark? { worldPoints[joint] }

    var bounds: CGRect? {
        guard let first = imagePoints.values.first else { return nil }
        return imagePoints.values.dropFirst().reduce(CGRect(origin: first.location, size: .zero)) {
            $0.union(CGRect(origin: $1.location, size: .zero))
        }
    }

    func paddedBounds(by amount: CGFloat = 0.035, minimumConfidence: Float = 0.30) -> CGRect? {
        let points = imagePoints.values.filter { $0.confidence >= minimumConfidence }
        guard let first = points.first else { return nil }
        return points.dropFirst().reduce(CGRect(origin: first.location, size: .zero)) {
            $0.union(CGRect(origin: $1.location, size: .zero))
        }
        .insetBy(dx: -amount, dy: -amount)
        .intersection(CGRect(x: 0, y: 0, width: 1, height: 1))
    }
}

enum FingerGunVariation: String, Codable, Equatable, Sendable {
    case singleBarrel = "SINGLE"
    case doubleBarrel = "DOUBLE"
}

enum ThumbState: String, Codable, Equatable, Sendable {
    case up = "UP"
    case down = "DOWN"
    case ambiguous = "AMBIGUOUS"
}

enum FingerExtensionState: String, Codable, Equatable, Sendable {
    case straight = "STRAIGHT"
    case curled = "CURLED"
    case ambiguous = "AMBIG"
}

struct FingerGunObservation: Codable, Equatable, Sendable {
    let variation: FingerGunVariation
    let muzzlePoint: CGPoint
    let barrelDirection: CameraSpaceVector
    let confidence: Float
    let poseMargin: Double
    let thumbState: ThumbState
    let handedness: Handedness

    var rawYaw: Double { atan2(barrelDirection.x, barrelDirection.z) }
    var rawPitch: Double { atan2(barrelDirection.y, hypot(barrelDirection.x, barrelDirection.z)) }
}

struct BarrelCalibrationSample: Codable, Equatable, Sendable {
    let direction: CameraSpaceVector
    let handedness: Handedness
    let confidence: Float

    var rawYaw: Double { atan2(direction.x, direction.z) }
    var rawPitch: Double { atan2(direction.y, hypot(direction.x, direction.z)) }
}

struct FingerGunAnalysis: Codable, Equatable, Sendable {
    let observation: FingerGunObservation?
    let calibrationSample: BarrelCalibrationSample?
    let indexDirection: CameraSpaceVector?
    let middleDirection: CameraSpaceVector?
    let indexState: FingerExtensionState
    let middleState: FingerExtensionState
    let ringState: FingerExtensionState
    let littleState: FingerExtensionState
    let thumbState: ThumbState
    let rejectionReason: String?

    static let empty = FingerGunAnalysis(
        observation: nil,
        calibrationSample: nil,
        indexDirection: nil,
        middleDirection: nil,
        indexState: .ambiguous,
        middleState: .ambiguous,
        ringState: .ambiguous,
        littleState: .ambiguous,
        thumbState: .ambiguous,
        rejectionReason: "NO TRACK"
    )
}

struct VisionAimFeature: Codable, Equatable, Sendable {
    let tipX: Double
    let tipY: Double
    let pipX: Double
    let pipY: Double
    let dipX: Double
    let dipY: Double
    let projectedLength: Double

    var components: [Double] { [tipX, tipY, pipX, pipY, dipX, dipY, projectedLength] }
    var directionComponents: [Double] {
        [tipX - pipX, tipY - pipY, dipX - pipX, dipY - pipY, projectedLength]
    }
}

struct VisionFingerGunObservation: Codable, Equatable, Sendable {
    let variation: FingerGunVariation
    let muzzlePoint: CGPoint
    let aimFeature: VisionAimFeature
    let confidence: Float
    let poseMargin: Double
    let thumbState: ThumbState
}

struct VisionFingerGunAnalysis: Codable, Equatable, Sendable {
    let observation: VisionFingerGunObservation?
    let aimFeature: VisionAimFeature?
    var calibrationVariation: FingerGunVariation? = nil
    let indexState: FingerExtensionState
    let middleState: FingerExtensionState
    let ringState: FingerExtensionState
    let littleState: FingerExtensionState
    let thumbState: ThumbState
    let rejectionReason: String?
}

enum VisionCalibrationTarget: String, CaseIterable, Codable, Equatable, Sendable {
    case center = "CENTER"
    case left = "LEFT"
    case right = "RIGHT"
    case up = "UP"
    case down = "DOWN"

    // Vision-normalized coordinates use a lower-left origin.
    var point: CGPoint {
        switch self {
        case .center: return CGPoint(x: 0.5, y: 0.5)
        case .left: return CGPoint(x: 0.22, y: 0.5)
        case .right: return CGPoint(x: 0.78, y: 0.5)
        case .up: return CGPoint(x: 0.5, y: 0.76)
        case .down: return CGPoint(x: 0.5, y: 0.24)
        }
    }


    var aimZone: AimDirectionZone {
        switch self {
        case .center: return .center
        case .left: return .left
        case .right: return .right
        case .up: return .up
        case .down: return .down
        }
    }
}

enum AimDirectionZone: String, CaseIterable, Codable, Equatable, Sendable {
    case center = "CENTER"
    case left = "LEFT"
    case right = "RIGHT"
    case up = "UP"
    case down = "DOWN"
    case upLeft = "UP-LEFT"
    case upRight = "UP-RIGHT"
    case downLeft = "DOWN-LEFT"
    case downRight = "DOWN-RIGHT"

    var point: CGPoint {
        switch self {
        case .center: return CGPoint(x: 0.5, y: 0.5)
        case .left: return CGPoint(x: 0.2, y: 0.5)
        case .right: return CGPoint(x: 0.8, y: 0.5)
        case .up: return CGPoint(x: 0.5, y: 0.8)
        case .down: return CGPoint(x: 0.5, y: 0.2)
        case .upLeft: return CGPoint(x: 0.2, y: 0.8)
        case .upRight: return CGPoint(x: 0.8, y: 0.8)
        case .downLeft: return CGPoint(x: 0.2, y: 0.2)
        case .downRight: return CGPoint(x: 0.8, y: 0.2)
        }
    }
}

struct VisionAimCalibration: Codable, Equatable, Sendable {
    static let modelVersion = "vision-hand-pose-2d-v7"
    let featureMeans: [Double]
    let featureScales: [Double]
    let coefficientsX: [Double]
    let coefficientsY: [Double]
    let zoneCentroids: [VisionAimFeature]?
    let zoneRMS: [Double]?
    let templateMeans: [Double]?
    let templateScales: [Double]?
    let rootMeanSquareError: Double
    let variation: FingerGunVariation
    let cameraIdentifier: String
    let modelVersion: String
    let createdAt: Date
}

struct AimSolution: Codable, Equatable, Sendable {
    let rawYaw: Double
    let rawPitch: Double
    let filteredYaw: Double
    let filteredPitch: Double
    let rawScreenPoint: CGPoint
    let screenPoint: CGPoint
    let confidence: Float
    let valid: Bool

    // The primary Vision solver stores its One-Euro-filtered continuous point
    // here. Multiplayer uses that precise point while screenPoint remains the
    // stabilized nine-zone diagnostic output.
    var gameplayScreenPoint: CGPoint { rawScreenPoint }
}

struct AimCalibration: Codable, Equatable, Sendable {
    static let modelVersion = "hand_landmarker_float16_v1"
    let neutralDirection: CameraSpaceVector
    let neutralYaw: Double
    let neutralPitch: Double
    let neutralRoll: Double
    let angularVariance: Double
    let handedness: Handedness
    let cameraIdentifier: String
    let modelVersion: String
    let createdAt: Date
}

enum CalibrationState: Equatable, Sendable {
    case required(Handedness?)
    case collecting(progress: Double, handedness: Handedness?)
    case calibrated(Handedness)
    case failed(String)

    var label: String {
        switch self {
        case .required: return "CALIBRATION REQUIRED"
        case .collecting(let progress, _): return "CALIBRATING \(Int(progress * 100))%"
        case .calibrated(let hand): return "CALIBRATED \(hand.rawValue)"
        case .failed(let message): return "CAL FAILED: \(message)"
        }
    }
}

enum GestureState: String, Codable, Equatable, Sendable {
    case notDetected = "NO HAND"
    case candidate = "CANDIDATE"
    case armed = "ARMED"
    case fired = "FIRED"
    case waitingForRearm = "REARM"
}

struct GestureTuning: Equatable, Sendable {
    var minimumJointConfidence: Float = 0.45
    var minimumTrackingConfidence: Float = 0.50
    var minimumPresenceConfidence: Float = 0.50
    var straightJointAngleDegrees: Double = 145
    var doubleBarrelStraightAngleDegrees: Double = 148
    var straightLengthRatio: Double = 0.78
    var curledJointAngleDegrees: Double = 118
    var curledLengthRatio: Double = 0.68
    var curledTipToWristRatio: Double = 1.08
    var thumbUpDistanceRatio: Double = 1.18
    var thumbDownDistanceRatio: Double = 0.90
    var thumbUpAngleDegrees: Double = 135
    var thumbDownAngleDegrees: Double = 115
    var minimumSceneDepthComponent: Double = 0.04
    var minimumAimDepthComponent: Double = 0.10
    var maximumDoubleBarrelDivergenceDegrees: Double = 24
    var aimSensitivity: Double = 1.0
    var aimDeadZoneDegrees: Double = 1.2
    var aimMinimumCutoff: Double = 1.6
    var aimBeta: Double = 0.06
    var aimDerivativeCutoff: Double = 1.0
    var maximumAngularVelocityDegrees: Double = 520
    var stabilizationFrames: Int = 3
    var rearmFrames: Int = 2
    // Once a fully validated pose reaches ARMED, tolerate a brief loss of the
    // curled-finger labels while the thumb moves. This is deliberately shorter
    // than a deliberate pose change and applies only to the down transition.
    var armedPoseLatchSeconds: TimeInterval = 0.18
    var trackingGraceSeconds: TimeInterval = 0.10
    var trackingResetSeconds: TimeInterval = 0.20
    var calibrationFrames: Int = 30
    var calibrationMaximumVarianceDegrees: Double = 5.0
    var flashDuration: TimeInterval = 0.22
    var visionMinimumJointConfidence: Float = 0.25
    var visionStraightJointAngleDegrees: Double = 132
    var visionStraightLengthRatio: Double = 0.70
    var visionCurledJointAngleDegrees: Double = 108
    var visionCurledLengthRatio: Double = 0.60
    var visionThumbUpDistanceRatio: Double = 1.05
    var visionThumbDownDistanceRatio: Double = 0.92
    var visionThumbUpAngleDegrees: Double = 112
    var visionThumbDownAngleDegrees: Double = 106
    var visionCalibrationFramesPerTarget: Int = 18
    var visionCalibrationSettlingFrames: Int = 12
    var visionCalibrationFeatureJumpLimit: Double = 0.28
    var visionCalibrationMinimumConfidence: Float = 0.55
    var visionCalibrationTargetChangeMinimum: Double = 0.16
    var visionCalibrationTargetSeparationMinimum: Double = 0.08
    var visionCalibrationMaximumClusterRMS: Double = 0.14
    var visionCalibrationMaximumRMSE: Double = 0.18
    var visionCalibrationRidge: Double = 0.12
    var visionMaximumReticleVelocity: Double = 30.0
    var visionDirectionLowThreshold: Double = 0.38
    var visionDirectionHighThreshold: Double = 0.62
    var visionDirectionStabilizationFrames: Int = 5
    var visionMaximumTemplateDistance: Double = 1.25
    var visionAimHoldSeconds: TimeInterval = 0.35
    var scopeZoomFactor: Double = 1.25
    var scopeEntrySeconds: TimeInterval = 0.15
    var scopeEntryLossGraceSeconds: TimeInterval = 0.12
    var scopeExitSeconds: TimeInterval = 0.35
    var scopeRetentionLossSeconds: TimeInterval = 0.40
    // Sights engage when the palm appears this much larger than the player's
    // own relaxed hold, and release well below that so the mode cannot chatter
    // around a single threshold.
    var scopeEnterProximityRatio: Double = 1.40
    var scopeExitProximityRatio: Double = 1.15
    // The baseline is a low percentile of recent unscoped spans. A percentile
    // over a sliding window cannot latch the way a freeze-gated average did,
    // and it ignores both the brief approach and outlier spans.
    var scopeBaselineWindowSeconds: TimeInterval = 4.0
    var scopeBaselinePercentile: Double = 0.30
    var scopeBaselineMinimumSamples: Int = 20
    var scopeBaselineMinimumSeconds: TimeInterval = 0.7
    var scopeMaximumProximityRatio: Double = 4.0
    // Each palm pair independently estimates apparent hand scale; at least this
    // many must agree before a frame counts as measured.
    var scopeMinimumProximityPairs: Int = 2
    var scopeProximityPairDisagreement: Double = 0.35

    static let `default` = GestureTuning()
}

struct CameraFrame: @unchecked Sendable {
    let pixelBuffer: CVPixelBuffer
    let timestamp: CMTime
    let orientation: CGImagePropertyOrientation
    let orientedImageSize: CGSize
}

struct HandTrackingResult: Sendable {
    let hands: [TrackedHand]
    let timestamp: TimeInterval
    let orientedImageSize: CGSize
}

protocol HandTracking: AnyObject, Sendable {
    var name: String { get }
    func submit(_ frame: CameraFrame, completion: @escaping @Sendable (Result<HandTrackingResult, Error>) -> Void) throws
}

protocol FingerGunClassifying: Sendable {
    func analyze(_ hand: TrackedHand) -> FingerGunAnalysis
}

extension FingerGunClassifying {
    func classify(_ hand: TrackedHand) -> FingerGunObservation? { analyze(hand).observation }
}

protocol AimCalibrating: Sendable {
    mutating func begin(handedness: Handedness?)
    mutating func ingest(_ sample: BarrelCalibrationSample) -> AimCalibration?
    mutating func cancel()
}

protocol AimSolving: Sendable {
    mutating func solve(
        observation: FingerGunObservation,
        calibration: AimCalibration,
        timestamp: TimeInterval,
        horizontalFieldOfView: Double,
        verticalFieldOfView: Double
    ) -> AimSolution?
    mutating func reset()
}

struct GestureUpdate: Equatable, Sendable {
    let state: GestureState
    let fired: Bool
}
