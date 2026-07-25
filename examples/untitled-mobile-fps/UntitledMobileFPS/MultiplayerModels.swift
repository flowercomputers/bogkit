import CoreGraphics
import Foundation

let multiplayerProtocolVersion = 2
let appearanceEmbeddingDimensions = 512

struct GameplayTargetingTuning: Equatable, Sendable {
    var foregroundThreshold: Float = 0.35
    var reticleRadiusFraction: Double = 0.018
    var minimumReticleCoverage: Float = 0.08
    var maximumTargetAgeSeconds: TimeInterval = 0.75
    var minimumTargetScore: Float = 0.5
    var minimumAcquisitionScore: Float = 0.56
    var minimumAcquisitionLead: Float = 0.03
    var identitySwitchMargin: Float = 0.08
    var acquisitionConfirmationFrames: Int = 2
    var switchConfirmationFrames: Int = 3

    static let `default` = GameplayTargetingTuning()
}

struct PersonTargetCandidate: Equatable, Sendable {
    let box: CGRect
    let identityScore: Float
}

struct PersonTargetSelector: Sendable {
    private(set) var selectedBox: CGRect?
    private(set) var selectedIdentityScore: Float?
    private var consecutiveMisses = 0
    private var pendingCandidate: PersonTargetCandidate?
    private var pendingCandidateFrames = 0
    private let maximumTrackedMisses = 6
    private let tuning: GameplayTargetingTuning

    init(tuning: GameplayTargetingTuning = .default) {
        self.tuning = tuning
    }

    mutating func select(
        candidates: [PersonTargetCandidate],
        faceBoxes: [CGRect]
    ) -> PersonTargetCandidate? {
        let viewport = CGRect(x: 0, y: 0, width: 1, height: 1)
        let boxes = candidates
            .map {
                PersonTargetCandidate(
                    box: $0.box.intersection(viewport),
                    identityScore: $0.identityScore
                )
            }
            .filter {
                !$0.box.isNull && $0.box.width >= 0.025 && $0.box.height >= 0.08
            }
            .filter { isPlausiblePerson($0.box, faceBoxes: faceBoxes) }
        guard !boxes.isEmpty else {
            clearPendingCandidate()
            registerMiss()
            return nil
        }

        if let selectedBox {
            let tracked = boxes
                .filter { isContinuous($0.box, from: selectedBox) }
                .max {
                    trackingScore($0, previous: selectedBox, faceBoxes: faceBoxes)
                        < trackingScore($1, previous: selectedBox, faceBoxes: faceBoxes)
                }
            guard let tracked else {
                registerMiss()
                guard let challenger = bestUnambiguousCandidate(in: boxes) else {
                    clearPendingCandidate()
                    return nil
                }
                if confirm(
                    challenger,
                    requiredFrames: tuning.switchConfirmationFrames
                ) {
                    return select(challenger)
                }
                return nil
            }

            let challenger = bestUnambiguousCandidate(
                in: boxes.filter { $0 != tracked }
            )
            if let challenger,
               challenger.identityScore >= tracked.identityScore + tuning.identitySwitchMargin {
                if confirm(challenger, requiredFrames: tuning.switchConfirmationFrames) {
                    return select(challenger)
                }
            } else {
                clearPendingCandidate()
            }
            self.selectedBox = tracked.box
            selectedIdentityScore = tracked.identityScore
            consecutiveMisses = 0
            return tracked
        }

        guard let candidate = bestUnambiguousCandidate(in: boxes),
              confirm(candidate, requiredFrames: tuning.acquisitionConfirmationFrames) else {
            return nil
        }
        return select(candidate)
    }

    mutating func reset() {
        selectedBox = nil
        selectedIdentityScore = nil
        consecutiveMisses = 0
        clearPendingCandidate()
    }

    private func isPlausiblePerson(_ box: CGRect, faceBoxes: [CGRect]) -> Bool {
        let hasFace = containsFace(box, faceBoxes: faceBoxes)
        let aspect = box.height / max(box.width, 0.001)
        let touchesEdge = box.minX <= 0.01 || box.maxX >= 0.99
            || box.minY <= 0.01 || box.maxY >= 0.99
        if !hasFace && aspect < 1.3 { return false }
        if !hasFace && box.width * box.height > 0.20 && touchesEdge { return false }
        return true
    }

    private func isContinuous(_ box: CGRect, from previous: CGRect) -> Bool {
        let centerDistance = hypot(box.midX - previous.midX, box.midY - previous.midY)
        // Person detection runs roughly every 0.3 seconds. A target cannot
        // plausibly cross most of the frame between submissions; the former
        // 1.5x-diagonal radius routinely treated a distant bystander as the
        // same person, especially for tall full-body boxes.
        let maximumDistance = min(
            max(0.10, hypot(previous.width, previous.height) * 0.55),
            0.30
        )
        let previousArea = max(previous.width * previous.height, 0.0001)
        let areaRatio = box.width * box.height / previousArea
        return intersectionOverUnion(box, previous) >= 0.02
            || (centerDistance <= maximumDistance && (0.25...4).contains(areaRatio))
    }

    private func acquisitionScore(
        _ candidate: PersonTargetCandidate,
        faceBoxes: [CGRect]
    ) -> Double {
        let box = candidate.box
        let faceBonus = containsFace(box, faceBoxes: faceBoxes) ? 5.0 : 0
        let aspect = min(Double(box.height / max(box.width, 0.001)), 4)
        let areaPenalty = Double(box.width * box.height) * 2
        return Double(candidate.identityScore) * 12 + faceBonus + aspect - areaPenalty
    }

    private func trackingScore(
        _ candidate: PersonTargetCandidate,
        previous: CGRect,
        faceBoxes: [CGRect]
    ) -> Double {
        let box = candidate.box
        let centerDistance = hypot(box.midX - previous.midX, box.midY - previous.midY)
        let previousArea = max(previous.width * previous.height, 0.0001)
        let areaRatio = max(box.width * box.height / previousArea, 0.0001)
        let faceBonus = containsFace(box, faceBoxes: faceBoxes) ? 2.0 : 0
        return intersectionOverUnion(box, previous) * 5
            - Double(centerDistance) * 3
            - abs(log(Double(areaRatio))) * 0.75
            + Double(candidate.identityScore)
            + faceBonus
    }

    private func bestUnambiguousCandidate(
        in candidates: [PersonTargetCandidate]
    ) -> PersonTargetCandidate? {
        let eligible = candidates
            .filter { $0.identityScore >= tuning.minimumAcquisitionScore }
            .sorted {
                if abs($0.identityScore - $1.identityScore) > 0.001 {
                    return $0.identityScore > $1.identityScore
                }
                return acquisitionScore($0, faceBoxes: [])
                    > acquisitionScore($1, faceBoxes: [])
            }
        guard let first = eligible.first else { return nil }
        if eligible.count > 1,
           first.identityScore - eligible[1].identityScore < tuning.minimumAcquisitionLead {
            return nil
        }
        return first
    }

    private mutating func confirm(
        _ candidate: PersonTargetCandidate,
        requiredFrames: Int
    ) -> Bool {
        if let pendingCandidate,
           isContinuous(candidate.box, from: pendingCandidate.box) {
            self.pendingCandidate = candidate
            pendingCandidateFrames += 1
        } else {
            pendingCandidate = candidate
            pendingCandidateFrames = 1
        }
        return pendingCandidateFrames >= max(requiredFrames, 1)
    }

    private mutating func select(
        _ candidate: PersonTargetCandidate
    ) -> PersonTargetCandidate {
        selectedBox = candidate.box
        selectedIdentityScore = candidate.identityScore
        consecutiveMisses = 0
        clearPendingCandidate()
        return candidate
    }

    private mutating func clearPendingCandidate() {
        pendingCandidate = nil
        pendingCandidateFrames = 0
    }

    private func containsFace(_ box: CGRect, faceBoxes: [CGRect]) -> Bool {
        faceBoxes.contains { face in
            box.contains(CGPoint(x: face.midX, y: face.midY))
        }
    }

    private func intersectionOverUnion(_ lhs: CGRect, _ rhs: CGRect) -> Double {
        let intersection = lhs.intersection(rhs)
        guard !intersection.isNull, !intersection.isEmpty else { return 0 }
        let intersectionArea = intersection.width * intersection.height
        let unionArea = lhs.width * lhs.height + rhs.width * rhs.height - intersectionArea
        return unionArea <= 0 ? 0 : Double(intersectionArea / unionArea)
    }

    private mutating func registerMiss() {
        consecutiveMisses += 1
        if consecutiveMisses >= maximumTrackedMisses {
            selectedBox = nil
            selectedIdentityScore = nil
            consecutiveMisses = 0
        }
    }
}

struct PersonCollisionMask: Equatable, Sendable {
    let width: Int
    let height: Int
    let pixels: [UInt8]

    init(width: Int, height: Int, pixels: [UInt8], clippingTo visionBoundingBox: CGRect? = nil) {
        self.width = max(width, 0)
        self.height = max(height, 0)
        let expectedCount = max(width, 0) * max(height, 0)
        guard expectedCount > 0, pixels.count == expectedCount else {
            self.pixels = []
            return
        }
        guard let visionBoundingBox else {
            self.pixels = pixels
            return
        }

        let clippedBox = visionBoundingBox.intersection(CGRect(x: 0, y: 0, width: 1, height: 1))
        guard !clippedBox.isNull, !clippedBox.isEmpty else {
            self.pixels = Array(repeating: 0, count: expectedCount)
            return
        }
        let minimumColumn = max(Int(floor(clippedBox.minX * CGFloat(width))), 0)
        let maximumColumn = min(Int(ceil(clippedBox.maxX * CGFloat(width))), width)
        let minimumRow = max(Int(floor((1 - clippedBox.maxY) * CGFloat(height))), 0)
        let maximumRow = min(Int(ceil((1 - clippedBox.minY) * CGFloat(height))), height)
        var clippedPixels = pixels
        for row in 0..<height {
            let rowStart = row * width
            if row < minimumRow || row >= maximumRow {
                clippedPixels.replaceSubrange(
                    rowStart..<(rowStart + width),
                    with: repeatElement(UInt8.zero, count: width)
                )
                continue
            }
            if minimumColumn > 0 {
                clippedPixels.replaceSubrange(
                    rowStart..<(rowStart + minimumColumn),
                    with: repeatElement(UInt8.zero, count: minimumColumn)
                )
            }
            if maximumColumn < width {
                clippedPixels.replaceSubrange(
                    (rowStart + maximumColumn)..<(rowStart + width),
                    with: repeatElement(UInt8.zero, count: width - maximumColumn)
                )
            }
        }
        self.pixels = clippedPixels
    }

    var isValid: Bool {
        width > 0 && height > 0 && pixels.count == width * height
    }

    func reticleCoverage(
        at point: CGPoint,
        tuning: GameplayTargetingTuning = .default
    ) -> Float {
        guard isValid,
              point.x.isFinite, point.y.isFinite,
              (0...1).contains(point.x), (0...1).contains(point.y) else { return 0 }

        let centerX = Double(point.x) * Double(width)
        let centerY = Double(1 - point.y) * Double(height)
        let radius = max(Double(min(width, height)) * tuning.reticleRadiusFraction, 1)
        let minimumX = max(Int(floor(centerX - radius)), 0)
        let maximumX = min(Int(ceil(centerX + radius)), width - 1)
        let minimumY = max(Int(floor(centerY - radius)), 0)
        let maximumY = min(Int(ceil(centerY + radius)), height - 1)
        let foregroundValue = UInt8(
            min(max(Int(ceil(Double(tuning.foregroundThreshold) * 255)), 0), 255)
        )
        var foreground = 0
        var samples = 0

        for row in minimumY...maximumY {
            for column in minimumX...maximumX {
                let deltaX = Double(column) + 0.5 - centerX
                let deltaY = Double(row) + 0.5 - centerY
                guard deltaX * deltaX + deltaY * deltaY <= radius * radius else { continue }
                samples += 1
                if pixels[row * width + column] >= foregroundValue {
                    foreground += 1
                }
            }
        }
        return samples == 0 ? 0 : Float(foreground) / Float(samples)
    }

    func occupancyDescriptor(gridSize: Int = 8) -> [Float] {
        guard isValid, gridSize > 0 else { return [] }
        var descriptor = Array(repeating: Float.zero, count: gridSize * gridSize)
        for row in 0..<gridSize {
            for column in 0..<gridSize {
                let yRange = (row * height / gridSize)..<((row + 1) * height / gridSize)
                let xRange = (column * width / gridSize)..<((column + 1) * width / gridSize)
                var sum = 0
                var count = 0
                for y in yRange {
                    for x in xRange {
                        sum += Int(pixels[y * width + x])
                        count += 1
                    }
                }
                descriptor[row * gridSize + column] = count == 0
                    ? 0
                    : Float(sum) / Float(count * 255)
            }
        }
        return descriptor
    }

    func premultipliedWhiteRGBA() -> [UInt8] {
        guard isValid else { return [] }
        var rgba = Array(repeating: UInt8.zero, count: pixels.count * 4)
        for (index, alpha) in pixels.enumerated() {
            let offset = index * 4
            rgba[offset] = alpha
            rgba[offset + 1] = alpha
            rgba[offset + 2] = alpha
            rgba[offset + 3] = alpha
        }
        return rgba
    }

    func composited(
        in frameSize: CGSize,
        targetBox: CGRect,
        maximumDimension: CGFloat = 512
    ) -> PersonCollisionMask? {
        guard isValid,
              frameSize.width > 0, frameSize.height > 0,
              maximumDimension > 0 else { return nil }
        let scale = maximumDimension / max(frameSize.width, frameSize.height)
        let canvasWidth = max(Int((frameSize.width * scale).rounded()), 1)
        let canvasHeight = max(Int((frameSize.height * scale).rounded()), 1)
        let clippedBox = targetBox.intersection(CGRect(x: 0, y: 0, width: 1, height: 1))
        guard !clippedBox.isNull, !clippedBox.isEmpty else { return nil }

        let minimumX = max(Int(floor(clippedBox.minX * CGFloat(canvasWidth))), 0)
        let maximumX = min(Int(ceil(clippedBox.maxX * CGFloat(canvasWidth))), canvasWidth)
        let minimumY = max(Int(floor((1 - clippedBox.maxY) * CGFloat(canvasHeight))), 0)
        let maximumY = min(Int(ceil((1 - clippedBox.minY) * CGFloat(canvasHeight))), canvasHeight)
        guard maximumX > minimumX, maximumY > minimumY else { return nil }

        var canvas = Array(repeating: UInt8.zero, count: canvasWidth * canvasHeight)
        let targetWidth = maximumX - minimumX
        let targetHeight = maximumY - minimumY
        for row in minimumY..<maximumY {
            let localY = min(
                Int(Double(row - minimumY) / Double(targetHeight) * Double(height)),
                height - 1
            )
            for column in minimumX..<maximumX {
                let localX = min(
                    Int(Double(column - minimumX) / Double(targetWidth) * Double(width)),
                    width - 1
                )
                canvas[row * canvasWidth + column] = pixels[localY * width + localX]
            }
        }
        return PersonCollisionMask(
            width: canvasWidth,
            height: canvasHeight,
            pixels: canvas,
            clippingTo: clippedBox
        )
    }
}

enum GameplayTargetingStatus: String, Codable, Equatable, Sendable {
    case unavailable
    case stale
    case outsideMask
    case identityWeak
    case ready
}

struct GameplayTargetingState: Codable, Equatable, Sendable {
    let gameplayPoint: CGPoint
    let zonePoint: CGPoint
    let targetBoundingBox: CGRect?
    let targetAgeSeconds: TimeInterval?
    let targetScore: Float
    let maskCoverage: Float
    let maskContainsReticle: Bool
    let status: GameplayTargetingStatus

    var localGatesPass: Bool { status == .ready }
}

enum GameplayTargetEvaluator {
    static func evaluate(
        gameplayPoint: CGPoint,
        zonePoint: CGPoint,
        targetBoundingBox: CGRect?,
        collisionMask: PersonCollisionMask?,
        targetScore: Float,
        targetTimestamp: TimeInterval?,
        frameTimestamp: TimeInterval,
        tuning: GameplayTargetingTuning = .default
    ) -> GameplayTargetingState {
        let age = targetTimestamp.map { max(frameTimestamp - $0, 0) }
        guard let targetBoundingBox, let collisionMask, collisionMask.isValid else {
            return GameplayTargetingState(
                gameplayPoint: gameplayPoint,
                zonePoint: zonePoint,
                targetBoundingBox: targetBoundingBox,
                targetAgeSeconds: age,
                targetScore: targetScore,
                maskCoverage: 0,
                maskContainsReticle: false,
                status: .unavailable
            )
        }
        guard let age, age <= tuning.maximumTargetAgeSeconds else {
            return GameplayTargetingState(
                gameplayPoint: gameplayPoint,
                zonePoint: zonePoint,
                targetBoundingBox: targetBoundingBox,
                targetAgeSeconds: age,
                targetScore: targetScore,
                maskCoverage: 0,
                maskContainsReticle: false,
                status: .stale
            )
        }

        let coverage = collisionMask.reticleCoverage(at: gameplayPoint, tuning: tuning)
        let contains = coverage >= tuning.minimumReticleCoverage
        let status: GameplayTargetingStatus
        if !contains {
            status = .outsideMask
        } else if targetScore < tuning.minimumTargetScore {
            status = .identityWeak
        } else {
            status = .ready
        }
        return GameplayTargetingState(
            gameplayPoint: gameplayPoint,
            zonePoint: zonePoint,
            targetBoundingBox: targetBoundingBox,
            targetAgeSeconds: age,
            targetScore: targetScore,
            maskCoverage: coverage,
            maskContainsReticle: contains,
            status: status
        )
    }
}

struct GameplayShotDiagnostic: Codable, Equatable, Sendable {
    let gameplayPoint: CGPoint
    let zonePoint: CGPoint
    let targetBoundingBox: CGRect?
    let targetAgeSeconds: TimeInterval?
    let targetScore: Float
    let maskCoverage: Float
    let maskContainsReticle: Bool
    let status: GameplayTargetingStatus

    init(_ state: GameplayTargetingState) {
        gameplayPoint = state.gameplayPoint
        zonePoint = state.zonePoint
        targetBoundingBox = state.targetBoundingBox
        targetAgeSeconds = state.targetAgeSeconds
        targetScore = state.targetScore
        maskCoverage = state.maskCoverage
        maskContainsReticle = state.maskContainsReticle
        status = state.status
    }
}

struct GameplayShotEvent: Identifiable, Equatable, Sendable {
    let id: Int
    let targeting: GameplayTargetingState
}

struct AppearanceProfile: Codable, Equatable, Sendable {
    let playerId: String
    let displayName: String
    let generatedDescription: String
    let embeddingModel: String
    let descriptorModel: String
    let wholeBodyEmbedding: [Float]
    let faceEmbeddings: [[Float]]
    let upperBodyEmbeddings: [[Float]]
    let lowerBodyEmbeddings: [[Float]]
    let headAccessoryEmbeddings: [[Float]]
    let silhouetteDescriptor: [Float]
    let briefingThumbnail: String?
    /// Raw wire value for the player's chosen silhouette skin — read
    /// ``silhouetteSkin`` instead.
    ///
    /// Stored as a string rather than as `SilhouetteSkin?` so a value written
    /// by a newer client neither fails the decode nor gets silently dropped
    /// when this build re-uploads the profile.
    let skin: String?
    let updatedAtMs: UInt64

    init(
        playerId: String,
        displayName: String,
        generatedDescription: String,
        embeddingModel: String,
        descriptorModel: String,
        wholeBodyEmbedding: [Float],
        faceEmbeddings: [[Float]],
        upperBodyEmbeddings: [[Float]],
        lowerBodyEmbeddings: [[Float]],
        headAccessoryEmbeddings: [[Float]],
        silhouetteDescriptor: [Float],
        briefingThumbnail: String?,
        skin: String? = nil,
        updatedAtMs: UInt64
    ) {
        self.playerId = playerId
        self.displayName = displayName
        self.generatedDescription = generatedDescription
        self.embeddingModel = embeddingModel
        self.descriptorModel = descriptorModel
        self.wholeBodyEmbedding = wholeBodyEmbedding
        self.faceEmbeddings = faceEmbeddings
        self.upperBodyEmbeddings = upperBodyEmbeddings
        self.lowerBodyEmbeddings = lowerBodyEmbeddings
        self.headAccessoryEmbeddings = headAccessoryEmbeddings
        self.silhouetteDescriptor = silhouetteDescriptor
        self.briefingThumbnail = briefingThumbnail
        self.skin = skin
        self.updatedAtMs = updatedAtMs
    }

    /// The chosen skin, or `nil` when none is recorded or the recorded value is
    /// unknown to this build. Render `silhouetteSkin ?? .fallback`.
    var silhouetteSkin: SilhouetteSkin? {
        skin.flatMap(SilhouetteSkin.init(rawValue:))
    }

    /// Returns a copy carrying `newSkin`. Changing a skin re-uploads the
    /// existing profile rather than re-running enrollment, so the source photos
    /// never have to be taken again.
    func withSkin(_ newSkin: SilhouetteSkin?) -> AppearanceProfile {
        AppearanceProfile(
            playerId: playerId,
            displayName: displayName,
            generatedDescription: generatedDescription,
            embeddingModel: embeddingModel,
            descriptorModel: descriptorModel,
            wholeBodyEmbedding: wholeBodyEmbedding,
            faceEmbeddings: faceEmbeddings,
            upperBodyEmbeddings: upperBodyEmbeddings,
            lowerBodyEmbeddings: lowerBodyEmbeddings,
            headAccessoryEmbeddings: headAccessoryEmbeddings,
            silhouetteDescriptor: silhouetteDescriptor,
            briefingThumbnail: briefingThumbnail,
            skin: newSkin?.rawValue,
            updatedAtMs: updatedAtMs
        )
    }

    var isValid: Bool {
        !playerId.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !generatedDescription.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && wholeBodyEmbedding.count == appearanceEmbeddingDimensions
            && wholeBodyEmbedding.allSatisfy(\.isFinite)
    }
}

struct PlayerPresence: Codable, Equatable, Sendable {
    let playerId: String
    let latitude: Double
    let longitude: Double
    let horizontalAccuracy: Double
    let foreground: Bool
    let updatedAtMs: UInt64
}

enum MultiplayerMatchStatus: String, Codable, Equatable, Sendable {
    case lobby
    case briefing
    case active
    case completed
}

struct PlayerMatchState: Codable, Equatable, Sendable {
    let playerId: String
    let health: Int
    let ready: Bool
    let eliminated: Bool
    let calibrationModelVersion: String?
    let briefingAcknowledged: Bool?

    init(
        playerId: String,
        health: Int,
        ready: Bool,
        eliminated: Bool,
        calibrationModelVersion: String? = nil,
        briefingAcknowledged: Bool? = nil
    ) {
        self.playerId = playerId
        self.health = health
        self.ready = ready
        self.eliminated = eliminated
        self.calibrationModelVersion = calibrationModelVersion
        self.briefingAcknowledged = briefingAcknowledged
    }
}

struct MultiplayerMatchSnapshot: Codable, Equatable, Sendable {
    let protocolVersion: Int
    let revision: UInt64
    let matchId: String
    let inviteCode: String
    let inviteExpiresAtMs: UInt64?
    let status: MultiplayerMatchStatus
    let players: [PlayerMatchState]
    let winner: String?
    let createdAtMs: UInt64?
    let startedAtMs: UInt64?
    let completedAtMs: UInt64?
    let updatedAtMs: UInt64

    init(
        protocolVersion: Int,
        revision: UInt64,
        matchId: String,
        inviteCode: String,
        inviteExpiresAtMs: UInt64? = nil,
        status: MultiplayerMatchStatus,
        players: [PlayerMatchState],
        winner: String?,
        createdAtMs: UInt64? = nil,
        startedAtMs: UInt64? = nil,
        completedAtMs: UInt64? = nil,
        updatedAtMs: UInt64
    ) {
        self.protocolVersion = protocolVersion
        self.revision = revision
        self.matchId = matchId
        self.inviteCode = inviteCode
        self.inviteExpiresAtMs = inviteExpiresAtMs
        self.status = status
        self.players = players
        self.winner = winner
        self.createdAtMs = createdAtMs
        self.startedAtMs = startedAtMs
        self.completedAtMs = completedAtMs
        self.updatedAtMs = updatedAtMs
    }

    func player(_ id: String) -> PlayerMatchState? {
        players.first { $0.playerId == id }
    }
}

enum MultiplayerShotFeedback {
    static func message(accepted: Bool, reason: String) -> String {
        guard !accepted else { return "HIT" }
        let detail = switch reason {
        case "reticle_outside_target": "AIM OUTSIDE TARGET"
        case "target_lock_too_weak": "IDENTITY TOO WEAK"
        case "missing_reciprocal_proximity": "PROXIMITY NOT READY"
        default: reason.replacingOccurrences(of: "_", with: " ").uppercased()
        }
        return "MISS · \(detail)"
    }

    static func shouldClear(
        previous: MultiplayerMatchSnapshot?,
        next: MultiplayerMatchSnapshot
    ) -> Bool {
        guard let previous else { return true }
        return previous.matchId != next.matchId
            || (previous.status != .active && next.status == .active)
    }
}

enum MultiplayerClientMessage: Encodable, Equatable, Sendable {
    case heartbeat(commandId: UUID)
    case presence(commandId: UUID, presence: PlayerPresence)
    case ready(commandId: UUID, matchId: String)
    case readyWithMetadata(commandId: UUID, matchId: String, calibrationModelVersion: String)
    case briefingAcknowledged(commandId: UUID, matchId: String)
    case nearbyToken(commandId: UUID, matchId: String, peerId: String, token: String)
    case proximity(
        commandId: UUID,
        matchId: String,
        peerId: String,
        distanceMeters: Float?,
        direction: [Float]?,
        sampledAtMs: UInt64
    )
    case shot(
        commandId: UUID,
        matchId: String,
        targetId: String,
        reticle: [Float],
        maskContainsReticle: Bool,
        targetScore: Float,
        firedAtMs: UInt64
    )

    private enum CodingKeys: String, CodingKey {
        case type, commandId, presence, matchId, peerId, token, distanceMeters, direction
        case sampledAtMs, targetId, reticle, maskContainsReticle, targetScore, firedAtMs
        case calibrationModelVersion
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .heartbeat(let commandId):
            try container.encode("heartbeat", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
        case .presence(let commandId, let presence):
            try container.encode("presence", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(presence, forKey: .presence)
        case .ready(let commandId, let matchId):
            try container.encode("ready", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(matchId, forKey: .matchId)
        case .readyWithMetadata(let commandId, let matchId, let calibrationModelVersion):
            try container.encode("ready_with_metadata", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(matchId, forKey: .matchId)
            try container.encode(calibrationModelVersion, forKey: .calibrationModelVersion)
        case .briefingAcknowledged(let commandId, let matchId):
            try container.encode("briefing_acknowledged", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(matchId, forKey: .matchId)
        case .nearbyToken(let commandId, let matchId, let peerId, let token):
            try container.encode("nearby_token", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(matchId, forKey: .matchId)
            try container.encode(peerId, forKey: .peerId)
            try container.encode(token, forKey: .token)
        case .proximity(let commandId, let matchId, let peerId, let distanceMeters, let direction, let sampledAtMs):
            try container.encode("proximity", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(matchId, forKey: .matchId)
            try container.encode(peerId, forKey: .peerId)
            try container.encodeIfPresent(distanceMeters, forKey: .distanceMeters)
            try container.encodeIfPresent(direction, forKey: .direction)
            try container.encode(sampledAtMs, forKey: .sampledAtMs)
        case .shot(let commandId, let matchId, let targetId, let reticle, let maskContainsReticle, let targetScore, let firedAtMs):
            try container.encode("shot", forKey: .type)
            try container.encode(commandId, forKey: .commandId)
            try container.encode(matchId, forKey: .matchId)
            try container.encode(targetId, forKey: .targetId)
            try container.encode(reticle, forKey: .reticle)
            try container.encode(maskContainsReticle, forKey: .maskContainsReticle)
            try container.encode(targetScore, forKey: .targetScore)
            try container.encode(firedAtMs, forKey: .firedAtMs)
        }
    }
}

enum MultiplayerServerMessage: Decodable, Equatable, Sendable {
    case hello(playerId: String, revision: UInt64)
    case matchSnapshot(MultiplayerMatchSnapshot)
    case socialRevision(revision: UInt64)
    case invitationRevision(revision: UInt64)
    case nearbyToken(playerId: String, token: String)
    case shotResolution(commandId: UUID, accepted: Bool, reason: String, snapshot: MultiplayerMatchSnapshot?)
    case error(String)

    private enum CodingKeys: String, CodingKey {
        case type, playerId, revision, snapshot, token, commandId, accepted, reason, message
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .type) {
        case "hello":
            self = .hello(
                playerId: try container.decode(String.self, forKey: .playerId),
                revision: try container.decode(UInt64.self, forKey: .revision)
            )
        case "match_snapshot":
            self = .matchSnapshot(try container.decode(MultiplayerMatchSnapshot.self, forKey: .snapshot))
        case "social_revision":
            self = .socialRevision(revision: try container.decode(UInt64.self, forKey: .revision))
        case "invitation_revision":
            self = .invitationRevision(revision: try container.decode(UInt64.self, forKey: .revision))
        case "nearby_token":
            self = .nearbyToken(
                playerId: try container.decode(String.self, forKey: .playerId),
                token: try container.decode(String.self, forKey: .token)
            )
        case "shot_resolution":
            self = .shotResolution(
                commandId: try container.decode(UUID.self, forKey: .commandId),
                accepted: try container.decode(Bool.self, forKey: .accepted),
                reason: try container.decode(String.self, forKey: .reason),
                snapshot: try container.decodeIfPresent(MultiplayerMatchSnapshot.self, forKey: .snapshot)
            )
        case "error":
            self = .error(try container.decode(String.self, forKey: .message))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .type,
                in: container,
                debugDescription: "Unknown multiplayer server message"
            )
        }
    }
}

extension UInt64 {
    static var currentMilliseconds: UInt64 {
        UInt64((Date().timeIntervalSince1970 * 1_000).rounded())
    }
}
