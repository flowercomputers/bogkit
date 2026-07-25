import Foundation
import CoreGraphics

struct AimCalibrationCollector: AimCalibrating {
    private(set) var requestedHandedness: Handedness?
    private(set) var samples: [BarrelCalibrationSample] = []
    let tuning: GestureTuning
    let cameraIdentifier: String

    init(tuning: GestureTuning = .default, cameraIdentifier: String) {
        self.tuning = tuning
        self.cameraIdentifier = cameraIdentifier
    }

    mutating func begin(handedness: Handedness?) {
        requestedHandedness = handedness
        samples.removeAll(keepingCapacity: true)
    }

    mutating func cancel() {
        requestedHandedness = nil
        samples.removeAll()
    }

    mutating func ingest(_ sample: BarrelCalibrationSample) -> AimCalibration? {
        guard sample.direction.z >= tuning.minimumAimDepthComponent,
              requestedHandedness == nil || requestedHandedness == sample.handedness else { return nil }
        if let first = samples.first,
           first.handedness != sample.handedness {
            samples.removeAll(keepingCapacity: true)
        }
        samples.append(sample)
        guard samples.count >= tuning.calibrationFrames else { return nil }

        let yaw = median(samples.map(\.rawYaw))
        let pitch = median(samples.map(\.rawPitch))
        let variance = sqrt(samples.reduce(0) { partial, sample in
            partial + pow(sample.rawYaw - yaw, 2) + pow(sample.rawPitch - pitch, 2)
        } / Double(samples.count))
        let maximumVariance = tuning.calibrationMaximumVarianceDegrees * .pi / 180
        guard variance <= maximumVariance else {
            samples.removeAll(keepingCapacity: true)
            return nil
        }
        let direction = CameraSpaceVector(
            x: median(samples.map { $0.direction.x }),
            y: median(samples.map { $0.direction.y }),
            z: median(samples.map { $0.direction.z })
        ).normalized
        let hand = sample.handedness
        let calibration = AimCalibration(
            neutralDirection: direction,
            neutralYaw: yaw,
            neutralPitch: pitch,
            neutralRoll: 0,
            angularVariance: variance,
            handedness: hand,
            cameraIdentifier: cameraIdentifier,
            modelVersion: AimCalibration.modelVersion,
            createdAt: Date()
        )
        cancel()
        return calibration
    }

    var progress: Double { min(Double(samples.count) / Double(max(tuning.calibrationFrames, 1)), 1) }
}

final class AimCalibrationStore: @unchecked Sendable {
    private let defaults: UserDefaults
    init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    func calibration(for hand: Handedness, cameraIdentifier: String) -> AimCalibration? {
        guard let data = defaults.data(forKey: key(hand, cameraIdentifier)),
              let value = try? JSONDecoder().decode(AimCalibration.self, from: data),
              value.modelVersion == AimCalibration.modelVersion else { return nil }
        return value
    }

    func save(_ calibration: AimCalibration) {
        guard let data = try? JSONEncoder().encode(calibration) else { return }
        defaults.set(data, forKey: key(calibration.handedness, calibration.cameraIdentifier))
    }

    func reset(cameraIdentifier: String) {
        for hand in [Handedness.left, .right, .unknown] {
            defaults.removeObject(forKey: key(hand, cameraIdentifier))
        }
    }

    private func key(_ hand: Handedness, _ camera: String) -> String {
        "aim-calibration.\(AimCalibration.modelVersion).\(camera).\(hand.rawValue)"
    }
}

struct AngularAimSolver: AimSolving {
    let tuning: GestureTuning
    private var yawFilter: OneEuroFilter
    private var pitchFilter: OneEuroFilter
    private var lastRaw: (yaw: Double, pitch: Double, timestamp: TimeInterval)?

    init(tuning: GestureTuning = .default) {
        self.tuning = tuning
        yawFilter = OneEuroFilter(tuning: tuning)
        pitchFilter = OneEuroFilter(tuning: tuning)
    }

    mutating func solve(
        observation: FingerGunObservation,
        calibration: AimCalibration,
        timestamp: TimeInterval,
        horizontalFieldOfView: Double,
        verticalFieldOfView: Double
    ) -> AimSolution? {
        guard calibration.handedness == observation.handedness,
              observation.barrelDirection.z >= tuning.minimumAimDepthComponent else { return nil }
        let rawYaw = observation.rawYaw - calibration.neutralYaw
        let rawPitch = observation.rawPitch - calibration.neutralPitch
        if let lastRaw {
            let dt = max(timestamp - lastRaw.timestamp, 0.001)
            let velocity = hypot(rawYaw - lastRaw.yaw, rawPitch - lastRaw.pitch) / dt * 180 / .pi
            if velocity > tuning.maximumAngularVelocityDegrees { return nil }
        }
        lastRaw = (rawYaw, rawPitch, timestamp)
        let filteredYaw = yawFilter.filter(rawYaw, timestamp: timestamp, confidence: observation.confidence)
        let filteredPitch = pitchFilter.filter(rawPitch, timestamp: timestamp, confidence: observation.confidence)
        let deadZone = tuning.aimDeadZoneDegrees * .pi / 180
        let yaw = abs(filteredYaw) < deadZone ? 0 : filteredYaw - copysign(deadZone, filteredYaw)
        let pitch = abs(filteredPitch) < deadZone ? 0 : filteredPitch - copysign(deadZone, filteredPitch)
        let horizontal = max(horizontalFieldOfView * .pi / 180, 0.01)
        let vertical = max(verticalFieldOfView * .pi / 180, 0.01)
        let rawPoint = clampedToViewport(CGPoint(
            x: 0.5 + 0.5 * tan(rawYaw) / tan(horizontal / 2) * tuning.aimSensitivity,
            y: 0.5 + 0.5 * tan(rawPitch) / tan(vertical / 2) * tuning.aimSensitivity
        ))
        let point = clampedToViewport(CGPoint(
            x: 0.5 + 0.5 * tan(yaw) / tan(horizontal / 2) * tuning.aimSensitivity,
            y: 0.5 + 0.5 * tan(pitch) / tan(vertical / 2) * tuning.aimSensitivity
        ))
        return AimSolution(
            rawYaw: rawYaw,
            rawPitch: rawPitch,
            filteredYaw: filteredYaw,
            filteredPitch: filteredPitch,
            rawScreenPoint: rawPoint,
            screenPoint: point,
            confidence: observation.confidence,
            valid: true
        )
    }

    mutating func reset() {
        yawFilter.reset()
        pitchFilter.reset()
        lastRaw = nil
    }
}

struct OneEuroFilter: Sendable {
    private let tuning: GestureTuning
    private var value: Double?
    private var derivative = 0.0
    private var timestamp: TimeInterval?

    init(tuning: GestureTuning) { self.tuning = tuning }

    mutating func filter(_ sample: Double, timestamp newTimestamp: TimeInterval, confidence: Float) -> Double {
        guard let previous = value, let timestamp else {
            value = sample
            self.timestamp = newTimestamp
            return sample
        }
        let dt = max(newTimestamp - timestamp, 1.0 / 120.0)
        let rawDerivative = (sample - previous) / dt
        derivative = lowPass(rawDerivative, previous: derivative, cutoff: tuning.aimDerivativeCutoff, dt: dt)
        // Kept modest: a large penalty here drives the cutoff to its floor whenever
        // Vision confidence dips, which reads as the reticle "floating" while at rest.
        let confidencePenalty = Double(max(0, 1 - confidence)) * 0.5
        let cutoff = max(0.1, tuning.aimMinimumCutoff + tuning.aimBeta * abs(derivative) - confidencePenalty)
        let next = lowPass(sample, previous: previous, cutoff: cutoff, dt: dt)
        value = next
        self.timestamp = newTimestamp
        return next
    }

    mutating func reset() { value = nil; derivative = 0; timestamp = nil }

    private func lowPass(_ sample: Double, previous: Double, cutoff: Double, dt: Double) -> Double {
        let tau = 1 / (2 * Double.pi * cutoff)
        let alpha = 1 / (1 + tau / dt)
        return previous + alpha * (sample - previous)
    }
}

private func median(_ values: [Double]) -> Double {
    let sorted = values.sorted()
    guard !sorted.isEmpty else { return 0 }
    let middle = sorted.count / 2
    return sorted.count.isMultiple(of: 2) ? (sorted[middle - 1] + sorted[middle]) / 2 : sorted[middle]
}

struct VisionAimCalibrationCollector: Sendable {
    private(set) var targetIndex = 0
    private(set) var targetSamples: [[VisionAimFeature]]
    private(set) var variation: FingerGunVariation?
    private(set) var awaitingTargetMovement = false
    private(set) var failureReason: String?
    private var settlingFrames: Int
    private var lastFeature: VisionAimFeature?
    let tuning: GestureTuning
    let cameraIdentifier: String

    init(tuning: GestureTuning = .default, cameraIdentifier: String) {
        self.tuning = tuning
        self.cameraIdentifier = cameraIdentifier
        targetSamples = Array(repeating: [], count: VisionCalibrationTarget.allCases.count)
        settlingFrames = tuning.visionCalibrationSettlingFrames
    }

    mutating func begin() {
        targetIndex = 0
        targetSamples = Array(repeating: [], count: VisionCalibrationTarget.allCases.count)
        variation = nil
        awaitingTargetMovement = false
        failureReason = nil
        settlingFrames = tuning.visionCalibrationSettlingFrames
        lastFeature = nil
    }

    mutating func cancel() { begin() }

    var currentTarget: VisionCalibrationTarget? {
        VisionCalibrationTarget.allCases.indices.contains(targetIndex) ? VisionCalibrationTarget.allCases[targetIndex] : nil
    }

    var targetProgress: Double {
        guard targetSamples.indices.contains(targetIndex) else { return 1 }
        return min(Double(targetSamples[targetIndex].count) / Double(max(tuning.visionCalibrationFramesPerTarget, 1)), 1)
    }

    var instruction: String? {
        guard let currentTarget else { return nil }
        if awaitingTargetMovement {
            return "Move your aim to \(currentTarget.rawValue)"
        }
        return "Hold steady on \(currentTarget.rawValue)"
    }

    var overallProgress: Double {
        let completed = targetSamples.prefix(targetIndex).reduce(0) { $0 + min($1.count, tuning.visionCalibrationFramesPerTarget) }
        let current = targetSamples.indices.contains(targetIndex) ? min(targetSamples[targetIndex].count, tuning.visionCalibrationFramesPerTarget) : 0
        let total = tuning.visionCalibrationFramesPerTarget * VisionCalibrationTarget.allCases.count
        return total > 0 ? Double(completed + current) / Double(total) : 0
    }

    mutating func ingest(_ observation: VisionFingerGunObservation) -> VisionAimCalibration? {
        ingest(
            feature: observation.aimFeature,
            variation: observation.variation,
            thumbState: observation.thumbState,
            confidence: observation.confidence
        )
    }

    mutating func ingest(
        feature: VisionAimFeature,
        variation _: FingerGunVariation,
        thumbState: ThumbState,
        confidence: Float
    ) -> VisionAimCalibration? {
        guard thumbState == .up,
              confidence >= tuning.visionCalibrationMinimumConfidence,
              currentTarget != nil,
              failureReason == nil else { return nil }
        // Calibration and reticle direction always use the index feature. Keep
        // barrel variation out of this state so end-on middle-finger label
        // flicker cannot stall or invalidate calibration.
        if variation == nil { variation = .singleBarrel }

        if awaitingTargetMovement,
           targetIndex > 0,
           let previousCenter = centroid(of: targetSamples[targetIndex - 1]) {
            guard featureDistance(previousCenter, feature) >= tuning.visionCalibrationTargetChangeMinimum else {
                return nil
            }
            awaitingTargetMovement = false
            settlingFrames = tuning.visionCalibrationSettlingFrames
            lastFeature = feature
            return nil
        }

        if let lastFeature {
            let jump = featureDistance(lastFeature, feature)
            if jump > tuning.visionCalibrationFeatureJumpLimit {
                targetSamples[targetIndex].removeAll(keepingCapacity: true)
                settlingFrames = tuning.visionCalibrationSettlingFrames
            }
        }
        lastFeature = feature
        if settlingFrames > 0 {
            settlingFrames -= 1
            return nil
        }

        targetSamples[targetIndex].append(feature)
        guard targetSamples[targetIndex].count >= tuning.visionCalibrationFramesPerTarget else { return nil }
        if clusterRMS(targetSamples[targetIndex]) > tuning.visionCalibrationMaximumClusterRMS {
            targetSamples[targetIndex].removeAll(keepingCapacity: true)
            settlingFrames = tuning.visionCalibrationSettlingFrames
            lastFeature = nil
            return nil
        }
        targetIndex += 1
        lastFeature = nil
        settlingFrames = tuning.visionCalibrationSettlingFrames
        awaitingTargetMovement = targetIndex < VisionCalibrationTarget.allCases.count
        guard targetIndex >= VisionCalibrationTarget.allCases.count else { return nil }
        return fitCalibration()
    }

    private mutating func fitCalibration() -> VisionAimCalibration? {
        let centers = targetSamples.compactMap { centroid(of: $0) }
        guard centers.count == VisionCalibrationTarget.allCases.count else {
            failureReason = "Calibration did not capture every target. Please try again."
            return nil
        }
        for first in centers.indices {
            for second in centers.indices where second > first {
                if featureDistance(centers[first], centers[second]) < tuning.visionCalibrationTargetSeparationMinimum {
                    failureReason = "Some aim points were too similar. Move the finger gun to each target, not just the hand."
                    return nil
                }
            }
        }
        let components = targetSamples.flatMap { $0 }.map(\.components)
        guard let dimension = components.first?.count, dimension > 0,
              components.allSatisfy({ $0.count == dimension }) else {
            failureReason = "Calibration landmarks were incomplete. Please try again."
            return nil
        }
        let means = (0..<dimension).map { column in
            components.reduce(0) { $0 + $1[column] } / Double(components.count)
        }
        let scales = (0..<dimension).map { column in
            let variance = components.reduce(0) { $0 + pow($1[column] - means[column], 2) } / Double(components.count)
            return max(sqrt(variance), 0.001)
        }
        let templateComponents = targetSamples.flatMap { $0 }.map(\.directionComponents)
        let templateDimension = templateComponents.first?.count ?? 0
        let templateMeans = (0..<templateDimension).map { column in
            templateComponents.reduce(0) { $0 + $1[column] } / Double(templateComponents.count)
        }
        let templateScales = (0..<templateDimension).map { column in
            let variance = templateComponents.reduce(0) {
                $0 + pow($1[column] - templateMeans[column], 2)
            } / Double(templateComponents.count)
            return max(sqrt(variance), 0.001)
        }
        let zoneRMS = zip(targetSamples, centers).map { samples, center in
            let standardizedCenter = zip(center.directionComponents, zip(templateMeans, templateScales)).map { value, pair in
                (value - pair.0) / pair.1
            }
            return sqrt(samples.reduce(0) { partial, sample in
                let standardized = zip(sample.directionComponents, zip(templateMeans, templateScales)).map { value, pair in
                    (value - pair.0) / pair.1
                }
                let squared = zip(standardized, standardizedCenter).reduce(0) {
                    $0 + pow($1.0 - $1.1, 2)
                }
                return partial + squared / Double(max(templateDimension, 1))
            } / Double(max(samples.count, 1)))
        }
        var design: [[Double]] = []
        var targetsX: [Double] = []
        var targetsY: [Double] = []
        for (index, samples) in targetSamples.enumerated() {
            let target = VisionCalibrationTarget.allCases[index].point
            for sample in samples {
                design.append([1] + zip(sample.components, zip(means, scales)).map { value, pair in
                    (value - pair.0) / pair.1
                })
                targetsX.append(Double(target.x))
                targetsY.append(Double(target.y))
            }
        }
        guard let coefficientsX = ridgeRegression(design: design, targets: targetsX, lambda: tuning.visionCalibrationRidge),
              let coefficientsY = ridgeRegression(design: design, targets: targetsY, lambda: tuning.visionCalibrationRidge),
              let variation else {
            failureReason = "Calibration could not fit a stable aim mapping. Please try again."
            return nil
        }
        let error = sqrt(zip(design.indices, design).reduce(0) { partial, entry in
            let predictedX = dot(entry.1, coefficientsX)
            let predictedY = dot(entry.1, coefficientsY)
            return partial + pow(predictedX - targetsX[entry.0], 2) + pow(predictedY - targetsY[entry.0], 2)
        } / Double(max(design.count, 1)))
        guard error <= tuning.visionCalibrationMaximumRMSE else {
            failureReason = String(
                format: "Calibration quality was too low (error %.3f). Use natural center, left, right, up, and down poses and try again.",
                error
            )
            return nil
        }
        let fitted = VisionAimCalibration(
            featureMeans: means,
            featureScales: scales,
            coefficientsX: coefficientsX,
            coefficientsY: coefficientsY,
            zoneCentroids: centers,
            zoneRMS: zoneRMS,
            templateMeans: templateMeans,
            templateScales: templateScales,
            rootMeanSquareError: error,
            variation: variation,
            cameraIdentifier: cameraIdentifier,
            modelVersion: VisionAimCalibration.modelVersion,
            createdAt: Date()
        )
        // The pairwise separation check above measures raw feature distance, so
        // it passes even when a cardinal centroid collapses onto center once
        // projected onto the solver's own axes. Without this gate a fit can be
        // saved that the solver rejects on every frame, leaving the app armed
        // but permanently unable to draw a reticle or fire.
        if let axis = fitted.directionalBasis?.degenerateAxis {
            failureReason = "Calibration could not separate \(axis) aim. Exaggerate the \(axis) poses and try again."
            return nil
        }
        guard fitted.producesUsableAim else {
            failureReason = "Calibration could not fit a stable aim mapping. Please try again."
            return nil
        }
        return fitted
    }

    private func featureDistance(_ lhs: VisionAimFeature, _ rhs: VisionAimFeature) -> Double {
        let pairs = zip(lhs.components, rhs.components)
        return sqrt(pairs.reduce(0) { $0 + pow($1.0 - $1.1, 2) } / Double(lhs.components.count))
    }

    private func centroid(of samples: [VisionAimFeature]) -> VisionAimFeature? {
        guard !samples.isEmpty else { return nil }
        let values = samples.map(\.components)
        let mean = (0..<values[0].count).map { column in
            values.reduce(0) { $0 + $1[column] } / Double(values.count)
        }
        return VisionAimFeature(
            tipX: mean[0], tipY: mean[1],
            pipX: mean[2], pipY: mean[3],
            dipX: mean[4], dipY: mean[5],
            projectedLength: mean[6]
        )
    }

    private func clusterRMS(_ samples: [VisionAimFeature]) -> Double {
        guard let center = centroid(of: samples), !samples.isEmpty else { return .infinity }
        return sqrt(samples.reduce(0) { $0 + pow(featureDistance($1, center), 2) } / Double(samples.count))
    }
}

final class VisionAimCalibrationStore: @unchecked Sendable {
    private let defaults: UserDefaults
    init(defaults: UserDefaults = .standard) { self.defaults = defaults }

    func calibration(for variation: FingerGunVariation, cameraIdentifier: String) -> VisionAimCalibration? {
        calibration(cameraIdentifier: cameraIdentifier)
    }

    func calibration(cameraIdentifier: String) -> VisionAimCalibration? {
        guard let data = defaults.data(forKey: key(cameraIdentifier)),
              let calibration = try? JSONDecoder().decode(VisionAimCalibration.self, from: data),
              calibration.modelVersion == VisionAimCalibration.modelVersion else { return nil }
        // A calibration saved before the degenerate-axis gate existed can be
        // decodable yet unusable by the solver. Report it as missing so the UI
        // asks for a recalibration instead of leaving aim silently dead.
        guard calibration.producesUsableAim else { return nil }
        return calibration
    }

    func save(_ calibration: VisionAimCalibration) {
        guard let data = try? JSONEncoder().encode(calibration) else { return }
        defaults.set(data, forKey: key(calibration.cameraIdentifier))
    }

    func reset(cameraIdentifier: String) {
        defaults.removeObject(forKey: key(cameraIdentifier))
    }

    private func key(_ camera: String) -> String {
        "vision-aim-calibration.\(VisionAimCalibration.modelVersion).\(camera)"
    }
}

/// Directional axis geometry the reticle solver derives from the five stored
/// calibration centroids. The axes come from the right/left and up/down
/// centroid pairs, and each cardinal anchor is that centroid's coordinate in
/// the resulting basis relative to center.
///
/// This depends only on the calibration, never on the live frame: if the
/// anchors collapse, the solver can never produce a reticle for any hand pose.
/// Both the collector and the store consult `isUsable` so a degenerate fit is
/// rejected up front instead of silently disabling aim forever.
struct VisionAimDirectionalBasis: Equatable, Sendable {
    /// Minimum distance a cardinal anchor must sit from center along its own
    /// axis for the normalized axis mapping to have a usable sign and span.
    static let minimumAnchorSeparation = 0.05

    let center: [Double]
    let horizontal: [Double]
    let vertical: [Double]
    let determinant: Double
    let leftAnchor: Double
    let rightAnchor: Double
    let upAnchor: Double
    let downAnchor: Double

    init?(standardizedCentroids centroids: [[Double]]) {
        guard centroids.count == 5 else { return nil }
        let centerCentroid = centroids[0]
        let left = centroids[1]
        let right = centroids[2]
        let up = centroids[3]
        let down = centroids[4]
        let horizontalAxis = zip(right, left).map { ($0.0 - $0.1) / 2 }
        let verticalAxis = zip(up, down).map { ($0.0 - $0.1) / 2 }
        let hh = dot(horizontalAxis, horizontalAxis)
        let hv = dot(horizontalAxis, verticalAxis)
        let vv = dot(verticalAxis, verticalAxis)
        let axisDeterminant = hh * vv - hv * hv
        guard axisDeterminant > 0.000_001 else { return nil }
        func axisCoefficients(_ vector: [Double]) -> (horizontal: Double, vertical: Double) {
            let dh = dot(vector, horizontalAxis)
            let dv = dot(vector, verticalAxis)
            return ((dh * vv - dv * hv) / axisDeterminant, (dv * hh - dh * hv) / axisDeterminant)
        }
        center = centerCentroid
        horizontal = horizontalAxis
        vertical = verticalAxis
        determinant = axisDeterminant
        leftAnchor = axisCoefficients(zip(left, centerCentroid).map { $0.0 - $0.1 }).horizontal
        rightAnchor = axisCoefficients(zip(right, centerCentroid).map { $0.0 - $0.1 }).horizontal
        upAnchor = axisCoefficients(zip(up, centerCentroid).map { $0.0 - $0.1 }).vertical
        downAnchor = axisCoefficients(zip(down, centerCentroid).map { $0.0 - $0.1 }).vertical
    }

    func coefficients(forOffset offset: [Double]) -> (horizontal: Double, vertical: Double) {
        let hh = dot(horizontal, horizontal)
        let hv = dot(horizontal, vertical)
        let vv = dot(vertical, vertical)
        let dh = dot(offset, horizontal)
        let dv = dot(offset, vertical)
        return ((dh * vv - dv * hv) / determinant, (dv * hh - dh * hv) / determinant)
    }

    var isUsable: Bool { degenerateAxis == nil }

    /// Names the axis whose anchors collapsed, for an actionable calibration
    /// failure message. `nil` when the basis is usable.
    var degenerateAxis: String? {
        let minimum = Self.minimumAnchorSeparation
        if !(leftAnchor < -minimum && rightAnchor > minimum) { return "left and right" }
        if !(downAnchor < -minimum && upAnchor > minimum) { return "up and down" }
        return nil
    }
}

extension VisionAimCalibration {
    /// Standardizes a feature into the direction-template space the stored
    /// centroids live in. `nil` when the calibration predates template data or
    /// its dimensions disagree with the feature.
    func standardizedDirection(_ feature: VisionAimFeature) -> [Double]? {
        guard let templateMeans, let templateScales,
              templateMeans.count == feature.directionComponents.count,
              templateMeans.count == templateScales.count else { return nil }
        return zip(feature.directionComponents, zip(templateMeans, templateScales)).map {
            ($0.0 - $0.1.0) / max($0.1.1, 0.001)
        }
    }

    var directionalBasis: VisionAimDirectionalBasis? {
        guard let zoneCentroids, zoneCentroids.count == 5 else { return nil }
        let standardized = zoneCentroids.compactMap { standardizedDirection($0) }
        guard standardized.count == zoneCentroids.count else { return nil }
        return VisionAimDirectionalBasis(standardizedCentroids: standardized)
    }

    /// False when the stored geometry can never yield a reticle, whatever the
    /// hand does. Treated as "not calibrated" so the user is prompted to
    /// recalibrate instead of aiming into a silently dead solver.
    var producesUsableAim: Bool { directionalBasis?.isUsable ?? false }
}

struct VisionAimSolver: Sendable {
    let tuning: GestureTuning
    private var xFilter: OneEuroFilter
    private var yFilter: OneEuroFilter
    private var lastRaw: (point: CGPoint, timestamp: TimeInterval)?
    private var quantizer: DirectionalAimQuantizer

    init(tuning: GestureTuning = .default) {
        self.tuning = tuning
        xFilter = OneEuroFilter(tuning: tuning)
        yFilter = OneEuroFilter(tuning: tuning)
        quantizer = DirectionalAimQuantizer(tuning: tuning)
    }

    mutating func solve(
        observation: VisionFingerGunObservation,
        calibration: VisionAimCalibration,
        timestamp: TimeInterval
    ) -> AimSolution? {
        guard observation.aimFeature.components.count == calibration.featureMeans.count,
              calibration.featureMeans.count == calibration.featureScales.count,
              calibration.coefficientsX.count == calibration.featureMeans.count + 1,
              calibration.coefficientsY.count == calibration.featureMeans.count + 1 else { return nil }
        guard let feature = calibration.standardizedDirection(observation.aimFeature),
              let basis = calibration.directionalBasis,
              basis.isUsable else { return nil }
        let offset = zip(feature, basis.center).map { $0.0 - $0.1 }
        let coefficients = basis.coefficients(forOffset: offset)
        let leftAnchor = basis.leftAnchor
        let rightAnchor = basis.rightAnchor
        let upAnchor = basis.upAnchor
        let downAnchor = basis.downAnchor

        let reconstructed = zip(basis.horizontal, basis.vertical).map {
            $0.0 * coefficients.horizontal + $0.1 * coefficients.vertical
        }
        let residual = sqrt(zip(offset, reconstructed).reduce(0) {
            $0 + pow($1.0 - $1.1, 2)
        } / Double(max(offset.count, 1)))
        guard residual <= tuning.visionMaximumTemplateDistance else { return nil }

        func normalizedAxis(_ value: Double, negative: Double, positive: Double) -> Double {
            if value >= 0 { return value / max(positive, 0.05) }
            return value / max(abs(negative), 0.05)
        }
        let normalizedHorizontal = normalizedAxis(coefficients.horizontal, negative: leftAnchor, positive: rightAnchor)
        let normalizedVertical = normalizedAxis(coefficients.vertical, negative: downAnchor, positive: upAnchor)
        let regressionInput = [1] + zip(
            observation.aimFeature.components,
            zip(calibration.featureMeans, calibration.featureScales)
        ).map { value, pair in
            (value - pair.0) / max(pair.1, 0.001)
        }
        let raw = clampedToViewport(CGPoint(
            x: dot(regressionInput, calibration.coefficientsX),
            y: dot(regressionInput, calibration.coefficientsY)
        ))
        if let lastRaw {
            let dt = max(timestamp - lastRaw.timestamp, 0.001)
            let velocity = hypot(raw.x - lastRaw.point.x, raw.y - lastRaw.point.y) / dt
            if velocity > tuning.visionMaximumReticleVelocity { return nil }
        }
        lastRaw = (raw, timestamp)
        let continuous = clampedToViewport(CGPoint(
            x: xFilter.filter(Double(raw.x), timestamp: timestamp, confidence: observation.confidence),
            y: yFilter.filter(Double(raw.y), timestamp: timestamp, confidence: observation.confidence)
        ))
        let horizontalZone = normalizedHorizontal < -0.5 ? -1 : normalizedHorizontal > 0.5 ? 1 : 0
        let verticalZone = normalizedVertical < -0.5 ? -1 : normalizedVertical > 0.5 ? 1 : 0
        let targetZone: AimDirectionZone = switch (horizontalZone, verticalZone) {
        case (0, 0): .center
        case (-1, 0): .left
        case (1, 0): .right
        case (0, 1): .up
        case (0, -1): .down
        case (-1, 1): .upLeft
        case (1, 1): .upRight
        case (-1, -1): .downLeft
        case (1, -1): .downRight
        default: .center
        }
        let filtered = quantizer.filter(targetZone)
        return AimSolution(
            rawYaw: Double(raw.x - 0.5),
            rawPitch: Double(raw.y - 0.5),
            filteredYaw: Double(filtered.x - 0.5),
            filteredPitch: Double(filtered.y - 0.5),
            rawScreenPoint: continuous,
            screenPoint: filtered,
            confidence: observation.confidence,
            valid: true
        )
    }

    mutating func reset() {
        xFilter.reset()
        yFilter.reset()
        lastRaw = nil
        quantizer.reset()
    }
}

struct DirectionalAimQuantizer: Sendable {
    let tuning: GestureTuning
    private(set) var zone: AimDirectionZone = .center
    private var candidate: AimDirectionZone?
    private var candidateFrames = 0

    init(tuning: GestureTuning = .default) { self.tuning = tuning }

    mutating func filter(_ point: CGPoint) -> CGPoint {
        filter(zoneFor(point))
    }

    mutating func filter(_ next: AimDirectionZone) -> CGPoint {
        if next == zone {
            candidate = nil
            candidateFrames = 0
        } else if next == candidate {
            candidateFrames += 1
            if candidateFrames >= max(tuning.visionDirectionStabilizationFrames, 1) {
                zone = next
                candidate = nil
                candidateFrames = 0
            }
        } else {
            candidate = next
            candidateFrames = 1
        }
        return zone.point
    }

    mutating func reset() {
        zone = .center
        candidate = nil
        candidateFrames = 0
    }

    private func zoneFor(_ point: CGPoint) -> AimDirectionZone {
        let horizontal = axis(point.x)
        let vertical = axis(point.y)
        switch (horizontal, vertical) {
        case (0, 0): return .center
        case (-1, 0): return .left
        case (1, 0): return .right
        case (0, 1): return .up
        case (0, -1): return .down
        case (-1, 1): return .upLeft
        case (1, 1): return .upRight
        case (-1, -1): return .downLeft
        case (1, -1): return .downRight
        default: return .center
        }
    }

    private func axis(_ value: CGFloat) -> Int {
        if value < tuning.visionDirectionLowThreshold { return -1 }
        if value > tuning.visionDirectionHighThreshold { return 1 }
        return 0
    }
}

private func ridgeRegression(design: [[Double]], targets: [Double], lambda: Double) -> [Double]? {
    guard let width = design.first?.count, width > 0, design.count == targets.count else { return nil }
    var matrix = Array(repeating: Array(repeating: 0.0, count: width), count: width)
    var vector = Array(repeating: 0.0, count: width)
    for row in design.indices {
        for i in 0..<width {
            vector[i] += design[row][i] * targets[row]
            for j in 0..<width { matrix[i][j] += design[row][i] * design[row][j] }
        }
    }
    for i in 1..<width { matrix[i][i] += lambda }
    return solveLinearSystem(matrix, vector)
}

private func solveLinearSystem(_ input: [[Double]], _ values: [Double]) -> [Double]? {
    var matrix = input
    var result = values
    let count = result.count
    guard matrix.count == count, matrix.allSatisfy({ $0.count == count }) else { return nil }
    for pivot in 0..<count {
        let best = (pivot..<count).max { abs(matrix[$0][pivot]) < abs(matrix[$1][pivot]) }!
        guard abs(matrix[best][pivot]) > 0.000_000_1 else { return nil }
        if best != pivot { matrix.swapAt(best, pivot); result.swapAt(best, pivot) }
        let divisor = matrix[pivot][pivot]
        for column in pivot..<count { matrix[pivot][column] /= divisor }
        result[pivot] /= divisor
        for row in 0..<count where row != pivot {
            let factor = matrix[row][pivot]
            guard factor != 0 else { continue }
            for column in pivot..<count { matrix[row][column] -= factor * matrix[pivot][column] }
            result[row] -= factor * result[pivot]
        }
    }
    return result
}

private func dot(_ lhs: [Double], _ rhs: [Double]) -> Double {
    zip(lhs, rhs).reduce(0) { $0 + $1.0 * $1.1 }
}
