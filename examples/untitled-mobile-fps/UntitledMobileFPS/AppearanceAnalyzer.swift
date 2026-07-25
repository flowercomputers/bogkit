import CoreImage
import UIKit
import Vision

struct AppearanceAnalysis: @unchecked Sendable {
    /// Reflects the encoder actually used: the MobileCLIP2-S0 Core ML model when
    /// it is bundled and loadable, otherwise the deterministic color-grid fallback.
    /// Written into the profile so ANNy never compares vectors across encoders.
    static var embeddingModel: String {
        MobileCLIPEmbedder.shared.isAvailable
            ? MobileCLIPEmbedder.modelVersion
            : "bogshot-color-grid-512-v2"
    }
    static let descriptorModel = "vision-full-body-descriptor-v2"

    let generatedDescription: String
    let wholeBodyEmbedding: [Float]
    let faceEmbeddings: [[Float]]
    let upperBodyEmbeddings: [[Float]]
    let lowerBodyEmbeddings: [[Float]]
    let headAccessoryEmbeddings: [[Float]]
    let silhouetteDescriptor: [Float]
    let briefingThumbnail: String?

    func profile(
        playerId: String,
        displayName: String,
        skin: SilhouetteSkin? = nil
    ) -> AppearanceProfile {
        AppearanceProfile(
            playerId: playerId,
            displayName: displayName,
            generatedDescription: generatedDescription,
            embeddingModel: Self.embeddingModel,
            descriptorModel: Self.descriptorModel,
            wholeBodyEmbedding: wholeBodyEmbedding,
            faceEmbeddings: faceEmbeddings,
            upperBodyEmbeddings: upperBodyEmbeddings,
            lowerBodyEmbeddings: lowerBodyEmbeddings,
            headAccessoryEmbeddings: headAccessoryEmbeddings,
            silhouetteDescriptor: silhouetteDescriptor,
            briefingThumbnail: briefingThumbnail,
            skin: skin?.rawValue,
            updatedAtMs: .currentMilliseconds
        )
    }
}

enum AppearanceAnalyzerError: LocalizedError {
    case invalidImage
    case noPerson
    case noFace

    var errorDescription: String? {
        switch self {
        case .invalidImage: return "The selected photo could not be decoded."
        case .noPerson: return "No full person was found. Try a brighter, uncropped mirror selfie."
        case .noFace: return "No face was found. Face the camera in even light and try again."
        }
    }
}

final class AppearanceAnalyzer: @unchecked Sendable {
    func analyze(_ image: UIImage) async throws -> AppearanceAnalysis {
        try await Task.detached(priority: .userInitiated) {
            try Self.analyzeSynchronously(image)
        }.value
    }

    func analyze(bodyImage: UIImage, faceImage: UIImage) async throws -> AppearanceAnalysis {
        try await Task.detached(priority: .userInitiated) {
            let body = try Self.analyzeSynchronously(bodyImage)
            let face = try Self.analyzeFaceSynchronously(faceImage)
            return AppearanceAnalysis(
                generatedDescription: body.generatedDescription,
                wholeBodyEmbedding: body.wholeBodyEmbedding,
                faceEmbeddings: [face.embedding],
                upperBodyEmbeddings: body.upperBodyEmbeddings,
                lowerBodyEmbeddings: body.lowerBodyEmbeddings,
                headAccessoryEmbeddings: body.headAccessoryEmbeddings,
                silhouetteDescriptor: body.silhouetteDescriptor,
                briefingThumbnail: face.thumbnail
            )
        }.value
    }

    private static func analyzeSynchronously(_ image: UIImage) throws -> AppearanceAnalysis {
        guard let normalized = normalizedCGImage(image) else { throw AppearanceAnalyzerError.invalidImage }
        let personRequest = VNDetectHumanRectanglesRequest()
        personRequest.upperBodyOnly = false
        let faceRequest = VNDetectFaceRectanglesRequest()
        let classificationRequest = VNClassifyImageRequest()
        let segmentationRequest = VNGeneratePersonSegmentationRequest()
        segmentationRequest.qualityLevel = .balanced
        segmentationRequest.outputPixelFormat = kCVPixelFormatType_OneComponent8
        let handler = VNImageRequestHandler(cgImage: normalized, orientation: .up)
        try handler.perform([personRequest, faceRequest, segmentationRequest])
        try? handler.perform([classificationRequest])

        guard let person = personRequest.results?.max(by: {
            $0.boundingBox.width * $0.boundingBox.height < $1.boundingBox.width * $1.boundingBox.height
        }), let body = AppearanceFeatureExtractor.crop(normalized, visionRect: person.boundingBox) else {
            throw AppearanceAnalyzerError.noPerson
        }

        let upper = AppearanceFeatureExtractor.crop(body, topFraction: 0, heightFraction: 0.58) ?? body
        let lower = AppearanceFeatureExtractor.crop(body, topFraction: 0.45, heightFraction: 0.55) ?? body
        let head = AppearanceFeatureExtractor.crop(body, topFraction: 0, heightFraction: 0.32) ?? body
        let upperGarment = AppearanceFeatureExtractor.crop(
            body,
            leftFraction: 0.18,
            topFraction: 0.24,
            widthFraction: 0.64,
            heightFraction: 0.30
        ) ?? upper
        let lowerGarment = AppearanceFeatureExtractor.crop(
            body,
            leftFraction: 0.20,
            topFraction: 0.54,
            widthFraction: 0.60,
            heightFraction: 0.34
        ) ?? lower
        // The globally searchable vector starts below the head. Head and face
        // crops remain separate, match-scoped signals.
        let nonFaceOutfit = AppearanceFeatureExtractor.crop(body, topFraction: 0.28, heightFraction: 0.72) ?? body
        let clothing = ClothingLabels(observations: classificationRequest.results ?? [])
        let attributes = outfitAttributes(
            upperGarment: upperGarment,
            lowerGarment: lowerGarment,
            clothing: clothing
        )

        let faceCrops = (faceRequest.results ?? []).compactMap {
            AppearanceFeatureExtractor.crop(normalized, visionRect: $0.boundingBox)
        }
        let faces = faceCrops.map(AppearanceFeatureExtractor.embedding)
        let silhouette = segmentationRequest.results?.first.map {
            AppearanceFeatureExtractor.maskDescriptor($0.pixelBuffer)
        } ?? []
        let briefingCrop = faceCrops.max {
            $0.width * $0.height < $1.width * $1.height
        } ?? head
        let thumbnail = briefingThumbnail(from: briefingCrop)

        return AppearanceAnalysis(
            generatedDescription: AutomaticAppearanceDescriber.describe(attributes),
            wholeBodyEmbedding: AppearanceFeatureExtractor.embedding(nonFaceOutfit),
            faceEmbeddings: faces,
            upperBodyEmbeddings: [AppearanceFeatureExtractor.embedding(upper)],
            lowerBodyEmbeddings: [AppearanceFeatureExtractor.embedding(lower)],
            headAccessoryEmbeddings: [AppearanceFeatureExtractor.embedding(head)],
            silhouetteDescriptor: silhouette,
            briefingThumbnail: thumbnail
        )
    }

    /// Names the top and bottom via MobileCLIP zero-shot when the label set is
    /// available, falling back to deterministic color + Vision classification.
    /// Zero-shot classifies the garment crop semantically, so exposed skin no longer
    /// forces every top to read as "orange".
    private static func outfitAttributes(
        upperGarment: CGImage,
        lowerGarment: CGImage,
        clothing: ClothingLabels
    ) -> ImageAppearanceAttributes {
        let classifier = OutfitZeroShotClassifier.shared
        if classifier.isAvailable,
           let top = classifier.classifyTop(AppearanceFeatureExtractor.embedding(upperGarment)) {
            let isDress = top.garment == "dress"
            let bottom = isDress ? nil : classifier.classifyBottom(AppearanceFeatureExtractor.embedding(lowerGarment))
            var colors = [top.color]
            if let bottom, bottom.color != top.color { colors.append(bottom.color) }
            return ImageAppearanceAttributes(
                dominantColors: colors,
                upperGarment: "\(top.color) \(top.garment)",
                lowerGarment: bottom.map { "\($0.color) \($0.garment)" },
                outerwear: nil,
                footwear: clothing.footwear,
                headwear: clothing.headwear,
                accessory: clothing.accessory
            )
        }
        let upperColor = AppearanceFeatureExtractor.colorName(for: upperGarment)
        let lowerColor = AppearanceFeatureExtractor.colorName(for: lowerGarment)
        return ImageAppearanceAttributes(
            dominantColors: [upperColor, lowerColor],
            upperGarment: clothing.upper.map { "\(upperColor) \($0)" } ?? "\(upperColor) top",
            lowerGarment: clothing.lower.map { "\(lowerColor) \($0)" } ?? "\(lowerColor) bottoms",
            outerwear: clothing.outerwear,
            footwear: clothing.footwear,
            headwear: clothing.headwear,
            accessory: clothing.accessory
        )
    }

    /// Shared, GPU-backed context for briefing stylization. Building a CIContext is
    /// expensive, so it is created once and reused across every enrollment.
    private static let stylizerContext = CIContext(options: [.useSoftwareRenderer: false])

    /// Encodes the opponent briefing image in the classic monochrome "e-fit" composite
    /// look — the face is melted into a waxy, contrast-crushed, lightly banded, and
    /// over-sharpened render so it reads as an uncanny police sketch rather than a real
    /// photo of the player (which also keeps a recognizable photo off the peer's screen).
    /// Falls back to a plain JPEG if Core Image is unavailable, and returns nil only when
    /// even that encode fails, preserving the previous throwing/optional contracts.
    private static func briefingThumbnail(from crop: CGImage) -> String? {
        let source = efitStylized(crop) ?? UIImage(cgImage: crop)
        return source.jpegData(compressionQuality: 0.6)?.base64EncodedString()
    }

    private static func efitStylized(_ crop: CGImage) -> UIImage? {
        let input = CIImage(cgImage: crop)
        let extent = input.extent
        guard extent.width >= 1, extent.height >= 1 else { return nil }

        // 1. Melt fine skin detail into a smooth composite. Clamp first so the blur
        //    does not darken the border pixels.
        guard let blurred = CIFilter(name: "CIGaussianBlur", parameters: [
            kCIInputImageKey: input.clampedToExtent(),
            kCIInputRadiusKey: 1.6
        ])?.outputImage else { return nil }
        // 2. Desaturate fully and crush the contrast toward the flat grey sketch tone.
        guard let mono = CIFilter(name: "CIColorControls", parameters: [
            kCIInputImageKey: blurred,
            kCIInputSaturationKey: 0.0,
            kCIInputContrastKey: 1.35,
            kCIInputBrightnessKey: 0.03
        ])?.outputImage else { return nil }
        // 3. Band the tones so shading reads as an uncanny drawn composite.
        guard let posterized = CIFilter(name: "CIColorPosterize", parameters: [
            kCIInputImageKey: mono,
            "inputLevels": 7.0
        ])?.outputImage else { return nil }
        // 4. Over-sharpen the feature edges for the "fried", over-processed finish.
        guard let sharpened = CIFilter(name: "CIUnsharpMask", parameters: [
            kCIInputImageKey: posterized,
            kCIInputRadiusKey: 2.5,
            kCIInputIntensityKey: 0.9
        ])?.outputImage else { return nil }

        guard let output = stylizerContext.createCGImage(sharpened, from: extent) else { return nil }
        return UIImage(cgImage: output)
    }

    private static func normalizedCGImage(_ image: UIImage) -> CGImage? {
        let longestEdge: CGFloat = 960
        let scale = min(1, longestEdge / max(image.size.width, image.size.height))
        let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
        return UIGraphicsImageRenderer(size: size).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }.cgImage
    }

    private static func analyzeFaceSynchronously(_ image: UIImage) throws -> (embedding: [Float], thumbnail: String) {
        guard let normalized = normalizedCGImage(image) else { throw AppearanceAnalyzerError.invalidImage }
        let request = VNDetectFaceRectanglesRequest()
        try VNImageRequestHandler(cgImage: normalized, orientation: .up).perform([request])
        guard let observation = request.results?.max(by: {
            $0.boundingBox.width * $0.boundingBox.height < $1.boundingBox.width * $1.boundingBox.height
        }), let crop = AppearanceFeatureExtractor.crop(normalized, visionRect: observation.boundingBox) else {
            throw AppearanceAnalyzerError.noFace
        }
        guard let thumbnail = briefingThumbnail(from: crop) else {
            throw AppearanceAnalyzerError.invalidImage
        }
        return (AppearanceFeatureExtractor.embedding(crop), thumbnail)
    }
}

private struct ClothingLabels {
    var upper: String?
    var lower: String?
    var outerwear: String?
    var footwear: String?
    var headwear: String?
    var accessory: String?

    init(observations: [VNClassificationObservation]) {
        let labels = observations
            .filter { $0.confidence >= 0.12 }
            .map { $0.identifier.lowercased() }
        upper = Self.first(in: labels, terms: ["hoodie", "sweater", "shirt", "jersey", "blouse", "top"])
        lower = Self.first(in: labels, terms: ["jeans", "pants", "trousers", "shorts", "skirt"])
        outerwear = Self.first(in: labels, terms: ["jacket", "coat", "parka", "blazer"])
        footwear = Self.first(in: labels, terms: ["sneakers", "shoes", "boots", "sandals"])
        headwear = Self.first(in: labels, terms: ["hat", "cap", "beanie", "helmet"])
        accessory = Self.first(in: labels, terms: ["backpack", "handbag", "glasses", "scarf"])
    }

    private static func first(in labels: [String], terms: [String]) -> String? {
        for term in terms where labels.contains(where: { $0.contains(term) }) { return term }
        return nil
    }
}

enum AppearanceFeatureExtractor {
    /// Prefer the real MobileCLIP2-S0 encoder; fall back to the deterministic
    /// color-grid hash when the model is unavailable or inference fails, so the
    /// app still functions (with weaker recognition) without the bundled model.
    static func embedding(_ image: CGImage) -> [Float] {
        if let vector = MobileCLIPEmbedder.shared.embed(image),
           vector.count == appearanceEmbeddingDimensions {
            return EmbeddingMath.normalized(vector)
        }
        return colorGridEmbedding(image)
    }

    private static func colorGridEmbedding(_ image: CGImage) -> [Float] {
        let side = 32
        guard let pixels = rgbaPixels(image, width: side, height: side) else {
            return Array(repeating: 0, count: appearanceEmbeddingDimensions)
        }
        var values = Array(repeating: Float.zero, count: appearanceEmbeddingDimensions)
        for index in values.indices {
            let pixelIndex = ((index * 67) + (index / 3) * 29) % (side * side)
            let channel = index % 3
            values[index] = Float(pixels[pixelIndex * 4 + channel]) / 127.5 - 1
        }
        return EmbeddingMath.normalized(values)
    }

    /// A small grayscale face descriptor used only to choose which detected
    /// person should receive the expensive active-match appearance score.
    /// Mean/contrast normalization makes it comparable with the monochrome,
    /// posterized briefing thumbnail without invoking Core ML for every person.
    static func faceStructureDescriptor(
        _ image: CGImage,
        mirrored: Bool = false
    ) -> [Float] {
        let side = 16
        guard let pixels = rgbaPixels(image, width: side, height: side) else {
            return Array(repeating: 0, count: appearanceEmbeddingDimensions)
        }
        var luminance = Array(repeating: Float.zero, count: side * side)
        for y in 0..<side {
            for x in 0..<side {
                let sourceX = mirrored ? side - x - 1 : x
                let offset = (y * side + sourceX) * 4
                luminance[y * side + x] =
                    Float(pixels[offset]) * 0.2126
                    + Float(pixels[offset + 1]) * 0.7152
                    + Float(pixels[offset + 2]) * 0.0722
            }
        }

        func standardized(_ values: [Float]) -> [Float] {
            let mean = values.reduce(0, +) / Float(values.count)
            let variance = values.reduce(Float.zero) {
                let delta = $1 - mean
                return $0 + delta * delta
            } / Float(values.count)
            let deviation = max(sqrt(variance), 1)
            return values.map { ($0 - mean) / deviation }
        }

        var gradientMagnitude = Array(repeating: Float.zero, count: side * side)
        for y in 0..<side {
            for x in 0..<side {
                let left = luminance[y * side + max(x - 1, 0)]
                let right = luminance[y * side + min(x + 1, side - 1)]
                let top = luminance[max(y - 1, 0) * side + x]
                let bottom = luminance[min(y + 1, side - 1) * side + x]
                gradientMagnitude[y * side + x] = hypot(right - left, bottom - top)
            }
        }
        return EmbeddingMath.normalized(
            standardized(luminance) + standardized(gradientMagnitude)
        )
    }

    static func colorName(for image: CGImage) -> String {
        let side = 24
        guard let pixels = rgbaPixels(image, width: side, height: side) else { return "neutral" }
        // Vote on the dominant color name rather than taking per-channel medians
        // (which blend distinct colors into a meaningless average). Skin-like pixels
        // are tallied separately and only used when the crop is essentially all skin,
        // so exposed arms/neck/face no longer make every garment read as "orange".
        var garment: [String: Int] = [:]
        var skin: [String: Int] = [:]
        for index in stride(from: 0, to: pixels.count, by: 4) {
            let red = Double(pixels[index]) / 255
            let green = Double(pixels[index + 1]) / 255
            let blue = Double(pixels[index + 2]) / 255
            let name = PerceptualColorClassifier.name(red: red, green: green, blue: blue)
            if isSkinLike(red: red, green: green, blue: blue) {
                skin[name, default: 0] += 1
            } else {
                garment[name, default: 0] += 1
            }
        }
        let pool = garment.isEmpty ? skin : garment
        return pool.max { $0.value < $1.value }?.key ?? "neutral"
    }

    /// Heuristic for human skin: warm hue, moderate saturation, mid-to-high
    /// brightness, with R >= G >= B. Deliberately conservative so genuinely orange
    /// or tan clothing still registers when no clearer garment color is present.
    private static func isSkinLike(red: Double, green: Double, blue: Double) -> Bool {
        let maximum = max(red, green, blue)
        let minimum = min(red, green, blue)
        let chroma = maximum - minimum
        guard chroma > 0 else { return false }
        let saturation = maximum > 0 ? chroma / maximum : 0
        let luminance = 0.2126 * red + 0.7152 * green + 0.0722 * blue
        let hue: Double
        if maximum == red {
            hue = 60 * ((green - blue) / chroma).truncatingRemainder(dividingBy: 6)
        } else if maximum == green {
            hue = 60 * ((blue - red) / chroma + 2)
        } else {
            hue = 60 * ((red - green) / chroma + 4)
        }
        let normalizedHue = hue < 0 ? hue + 360 : hue
        return (normalizedHue < 50 || normalizedHue > 350)
            && saturation >= 0.12 && saturation <= 0.70
            && luminance >= 0.32 && luminance <= 0.94
            && red >= green && green >= blue
    }

    static func maskDescriptor(_ pixelBuffer: CVPixelBuffer) -> [Float] {
        CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pixelBuffer) else { return [] }
        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        let rowBytes = CVPixelBufferGetBytesPerRow(pixelBuffer)
        let bytes = base.assumingMemoryBound(to: UInt8.self)
        var descriptor = Array(repeating: Float.zero, count: 64)
        for row in 0..<8 {
            for column in 0..<8 {
                var sum = 0
                var count = 0
                let yRange = (row * height / 8)..<((row + 1) * height / 8)
                let xRange = (column * width / 8)..<((column + 1) * width / 8)
                for y in yRange where y % 2 == 0 {
                    for x in xRange where x % 2 == 0 {
                        sum += Int(bytes[y * rowBytes + x])
                        count += 1
                    }
                }
                descriptor[row * 8 + column] = count == 0 ? 0 : Float(sum) / Float(count * 255)
            }
        }
        return descriptor
    }

    static func crop(_ image: CGImage, visionRect: CGRect) -> CGImage? {
        let width = CGFloat(image.width)
        let height = CGFloat(image.height)
        let rect = CGRect(
            x: visionRect.minX * width,
            y: (1 - visionRect.maxY) * height,
            width: visionRect.width * width,
            height: visionRect.height * height
        ).intersection(CGRect(x: 0, y: 0, width: width, height: height)).integral
        guard rect.width > 1, rect.height > 1 else { return nil }
        return image.cropping(to: rect)
    }

    static func crop(_ image: CGImage, topFraction: CGFloat, heightFraction: CGFloat) -> CGImage? {
        crop(
            image,
            leftFraction: 0,
            topFraction: topFraction,
            widthFraction: 1,
            heightFraction: heightFraction
        )
    }

    static func crop(
        _ image: CGImage,
        leftFraction: CGFloat,
        topFraction: CGFloat,
        widthFraction: CGFloat,
        heightFraction: CGFloat
    ) -> CGImage? {
        let rect = CGRect(
            x: CGFloat(image.width) * leftFraction,
            y: CGFloat(image.height) * topFraction,
            width: CGFloat(image.width) * widthFraction,
            height: CGFloat(image.height) * heightFraction
        ).intersection(CGRect(x: 0, y: 0, width: image.width, height: image.height)).integral
        return image.cropping(to: rect)
    }

    private static func rgbaPixels(_ image: CGImage, width: Int, height: Int) -> [UInt8]? {
        var pixels = Array(repeating: UInt8.zero, count: width * height * 4)
        let created = pixels.withUnsafeMutableBytes { rawBuffer -> Bool in
            guard let context = CGContext(
                data: rawBuffer.baseAddress,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: width * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGBitmapInfo.byteOrder32Big.rawValue
                    | CGImageAlphaInfo.premultipliedLast.rawValue
            ) else { return false }
            context.interpolationQuality = .medium
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        return created ? pixels : nil
    }
}
