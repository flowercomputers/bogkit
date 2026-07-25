import CoreGraphics
import Foundation

enum AimingMode: String, Codable, Equatable, Sendable {
    case unscoped = "UNSCOPED"
    case sights = "SIGHTS"
}

/// Per-frame proximity state, recorded for diagnostics so a rejected entry can
/// be explained from a landmark recording alone.
struct ScopeProximityDiagnostic: Codable, Equatable, Sendable {
    let span: Double
    let baseline: Double
    let ratio: Double
    let progress: Double
    let warm: Bool
}

/// Apparent size of the knuckle line in the image, in knuckle-width units.
///
/// Two corrections matter. Vision reports normalised coordinates, so a purely
/// horizontal span and a purely vertical span of the same physical length carry
/// different normalised magnitudes; without an aspect correction, simply
/// rolling the hand would change the measurement by the frame's aspect ratio.
/// Dividing by the live zoom factor then keeps the signal in one frame of
/// reference while the camera ramps into its scoped zoom.
///
/// Only the four MCP knuckles are used. Two landmark groups were rejected on
/// evidence from device recordings:
///
/// - The index barrel foreshortens badly when it points away from the lens,
///   which is exactly the pose sights are entered from.
/// - The wrist is worse than useless here. Measured against knuckle width over
///   confident frames, wrist spans varied by an IQR of 0.29-0.34 (wrist flexion
///   changes the distance, and the landmark wanders) against 0.02-0.09 for
///   knuckle-to-knuckle spans. It is also the first landmark to be lost as the
///   hand approaches, because it leaves the bottom of the frame: it carried a
///   median confidence of 0.14 on frames where measurement failed.
///
/// Each pair contributes an independent estimate of one quantity — apparent
/// hand scale — by dividing its measured length by its nominal proportion of
/// knuckle width. Estimates are combined by median, and any that disagrees
/// badly with that median is discarded, so one bad landmark cannot move the
/// result. Because every pair estimates the same quantity, a partial pair set
/// stays comparable with the baseline instead of silently measuring something
/// else, which is what lets the measurement survive landmark dropout at all.
enum HandProximityMeasure {
    /// Nominal length of each palm span as a fraction of index-to-little
    /// knuckle width, taken as the median over high-confidence device frames.
    static let pairs: [(joints: (LandmarkJoint, LandmarkJoint), proportion: Double)] = [
        ((.indexMCP, .littleMCP), 1.0),
        ((.indexMCP, .ringMCP), 0.668),
        ((.middleMCP, .littleMCP), 0.664),
        ((.indexMCP, .middleMCP), 0.351),
        ((.middleMCP, .ringMCP), 0.332),
        ((.ringMCP, .littleMCP), 0.351)
    ]

    static func span(
        of hand: TrackedHand,
        imageSize: CGSize,
        zoomFactor: Double,
        minimumConfidence: Float,
        minimumPairs: Int = GestureTuning.default.scopeMinimumProximityPairs,
        maximumDisagreement: Double = GestureTuning.default.scopeProximityPairDisagreement
    ) -> Double? {
        let aspect = imageSize.height > 0 ? Double(imageSize.width / imageSize.height) : 1
        var estimates: [Double] = []
        estimates.reserveCapacity(pairs.count)
        for (joints, proportion) in pairs {
            guard let a = hand[image: joints.0], a.confidence >= minimumConfidence,
                  let b = hand[image: joints.1], b.confidence >= minimumConfidence else { continue }
            let dx = Double(a.location.x - b.location.x) * aspect
            let dy = Double(a.location.y - b.location.y)
            let length = (dx * dx + dy * dy).squareRoot()
            guard length > 0, proportion > 0 else { continue }
            estimates.append(length / proportion)
        }
        guard estimates.count >= max(minimumPairs, 1), var scale = median(estimates) else { return nil }
        if estimates.count >= 3 {
            let agreeing = estimates.filter { abs($0 - scale) / scale <= maximumDisagreement }
            guard agreeing.count >= max(minimumPairs, 1), let refined = median(agreeing) else { return nil }
            scale = refined
        }
        let corrected = scale / max(zoomFactor, 0.001)
        return corrected.isFinite && corrected > 0 ? corrected : nil
    }

    private static func median(_ values: [Double]) -> Double? {
        guard !values.isEmpty else { return nil }
        let sorted = values.sorted()
        let middle = sorted.count / 2
        if sorted.count.isMultiple(of: 2) {
            return (sorted[middle - 1] + sorted[middle]) / 2
        }
        return sorted[middle]
    }
}

/// Running reference for how large the hand appears when it is *not* being
/// pulled toward the camera: a low percentile of the recent unscoped spans.
///
/// This replaced an exponential average that stopped updating whenever the
/// current sample was elevated. That freeze was a one-way latch. Because any
/// sample above the freeze ratio was ignored, the reference could only ever
/// move *down*, so one low reading — a distant or half-tracked hand — pinned it
/// there permanently and every subsequent frame read as "close". On a device
/// recording the baseline sat unchanged for the first 16 seconds and sights were
/// engaged for 81% of the session.
///
/// A percentile over a sliding window fixes that by construction: it is
/// recomputed from scratch each frame, so it can move in both directions and
/// cannot latch. It still resists absorbing the gesture, because a fraction of
/// a second of approach is a small minority of a multi-second window and a low
/// percentile ignores it — and because a percentile discards outliers, a single
/// bad span cannot drag the reference either.
///
/// Samples are only taken while unscoped. That is what keeps a held scoped pose
/// from drifting back under the exit threshold on its own, and it is now the
/// *only* freeze rule, which is why the latch is gone.
struct ScopeProximityBaseline: Sendable {
    private var window: [(timestamp: TimeInterval, span: Double)] = []
    private let tuning: GestureTuning

    init(tuning: GestureTuning = .default) { self.tuning = tuning }

    var value: Double? {
        guard isWarm else { return nil }
        let spans = window.map(\.span).sorted()
        let index = min(Int(tuning.scopeBaselinePercentile * Double(spans.count)), spans.count - 1)
        return spans[max(index, 0)]
    }

    var samples: Int { window.count }

    /// Requires both a sample count and a spread of time, so a burst of frames
    /// in one position cannot pass for a settled reference.
    var isWarm: Bool {
        guard window.count >= tuning.scopeBaselineMinimumSamples,
              let first = window.first, let last = window.last else { return false }
        return last.timestamp - first.timestamp >= tuning.scopeBaselineMinimumSeconds
    }

    mutating func update(span: Double, timestamp: TimeInterval) {
        // A replayed recording can step backwards; drop anything ahead of the
        // new timestamp so the window stays ordered and bounded.
        if let last = window.last, timestamp < last.timestamp {
            window.removeAll { $0.timestamp > timestamp }
        }
        window.append((timestamp, span))
        let cutoff = timestamp - tuning.scopeBaselineWindowSeconds
        window.removeAll { $0.timestamp < cutoff }
    }

    mutating func reset() {
        window.removeAll(keepingCapacity: true)
    }
}

/// Enters sights when the finger gun is drawn toward the phone, the way a real
/// weapon comes up to the eye.
///
/// The signal is one monotonic scalar with hysteresis rather than a conjunction
/// of absolute in-frame position gates, so a single marginal frame cannot block
/// entry and the player gets a progress value to aim at. Nothing here consumes
/// the thumb: pulling the hand closer does not change thumb geometry, so entry
/// cannot be mistaken for a trigger pull.
struct ScopeModeDetector: Sendable {
    private(set) var mode: AimingMode = .unscoped
    private(set) var diagnostic: ScopeProximityDiagnostic?

    private let tuning: GestureTuning
    private var baseline: ScopeProximityBaseline
    private var entryCandidateSince: TimeInterval?
    private var entryLostSince: TimeInterval?
    private var exitCandidateSince: TimeInterval?
    private var lastMeasurementTimestamp: TimeInterval?

    init(tuning: GestureTuning = .default) {
        self.tuning = tuning
        baseline = ScopeProximityBaseline(tuning: tuning)
    }

    /// - Parameters:
    ///   - entryEligible: whether the frame carries a usable finger-gun-like
    ///     pose. Proximity alone would also fire for an open palm held near the
    ///     lens.
    ///   - zoomFactor: the camera's live zoom, so the scoped ramp does not
    ///     inflate the measured span.
    mutating func update(
        hand: TrackedHand?,
        imageSize: CGSize,
        timestamp: TimeInterval,
        zoomFactor: Double = 1,
        enabled: Bool = true,
        entryEligible: Bool = true
    ) -> AimingMode {
        guard enabled else {
            reset()
            return mode
        }

        let span = hand.flatMap {
            HandProximityMeasure.span(
                of: $0,
                imageSize: imageSize,
                zoomFactor: zoomFactor,
                minimumConfidence: tuning.visionMinimumJointConfidence,
                minimumPairs: tuning.scopeMinimumProximityPairs,
                maximumDisagreement: tuning.scopeProximityPairDisagreement
            )
        }

        if mode == .unscoped, let span {
            baseline.update(span: span, timestamp: timestamp)
        }

        let rawRatio: Double? = if let span, let reference = baseline.value, reference > 0 {
            span / reference
        } else {
            nil
        }
        // A ratio far beyond any real reach indicates broken landmarks rather
        // than a very close hand, and must not be allowed to force entry.
        let ratio = rawRatio.flatMap { $0 <= tuning.scopeMaximumProximityRatio ? $0 : nil }
        if ratio != nil { lastMeasurementTimestamp = timestamp }

        let progress: Double
        if mode == .sights {
            progress = 1
        } else if let ratio, baseline.isWarm, entryEligible {
            let range = max(tuning.scopeEnterProximityRatio - 1, 0.001)
            progress = min(max((ratio - 1) / range, 0), 1)
        } else {
            progress = 0
        }
        diagnostic = ScopeProximityDiagnostic(
            span: span ?? 0,
            baseline: baseline.value ?? 0,
            ratio: ratio ?? 0,
            progress: progress,
            warm: baseline.isWarm
        )

        switch mode {
        case .unscoped:
            exitCandidateSince = nil
            let close = entryEligible && baseline.isWarm &&
                (ratio ?? 0) >= tuning.scopeEnterProximityRatio
            guard close else {
                // Brief classification or landmark dropouts should not restart
                // an approach that is otherwise still in progress.
                if entryCandidateSince != nil {
                    if let entryLostSince {
                        if timestamp < entryLostSince ||
                            timestamp - entryLostSince > tuning.scopeEntryLossGraceSeconds {
                            entryCandidateSince = nil
                            self.entryLostSince = nil
                        }
                    } else {
                        entryLostSince = timestamp
                    }
                } else {
                    entryLostSince = nil
                }
                return mode
            }
            entryLostSince = nil
            if let start = entryCandidateSince, timestamp < start {
                entryCandidateSince = timestamp
            } else if entryCandidateSince == nil {
                entryCandidateSince = timestamp
            }
            if let start = entryCandidateSince, timestamp - start >= tuning.scopeEntrySeconds {
                mode = .sights
                entryCandidateSince = nil
                // The baseline is deliberately kept, not cleared: it describes
                // the player's relaxed hold and is what the exit test compares
                // against when they lower the gun again.
            }

        case .sights:
            entryCandidateSince = nil
            entryLostSince = nil
            let released: Bool
            if let ratio {
                released = ratio <= tuning.scopeExitProximityRatio
            } else if let lastMeasurementTimestamp,
                      timestamp >= lastMeasurementTimestamp,
                      timestamp - lastMeasurementTimestamp <= tuning.scopeRetentionLossSeconds {
                // Measurement gap inside the grace window: hold the mode so a
                // dropped frame cannot drop the player out of sights.
                released = false
            } else {
                released = true
            }
            guard released else {
                exitCandidateSince = nil
                return mode
            }
            if let start = exitCandidateSince {
                if timestamp < start {
                    exitCandidateSince = timestamp
                } else if timestamp - start >= tuning.scopeExitSeconds {
                    mode = .unscoped
                    exitCandidateSince = nil
                }
            } else {
                exitCandidateSince = timestamp
            }
        }

        return mode
    }

    var entryProgress: Double { diagnostic?.progress ?? 0 }

    mutating func reset() {
        mode = .unscoped
        diagnostic = nil
        baseline.reset()
        entryCandidateSince = nil
        entryLostSince = nil
        exitCandidateSince = nil
        lastMeasurementTimestamp = nil
    }
}

enum AimingModePolicy {
    static let sightsPoint = CGPoint(x: 0.5, y: 0.5)

    static func triggerObservation(
        mode: AimingMode,
        observation: VisionFingerGunObservation?,
        hasCalibration: Bool,
        hasAimSolution: Bool = true
    ) -> VisionFingerGunObservation? {
        switch mode {
        case .unscoped: return hasCalibration && hasAimSolution ? observation : nil
        case .sights: return observation
        }
    }

    static func visibleAim(mode: AimingMode, solution: AimSolution?) -> AimSolution? {
        mode == .sights ? nil : solution
    }

    static func flashPoint(mode: AimingMode, aim: AimSolution?) -> CGPoint? {
        switch mode {
        case .unscoped: return aim?.screenPoint
        case .sights: return sightsPoint
        }
    }

    /// Where a shot resolves for gameplay targeting. Sights always resolves to
    /// the fixed centre reticle, so a scoped shot is still evaluated against
    /// the opponent mask exactly like an unscoped one.
    static func gameplayPoint(mode: AimingMode, aim: AimSolution?) -> CGPoint? {
        switch mode {
        case .unscoped: return aim?.gameplayScreenPoint
        case .sights: return sightsPoint
        }
    }
}

enum ScopePosePolicy {
    /// Proximity decides *when* to scope; this decides whether the frame holds
    /// a plausible finger gun at all, so an open palm brought near the lens
    /// cannot enter sights.
    static func isEntryEligible(
        _ analysis: VisionFingerGunAnalysis,
        hand: TrackedHand,
        tuning: GestureTuning = .default
    ) -> Bool {
        observation(from: analysis, hand: hand, tuning: tuning) != nil
    }

    static func observation(
        from analysis: VisionFingerGunAnalysis,
        hand: TrackedHand,
        tuning: GestureTuning = .default
    ) -> VisionFingerGunObservation? {
        if let observation = analysis.observation { return observation }

        // An index aimed nearly along the camera axis is heavily foreshortened
        // in Vision's 2D landmarks and is commonly classified CURLED. Sights can
        // tolerate that one failure because its target is camera center, while
        // the other fingers still protect against open-palm false positives.
        guard hand.confidence >= tuning.visionMinimumJointConfidence,
              let feature = analysis.aimFeature,
              analysis.ringState != .straight,
              analysis.littleState != .straight,
              analysis.thumbState != .ambiguous,
              let indexTip = hand[image: .indexTip],
              indexTip.confidence >= tuning.visionMinimumJointConfidence else { return nil }

        // When the hand points almost directly into the camera, the middle
        // finger frequently becomes ambiguous even though the index landmarks
        // still form a usable single-barrel scope pose. Preserve a confident
        // double-barrel label, otherwise fall back to the index-only variation.
        let variation = analysis.calibrationVariation ?? .singleBarrel

        var muzzlePoint = indexTip.location
        if variation == .doubleBarrel,
           let middleTip = hand[image: .middleTip],
           middleTip.confidence >= tuning.visionMinimumJointConfidence {
            muzzlePoint = CGPoint(
                x: (indexTip.location.x + middleTip.location.x) / 2,
                y: (indexTip.location.y + middleTip.location.y) / 2
            )
        }

        return VisionFingerGunObservation(
            variation: variation,
            muzzlePoint: muzzlePoint,
            aimFeature: feature,
            confidence: hand.confidence,
            poseMargin: 0,
            thumbState: analysis.thumbState
        )
    }
}
