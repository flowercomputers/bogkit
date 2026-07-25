import Foundation

struct LandmarkRecordingFrame: Codable, Equatable, Sendable {
    let timestamp: TimeInterval
    let hand: TrackedHand?
    let observation: FingerGunObservation?
    let aim: AimSolution?
    let calibration: AimCalibration?
    var analysis: FingerGunAnalysis? = nil
    var visionAnalysis: VisionFingerGunAnalysis? = nil
    var visionCalibration: VisionAimCalibration? = nil
    var gestureState: GestureState? = nil
    var fired: Bool? = nil
    var flashPoint: CGPoint? = nil
    var gameplayShot: GameplayShotDiagnostic? = nil
    var aimingMode: AimingMode? = nil
    var scopeProximity: ScopeProximityDiagnostic? = nil
    var nearbyInteraction: NearbyInteractionRecordingDiagnostic? = nil
}

struct NearbyInteractionRecordingDiagnostic: Codable, Equatable, Sendable {
    let status: String
    let distanceMeters: Float?
    let direction: [Float]?
    let sampledAtMs: UInt64?
}

enum LandmarkTrackerSource: String, Codable, Equatable, Sendable {
    case mediaPipe = "MEDIAPIPE"
    case vision = "VISION"
}

struct TrackerLandmarkSample: Codable, Equatable, Sendable {
    let timestamp: TimeInterval
    let source: LandmarkTrackerSource
    let hands: [TrackedHand]
    let latencyMilliseconds: Double
}

struct LandmarkRecording: Codable, Equatable, Sendable {
    let schemaVersion: Int
    let modelVersion: String
    let startedAt: Date
    var frames: [LandmarkRecordingFrame]
    var trackerSamples: [TrackerLandmarkSample]? = nil
}

enum LandmarkReplay {
    enum ReplayError: LocalizedError {
        case unsupportedSchema(Int)
        case incompatibleModel(String)

        var errorDescription: String? {
            switch self {
            case .unsupportedSchema(let version): return "Unsupported landmark recording schema \(version)."
            case .incompatibleModel(let model): return "Recording uses incompatible landmark model \(model)."
            }
        }
    }

    static func load(data: Data) throws -> LandmarkRecording {
        let recording = try JSONDecoder().decode(LandmarkRecording.self, from: data)
        guard (1...2).contains(recording.schemaVersion) else { throw ReplayError.unsupportedSchema(recording.schemaVersion) }
        guard recording.modelVersion == AimCalibration.modelVersion else {
            throw ReplayError.incompatibleModel(recording.modelVersion)
        }
        return recording
    }

    static func load(url: URL) throws -> LandmarkRecording { try load(data: Data(contentsOf: url)) }

    static func replay(_ recording: LandmarkRecording, handler: (LandmarkRecordingFrame) throws -> Void) rethrows {
        for frame in recording.frames.sorted(by: { $0.timestamp < $1.timestamp }) { try handler(frame) }
    }
}

final class DiagnosticRecorder: @unchecked Sendable {
    private(set) var recording: LandmarkRecording?
    var isRecording: Bool { recording != nil }
    private let outputDirectory: URL?

    init(outputDirectory: URL? = nil) {
        self.outputDirectory = outputDirectory
    }

    func start() {
        recording = LandmarkRecording(
            schemaVersion: 2,
            modelVersion: AimCalibration.modelVersion,
            startedAt: Date(),
            frames: [],
            trackerSamples: []
        )
    }

    func append(_ frame: LandmarkRecordingFrame) {
        recording?.frames.append(frame)
    }

    func appendTrackerSample(_ sample: TrackerLandmarkSample) {
        recording?.trackerSamples?.append(sample)
    }

    func stop() throws -> URL? {
        guard let recording else { return nil }
        self.recording = nil
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let filename = "finger-gun-\(formatter.string(from: recording.startedAt).replacingOccurrences(of: ":", with: "-"))-landmarks.json"
        let directory = try outputDirectory ?? Self.defaultOutputDirectory()
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        let url = directory.appendingPathComponent(filename)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(recording).write(to: url, options: .atomic)
        return url
    }

    static func latestRecordingURL(
        in directory: URL? = nil,
        fileManager: FileManager = .default
    ) -> URL? {
        let directory = try? directory ?? defaultOutputDirectory()
        guard let directory,
              let urls = try? fileManager.contentsOfDirectory(
                at: directory,
                includingPropertiesForKeys: [.contentModificationDateKey],
                options: [.skipsHiddenFiles]
              ) else {
            return nil
        }
        return urls
            .filter {
                $0.lastPathComponent.hasPrefix("finger-gun-")
                    && $0.lastPathComponent.hasSuffix("-landmarks.json")
            }
            .max {
                let lhs = try? $0.resourceValues(
                    forKeys: [.contentModificationDateKey]
                ).contentModificationDate
                let rhs = try? $1.resourceValues(
                    forKeys: [.contentModificationDateKey]
                ).contentModificationDate
                return (lhs ?? .distantPast) < (rhs ?? .distantPast)
            }
    }

    private static func defaultOutputDirectory() throws -> URL {
        try FileManager.default.url(
            for: .documentDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
    }
}
