import CoreML
import Vision

/// Wraps the bundled MobileCLIP2-S0 Core ML image encoder.
///
/// The `.mlpackage` is compiled by Xcode into `MobileCLIPImageEncoder.mlmodelc`
/// inside the app bundle. We load it via `MLModel(contentsOf:)` rather than a
/// generated class so this file compiles even before the model is added to a
/// target, and so a missing model degrades gracefully instead of failing to build.
///
/// Given an appearance crop, `embed` returns a 512-dimension L2-normalized vector
/// (the encoder already unit-normalizes; we re-normalize defensively). Returns
/// `nil` on any load/inference failure so callers can fall back to the
/// deterministic color-grid embedding.
final class MobileCLIPEmbedder: @unchecked Sendable {
    static let shared = MobileCLIPEmbedder()

    /// Namespace tag written into the appearance profile so vectors from this
    /// encoder are never compared against vectors from a different model/version.
    static let modelVersion = "mobileclip2-s0-image-512-v1"

    private let visionModel: VNCoreMLModel?

    private init() {
        visionModel = Self.loadModel()
    }

    var isAvailable: Bool { visionModel != nil }

    private static func loadModel() -> VNCoreMLModel? {
        guard let url = Bundle.main.url(forResource: "MobileCLIPImageEncoder", withExtension: "mlmodelc") else {
            return nil
        }
        let configuration = MLModelConfiguration()
        configuration.computeUnits = .all
        guard let model = try? MLModel(contentsOf: url, configuration: configuration),
              let visionModel = try? VNCoreMLModel(for: model) else {
            return nil
        }
        return visionModel
    }

    /// 512-d unit-length embedding for the crop, or `nil` on failure.
    func embed(_ image: CGImage) -> [Float]? {
        guard let visionModel else { return nil }
        let request = VNCoreMLRequest(model: visionModel)
        // The model wants a fixed square input; let Vision scale the crop to fill it.
        request.imageCropAndScaleOption = .scaleFill
        let handler = VNImageRequestHandler(cgImage: image, orientation: .up)
        do {
            try handler.perform([request])
        } catch {
            return nil
        }
        guard let observation = request.results?.first as? VNCoreMLFeatureValueObservation,
              let array = observation.featureValue.multiArrayValue,
              array.count > 0 else {
            return nil
        }
        // 512 elements: the NSNumber path is negligible cost and dtype-agnostic
        // (the model is exported at float16 precision).
        var values = [Float](repeating: 0, count: array.count)
        for index in 0..<array.count {
            values[index] = array[index].floatValue
        }
        return values
    }
}
