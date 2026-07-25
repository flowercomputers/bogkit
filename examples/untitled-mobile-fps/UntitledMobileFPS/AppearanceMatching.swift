import Foundation

struct ImageAppearanceAttributes: Codable, Equatable, Sendable {
    var dominantColors: [String] = []
    var upperGarment: String?
    var lowerGarment: String?
    var outerwear: String?
    var footwear: String?
    var headwear: String?
    var accessory: String?
}

enum PerceptualColorClassifier {
    static func name(red: Double, green: Double, blue: Double) -> String {
        let red = min(max(red, 0), 1)
        let green = min(max(green, 0), 1)
        let blue = min(max(blue, 0), 1)
        let maximum = max(red, green, blue)
        let minimum = min(red, green, blue)
        let chroma = maximum - minimum
        let saturation = maximum > 0 ? chroma / maximum : 0
        let luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue

        if luminance < 0.23 { return "black" }
        if saturation < 0.23 {
            if luminance >= 0.68 { return "white" }
            if luminance >= 0.52 { return "light gray" }
            return "gray"
        }

        let hue: Double
        if chroma == 0 {
            hue = 0
        } else if maximum == red {
            hue = 60 * ((green - blue) / chroma).truncatingRemainder(dividingBy: 6)
        } else if maximum == green {
            hue = 60 * ((blue - red) / chroma + 2)
        } else {
            hue = 60 * ((red - green) / chroma + 4)
        }
        let normalizedHue = hue < 0 ? hue + 360 : hue

        if saturation < 0.30, luminance >= 0.72 { return "white" }
        switch normalizedHue {
        case 15..<45: return "orange"
        case 45..<75: return saturation < 0.45 ? "tan" : "yellow"
        case 75..<170: return "green"
        case 170..<260: return "blue"
        case 260..<330: return "purple"
        default: return "red"
        }
    }
}

enum AutomaticAppearanceDescriber {
    static func describe(_ attributes: ImageAppearanceAttributes) -> String {
        var details: [String] = []
        if let outerwear = cleaned(attributes.outerwear) {
            details.append(outerwear)
        }
        if let upper = cleaned(attributes.upperGarment) {
            details.append(upper)
        }
        if let lower = cleaned(attributes.lowerGarment) {
            details.append(lower)
        }
        if let footwear = cleaned(attributes.footwear) {
            details.append(footwear)
        }
        if let headwear = cleaned(attributes.headwear) {
            details.append(headwear)
        }
        if let accessory = cleaned(attributes.accessory) {
            details.append(accessory)
        }

        let colors = attributes.dominantColors
            .compactMap(cleaned)
            .reduce(into: [String]()) { result, color in
                if !result.contains(color) { result.append(color) }
            }
            .prefix(3)

        let outfit = details.isEmpty ? "an outfit" : details.joined(separator: ", ")
        guard !colors.isEmpty else { return "Person wearing \(outfit)." }
        return "Person wearing \(outfit), mainly \(colors.joined(separator: ", "))."
    }

    private static func cleaned(_ value: String?) -> String? {
        guard let value else { return nil }
        let cleaned = value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return cleaned.isEmpty ? nil : cleaned
    }
}

struct AppearanceSignalScores: Equatable, Sendable {
    var wholeBody: Float?
    var outfitText: Float?
    var upperBody: Float?
    var lowerBody: Float?
    var headAccessory: Float?
    var silhouette: Float?
    var face: Float?
    var bodyGeometry: Float?
}

enum AppearanceScoringScope: Equatable, Sendable {
    case globalSearch
    case activeMatch
}

/// A single modality's contribution to a fused appearance score, retained so the
/// on-device HUD (and the server inspector, when relayed) can show *why* a lock
/// scored the way it did rather than a single opaque number.
struct AppearanceSignalContribution: Equatable, Sendable {
    let name: String
    let value: Float
    let weight: Float
    /// `true` for outfit signals that are allowed to carry the decision;
    /// `false` for confirmatory signals (face/silhouette/head) that may only nudge it.
    let isDiscriminative: Bool
}

/// The full outcome of a fusion, including the two group aggregates and every
/// per-modality contribution. `fused` is the value the gate consumes.
struct AppearanceFusionBreakdown: Equatable, Sendable {
    var fused: Float
    var discriminative: Float?
    var confirmatory: Float?
    var contributions: [AppearanceSignalContribution]
}

/// Fuses per-modality appearance similarities into a single 0…1 lock confidence.
///
/// The prior implementation was a flat weighted mean, which let any single strong
/// modality carry a hit — in practice face and silhouette, the two signals that are
/// *reliably* high (every person is a person-shape; a frontal face matches strongly)
/// yet the *least* discriminative at phone-camera range. This version splits signals
/// into two roles and enforces that the outfit decides:
///
/// - **Discriminative** (whole-body, outfit text, upper/lower garment) sets the base
///   score *and* its ceiling.
/// - **Confirmatory** (silhouette, head accessory, face, body geometry) can only nudge
///   the base within ±`confirmatoryBand`, and on their own (no outfit visible) are
///   capped below the accept gate by `confirmatoryOnlyCap`.
///
/// Net guarantee: a perfect face + silhouette can never clear the gate unless the
/// outfit also agrees. Thresholds are deliberately centralized here for on-device tuning.
enum AppearanceScoreFusion {
    /// How far confirmatory agreement may move the outfit base, in either direction.
    static let confirmatoryBand: Float = 0.18
    /// Hard ceiling when no discriminative (outfit) signal is present at all, kept
    /// below GameplayTargetingTuning.minimumTargetScore (0.5) so confirmatory-only
    /// evidence can never fire a shot by itself.
    static let confirmatoryOnlyCap: Float = 0.48

    static func score(_ scores: AppearanceSignalScores, scope: AppearanceScoringScope) -> Float {
        breakdown(scores, scope: scope).fused
    }

    static func breakdown(
        _ scores: AppearanceSignalScores,
        scope: AppearanceScoringScope
    ) -> AppearanceFusionBreakdown {
        // (value, weight, isDiscriminative). Weights are relative *within* each group;
        // absent signals are dropped and the remaining weights renormalize.
        var signals: [(String, Float?, Float, Bool)] = [
            ("wholeBody", scores.wholeBody, 0.34, true),
            ("outfitText", scores.outfitText, 0.20, true),
            ("upperBody", scores.upperBody, 0.30, true),
            ("lowerBody", scores.lowerBody, 0.16, true),
            ("silhouette", scores.silhouette, 0.45, false),
            ("headAccessory", scores.headAccessory, 0.25, false)
        ]
        if scope == .activeMatch {
            // Face and body geometry are match-scoped and, crucially, confirmatory:
            // a strong face match may reinforce an outfit lock but can never create one.
            signals.append(("face", scores.face, 0.20, false))
            signals.append(("bodyGeometry", scores.bodyGeometry, 0.10, false))
        }

        let contributions = signals.compactMap { name, value, weight, discriminative
            -> AppearanceSignalContribution? in
            guard let value, value.isFinite else { return nil }
            return AppearanceSignalContribution(
                name: name,
                value: min(max(value, 0), 1),
                weight: weight,
                isDiscriminative: discriminative
            )
        }

        let discriminative = weightedMean(contributions.filter(\.isDiscriminative))
        let confirmatory = weightedMean(contributions.filter { !$0.isDiscriminative })

        let fused: Float
        switch (discriminative, confirmatory) {
        case (nil, nil):
            fused = 0
        case let (nil, .some(confirm)):
            fused = min(confirm, confirmatoryOnlyCap)
        case let (.some(base), nil):
            fused = base
        case let (.some(base), .some(confirm)):
            // confirm in [0,1] maps to a nudge in [-band, +band]; the outfit base is
            // both the anchor and the ceiling-plus-band, so confirmation only moves
            // the score at the margin.
            let nudge = (confirm - 0.5) * 2 * confirmatoryBand
            fused = min(max(base + nudge, 0), 1)
        }

        return AppearanceFusionBreakdown(
            fused: fused,
            discriminative: discriminative,
            confirmatory: confirmatory,
            contributions: contributions
        )
    }

    private static func weightedMean(_ contributions: [AppearanceSignalContribution]) -> Float? {
        let totalWeight = contributions.reduce(0) { $0 + $1.weight }
        guard totalWeight > 0 else { return nil }
        return contributions.reduce(0) { $0 + $1.value * $1.weight } / totalWeight
    }
}

enum EmbeddingMath {
    static func normalized(_ values: [Float], dimensions: Int = appearanceEmbeddingDimensions) -> [Float] {
        var result = Array(repeating: Float.zero, count: dimensions)
        for index in 0..<min(values.count, dimensions) where values[index].isFinite {
            result[index] = values[index]
        }
        let magnitude = sqrt(result.reduce(0) { $0 + $1 * $1 })
        guard magnitude > Float.ulpOfOne else { return result }
        return result.map { $0 / magnitude }
    }

    static func cosineSimilarity(_ lhs: [Float], _ rhs: [Float]) -> Float? {
        guard lhs.count == rhs.count, !lhs.isEmpty else { return nil }
        let lhs = normalized(lhs, dimensions: lhs.count)
        let rhs = normalized(rhs, dimensions: rhs.count)
        guard lhs.contains(where: { $0 != 0 }), rhs.contains(where: { $0 != 0 }) else { return nil }
        return zip(lhs, rhs).reduce(0) { $0 + $1.0 * $1.1 }
    }
}
