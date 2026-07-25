import CoreImage
import UIKit
import Vision

struct PersonTargetingResult: Identifiable, @unchecked Sendable {
    let id: Int
    let visionBoundingBox: CGRect
    let maskImage: UIImage?
    let collisionMask: PersonCollisionMask?
    let silhouetteGrid: [Float]
    let targetScore: Float
    let identityIsFixture: Bool
    let timestamp: TimeInterval

    func targetingState(
        for aim: AimSolution,
        frameTimestamp: TimeInterval,
        tuning: GameplayTargetingTuning = .default
    ) -> GameplayTargetingState {
        targetingState(
            gameplayPoint: aim.gameplayScreenPoint,
            zonePoint: aim.screenPoint,
            frameTimestamp: frameTimestamp,
            tuning: tuning
        )
    }

    // Sights mode publishes no directional solution, so it evaluates targeting
    // at the fixed centre reticle instead of at an aim point.
    func targetingState(
        gameplayPoint: CGPoint,
        zonePoint: CGPoint,
        frameTimestamp: TimeInterval,
        tuning: GameplayTargetingTuning = .default
    ) -> GameplayTargetingState {
        GameplayTargetEvaluator.evaluate(
            gameplayPoint: gameplayPoint,
            zonePoint: zonePoint,
            targetBoundingBox: visionBoundingBox,
            collisionMask: collisionMask,
            targetScore: targetScore,
            targetTimestamp: timestamp,
            frameTimestamp: frameTimestamp,
            tuning: tuning
        )
    }
}

final class PersonTargetingRunner: @unchecked Sendable {
    private let queue = DispatchQueue(label: "camera.person.targeting.queue", qos: .userInitiated)
    private let lock = NSLock()
    private let context = CIContext(options: [.cacheIntermediates: false])
    private var busy = false
    private var sequence = 0
    private var lastSubmission = 0.0
    private var targetSelector = PersonTargetSelector()
    private var referenceFaceKey: String?
    private var cachedReferenceFaceDescriptor: [Float]?

    func reset() {
        queue.async { [weak self] in
            self?.targetSelector.reset()
        }
    }

    @discardableResult
    func submit(
        _ frame: CameraFrame,
        targetProfile: AppearanceProfile?,
        completion: @escaping @Sendable (PersonTargetingResult?) -> Void
    ) -> Bool {
        let now = CACurrentMediaTime()
        let accepted = lock.withLock {
            guard !busy, now - lastSubmission >= 0.30 else { return false }
            busy = true
            lastSubmission = now
            return true
        }
        guard accepted else { return false }
        queue.async { [weak self] in
            guard let self else { return }
            defer { self.lock.withLock { self.busy = false } }
            let humanRequest = VNDetectHumanRectanglesRequest()
            humanRequest.upperBodyOnly = false
            let faceRequest = VNDetectFaceRectanglesRequest()
            let handler = VNImageRequestHandler(
                cvPixelBuffer: frame.pixelBuffer,
                orientation: frame.orientation,
                options: [:]
            )
            do {
                try handler.perform([humanRequest])
                try? handler.perform([faceRequest])
                let faces = faceRequest.results ?? []
                let frameImage = CIImage(cvPixelBuffer: frame.pixelBuffer).oriented(frame.orientation)
                let fullImage = context.createCGImage(frameImage, from: frameImage.extent)
                let identityIsFixture = targetProfile?.embeddingModel.hasPrefix("fixture") == true
                let targetFaceDescriptor = referenceFaceDescriptor(for: targetProfile)
                let candidates = (humanRequest.results ?? []).map(\.boundingBox).map { box in
                    let face = faces
                        .filter {
                            box.contains(
                                CGPoint(
                                    x: $0.boundingBox.midX,
                                    y: $0.boundingBox.midY
                                )
                            )
                        }
                        .max {
                            $0.boundingBox.width * $0.boundingBox.height
                                < $1.boundingBox.width * $1.boundingBox.height
                        }
                        .flatMap { observation in
                            fullImage.flatMap {
                                AppearanceFeatureExtractor.crop(
                                    $0,
                                    visionRect: observation.boundingBox
                                )
                            }
                        }
                    return PersonTargetCandidate(
                        box: box,
                        identityScore: Self.acquisitionScore(
                            face: face,
                            targetFaceDescriptor: targetFaceDescriptor,
                            target: targetProfile
                        )
                    )
                }
                guard let selected = targetSelector.select(
                    candidates: candidates,
                    faceBoxes: faces.map(\.boundingBox)
                ) else {
                    completion(nil)
                    return
                }
                let humanBox = selected.box
                let whole = fullImage.flatMap { AppearanceFeatureExtractor.crop($0, visionRect: humanBox) }
                var maskImage: UIImage?
                var collisionMask: PersonCollisionMask?
                var maskDescriptor: [Float] = []
                if let whole,
                   let localMask = Self.personMask(for: whole) {
                    maskDescriptor = localMask.occupancyDescriptor()
                    collisionMask = localMask.composited(
                        in: frame.orientedImageSize,
                        targetBox: humanBox
                    )
                    if let collisionMask {
                        maskImage = Self.alphaImage(from: collisionMask)
                    }
                }
                let candidateFaces = faces.filter {
                    humanBox.contains(CGPoint(x: $0.boundingBox.midX, y: $0.boundingBox.midY))
                }
                let targetScore = Self.targetScore(
                    whole: whole,
                    fullImage: fullImage,
                    faces: candidateFaces,
                    maskDescriptor: maskDescriptor,
                    target: targetProfile
                )
                sequence += 1
                completion(PersonTargetingResult(
                    id: sequence,
                    visionBoundingBox: humanBox,
                    maskImage: maskImage,
                    collisionMask: collisionMask,
                    silhouetteGrid: maskDescriptor,
                    targetScore: targetScore,
                    identityIsFixture: identityIsFixture,
                    timestamp: frame.timestamp.seconds
                ))
            } catch {
                completion(nil)
            }
        }
        return true
    }

    private static func personMask(for image: CGImage) -> PersonCollisionMask? {
        let request = VNGeneratePersonSegmentationRequest()
        request.qualityLevel = .fast
        request.outputPixelFormat = kCVPixelFormatType_OneComponent8
        let handler = VNImageRequestHandler(cgImage: image, orientation: .up)
        do {
            try handler.perform([request])
        } catch {
            return nil
        }
        guard let pixelBuffer = request.results?.first?.pixelBuffer else { return nil }
        CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly) }
        guard let baseAddress = CVPixelBufferGetBaseAddress(pixelBuffer) else { return nil }
        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        let bytesPerRow = CVPixelBufferGetBytesPerRow(pixelBuffer)
        let source = baseAddress.assumingMemoryBound(to: UInt8.self)
        var pixels = Array(repeating: UInt8.zero, count: width * height)
        for row in 0..<height {
            let start = row * bytesPerRow
            pixels.replaceSubrange(
                (row * width)..<((row + 1) * width),
                with: UnsafeBufferPointer(start: source + start, count: width)
            )
        }
        let foregroundValue = UInt8(
            min(
                max(
                    Int(ceil(Double(GameplayTargetingTuning.default.foregroundThreshold) * 255)),
                    0
                ),
                255
            )
        )
        guard pixels.contains(where: { $0 >= foregroundValue }) else { return nil }
        return PersonCollisionMask(
            width: width,
            height: height,
            pixels: pixels
        )
    }

    private static func alphaImage(from mask: PersonCollisionMask) -> UIImage? {
        guard mask.isValid else { return nil }
        let rgba = mask.premultipliedWhiteRGBA()
        guard let provider = CGDataProvider(data: Data(rgba) as CFData),
              let image = CGImage(
                width: mask.width,
                height: mask.height,
                bitsPerComponent: 8,
                bitsPerPixel: 32,
                bytesPerRow: mask.width * 4,
                space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                provider: provider,
                decode: nil,
                shouldInterpolate: true,
                intent: .defaultIntent
              ) else { return nil }
        return UIImage(cgImage: image)
    }

    private func referenceFaceDescriptor(
        for target: AppearanceProfile?
    ) -> [Float]? {
        let key = target.map {
            "\($0.playerId):\($0.updatedAtMs):\($0.briefingThumbnail?.hashValue ?? 0)"
        }
        guard key != referenceFaceKey else {
            return cachedReferenceFaceDescriptor
        }
        referenceFaceKey = key
        guard let base64 = target?.briefingThumbnail,
              let data = Data(base64Encoded: base64),
              let image = UIImage(data: data),
              let cgImage = image.cgImage else {
            cachedReferenceFaceDescriptor = nil
            return nil
        }
        cachedReferenceFaceDescriptor =
            AppearanceFeatureExtractor.faceStructureDescriptor(cgImage)
        return cachedReferenceFaceDescriptor
    }

    private static func acquisitionScore(
        face: CGImage?,
        targetFaceDescriptor: [Float]?,
        target: AppearanceProfile?
    ) -> Float {
        guard let target else { return 0 }
        guard !target.embeddingModel.hasPrefix("fixture") else { return 0.72 }
        guard let face, let targetFaceDescriptor else { return 0 }
        let normal = AppearanceFeatureExtractor.faceStructureDescriptor(face)
        let mirrored = AppearanceFeatureExtractor.faceStructureDescriptor(
            face,
            mirrored: true
        )
        return [normal, mirrored]
            .compactMap { similarity($0, [targetFaceDescriptor]) }
            .max() ?? 0
    }

    private static func targetScore(
        whole: CGImage?,
        fullImage: CGImage?,
        faces: [VNFaceObservation],
        maskDescriptor: [Float],
        target: AppearanceProfile?
    ) -> Float {
        guard let target else { return 0 }
        guard !target.embeddingModel.hasPrefix("fixture") else { return 0.72 }
        guard let whole else { return 0 }
        let upper = AppearanceFeatureExtractor.crop(whole, topFraction: 0, heightFraction: 0.58)
        let lower = AppearanceFeatureExtractor.crop(whole, topFraction: 0.45, heightFraction: 0.55)
        let head = AppearanceFeatureExtractor.crop(whole, topFraction: 0, heightFraction: 0.32)
        // Mirror enrollment's head exclusion for the globally indexed outfit
        // vector; face/head comparisons are fused only below in active-match scope.
        let nonFaceOutfit = AppearanceFeatureExtractor.crop(whole, topFraction: 0.28, heightFraction: 0.72) ?? whole
        let observedFaces = faces.compactMap { observation in
            fullImage.flatMap { AppearanceFeatureExtractor.crop($0, visionRect: observation.boundingBox) }
        }.map(AppearanceFeatureExtractor.embedding)

        return AppearanceScoreFusion.score(AppearanceSignalScores(
            wholeBody: similarity(AppearanceFeatureExtractor.embedding(nonFaceOutfit), [target.wholeBodyEmbedding]),
            outfitText: nil,
            upperBody: upper.flatMap { similarity(AppearanceFeatureExtractor.embedding($0), target.upperBodyEmbeddings) },
            lowerBody: lower.flatMap { similarity(AppearanceFeatureExtractor.embedding($0), target.lowerBodyEmbeddings) },
            headAccessory: head.flatMap { similarity(AppearanceFeatureExtractor.embedding($0), target.headAccessoryEmbeddings) },
            silhouette: similarity(maskDescriptor, [target.silhouetteDescriptor]),
            face: observedFaces.compactMap { similarity($0, target.faceEmbeddings) }.max(),
            bodyGeometry: nil
        ), scope: .activeMatch)
    }

    private static func similarity(_ observed: [Float], _ enrolled: [[Float]]) -> Float? {
        guard !observed.isEmpty else { return nil }
        return enrolled.compactMap { EmbeddingMath.cosineSimilarity(observed, $0) }
            .map { min(max(($0 + 1) * 0.5, 0), 1) }
            .max()
    }
}
