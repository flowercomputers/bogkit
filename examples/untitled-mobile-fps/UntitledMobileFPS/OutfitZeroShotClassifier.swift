import Foundation

struct OutfitLabelMatch: Equatable {
    let color: String
    let garment: String
}

/// Zero-shot outfit naming: cosine-matches a MobileCLIP image embedding of a
/// garment crop against precomputed text-label embeddings (`OutfitLabels.json`).
///
/// Color and garment type are marginalized independently — the chosen color is the
/// one whose best-fitting garment scores highest, and vice versa — which is more
/// stable than a single joint argmax over the full label grid. Labels are only
/// loaded when their model tag matches the bundled image encoder, so image and text
/// vectors always come from the same joint space.
final class OutfitZeroShotClassifier: @unchecked Sendable {
    static let shared = OutfitZeroShotClassifier()

    struct Label { let color: String; let garment: String; let vector: [Float] }

    private let tops: [Label]
    private let bottoms: [Label]

    var isAvailable: Bool { !tops.isEmpty && !bottoms.isEmpty }

    private init() {
        (tops, bottoms) = Self.load()
    }

    func classifyTop(_ embedding: [Float]) -> OutfitLabelMatch? { best(embedding, among: tops) }
    func classifyBottom(_ embedding: [Float]) -> OutfitLabelMatch? { best(embedding, among: bottoms) }

    private func best(_ embedding: [Float], among labels: [Label]) -> OutfitLabelMatch? {
        guard !labels.isEmpty, embedding.count == appearanceEmbeddingDimensions else { return nil }
        var colorScore: [String: Float] = [:]
        var garmentScore: [String: Float] = [:]
        for label in labels where label.vector.count == embedding.count {
            let similarity = Self.dot(embedding, label.vector)
            if similarity > (colorScore[label.color] ?? -.greatestFiniteMagnitude) {
                colorScore[label.color] = similarity
            }
            if similarity > (garmentScore[label.garment] ?? -.greatestFiniteMagnitude) {
                garmentScore[label.garment] = similarity
            }
        }
        guard let color = colorScore.max(by: { $0.value < $1.value })?.key,
              let garment = garmentScore.max(by: { $0.value < $1.value })?.key else { return nil }
        return OutfitLabelMatch(color: color, garment: garment)
    }

    private static func dot(_ lhs: [Float], _ rhs: [Float]) -> Float {
        var sum: Float = 0
        for index in lhs.indices { sum += lhs[index] * rhs[index] }
        return sum
    }

    private static func load() -> ([Label], [Label]) {
        guard let url = Bundle.main.url(forResource: "OutfitLabels", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let model = root["model"] as? String,
              model == MobileCLIPEmbedder.modelVersion,
              let dim = root["dim"] as? Int, dim == appearanceEmbeddingDimensions else {
            return ([], [])
        }
        func parse(_ key: String) -> [Label] {
            guard let array = root[key] as? [[String: Any]] else { return [] }
            return array.compactMap { item in
                guard let color = item["color"] as? String,
                      let garment = item["garment"] as? String,
                      let vector = item["vector"] as? [Double] else { return nil }
                return Label(color: color, garment: garment, vector: vector.map(Float.init))
            }
        }
        return (parse("tops"), parse("bottoms"))
    }
}
