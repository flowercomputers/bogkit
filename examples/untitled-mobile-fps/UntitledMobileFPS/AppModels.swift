import Foundation

struct ServerEndpoint: Codable, Equatable, Hashable, Identifiable, Sendable {
    let serverId: String?
    let displayName: String
    let url: URL

    var id: String { serverId ?? canonicalAddress }
    var canonicalAddress: String { Self.canonicalAddress(for: url) }

    init(serverId: String? = nil, displayName: String, url: URL) {
        self.serverId = serverId
        self.displayName = displayName
        self.url = url
    }

    static func parse(_ address: String, displayName: String = "Custom server") -> ServerEndpoint? {
        let trimmed = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard var components = URLComponents(string: trimmed),
              let scheme = components.scheme?.lowercased(),
              scheme == "http" || scheme == "https",
              components.host != nil else { return nil }
        components.scheme = scheme
        components.host = components.host?.lowercased()
        components.fragment = nil
        components.query = nil
        components.path = components.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard components.path.isEmpty else { return nil }
        components.path = ""
        guard let url = components.url else { return nil }
        return ServerEndpoint(displayName: displayName, url: url)
    }

    static func canonicalAddress(for url: URL) -> String {
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            return url.absoluteString
        }
        components.scheme = components.scheme?.lowercased()
        components.host = components.host?.lowercased()
        components.fragment = nil
        components.query = nil
        if (components.scheme == "http" && components.port == 80)
            || (components.scheme == "https" && components.port == 443) {
            components.port = nil
        }
        components.path = components.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if !components.path.isEmpty { components.path = "/" + components.path }
        return components.string?.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            ?? url.absoluteString
    }

    var allowsInsecureDevelopmentTransport: Bool {
        guard url.scheme == "http", let host = url.host?.lowercased() else { return false }
        if host == "localhost" || host == "127.0.0.1" || host == "::1" { return true }
        let octets = host.split(separator: ".").compactMap { Int($0) }
        guard octets.count == 4 else { return false }
        return octets[0] == 10
            || (octets[0] == 172 && (16...31).contains(octets[1]))
            || (octets[0] == 192 && octets[1] == 168)
    }
}

struct ServerInfo: Codable, Equatable, Sendable {
    let serverId: String
    let displayName: String
    let environment: String
    let protocolVersion: Int
    let capabilities: [String]
    let minimumClientVersion: String?
}

enum AccountAppearanceStatus: String, Codable, Equatable, Sendable {
    case missing
    case registered
}

struct PlayerAccount: Codable, Equatable, Identifiable, Sendable {
    let playerId: String
    let handle: String
    let displayName: String
    let appearanceStatus: AccountAppearanceStatus
    let createdAtMs: UInt64
    let updatedAtMs: UInt64

    var id: String { playerId }
}

struct AccountRegistration: Codable, Equatable, Sendable {
    let handle: String
    let displayName: String
}

struct AccountEnvelope: Codable, Equatable, Sendable {
    let account: PlayerAccount
    let token: String?
}

enum SetupRequirement: String, CaseIterable, Equatable, Sendable {
    case connection
    case account
    case bodyAppearance
    case faceAppearance
    case calibration

    var title: String {
        switch self {
        case .connection: "Connect to a server"
        case .account: "Create your player"
        case .bodyAppearance: "Scan your outfit"
        case .faceAppearance: "Capture your briefing photo"
        case .calibration: "Calibrate your finger gun"
        }
    }
}

struct MatchReadiness: Equatable, Sendable {
    var connected: Bool
    var registered: Bool
    var hasBodyAppearance: Bool
    var hasFaceAppearance: Bool
    var calibrated: Bool

    var missingRequirements: [SetupRequirement] {
        var result: [SetupRequirement] = []
        if !connected { result.append(.connection) }
        if !registered { result.append(.account) }
        if !hasBodyAppearance { result.append(.bodyAppearance) }
        if !hasFaceAppearance { result.append(.faceAppearance) }
        if !calibrated { result.append(.calibration) }
        return result
    }

    var canEnterMatch: Bool { missingRequirements.isEmpty }
}

enum FriendshipStatus: String, Codable, Equatable, Sendable {
    case pending
    case accepted
    case declined
    case removed
}

struct FriendSummary: Codable, Equatable, Identifiable, Sendable {
    let playerId: String
    let handle: String
    let displayName: String
    let available: Bool
    let lastSeenAtMs: UInt64?

    var id: String { playerId }
}

struct FriendRequestSummary: Codable, Equatable, Identifiable, Sendable {
    let requestId: String
    let sender: FriendSummary
    let status: FriendshipStatus
    let createdAtMs: UInt64

    var id: String { requestId }
}

enum MatchInvitationStatus: String, Codable, Equatable, Sendable {
    case pending
    case accepted
    case declined
    case cancelled
    case expired
}

struct MatchInvitation: Codable, Equatable, Identifiable, Sendable {
    let invitationId: String
    let fromPlayerId: String
    let toPlayerId: String
    let matchId: String
    let status: MatchInvitationStatus
    let createdAtMs: UInt64
    let expiresAtMs: UInt64
    let updatedAtMs: UInt64

    var id: String { invitationId }
}

struct PlayerSearchResult: Codable, Equatable, Identifiable, Sendable {
    let playerId: String
    let handle: String
    let displayName: String

    var id: String { playerId }
}

enum MatchResult: String, Codable, Equatable, Sendable {
    case won
    case lost
    case draw
}

struct MatchHistoryParticipant: Codable, Equatable, Identifiable, Sendable {
    let playerId: String
    let handle: String?
    let displayName: String
    let hitTotal: Int
    let winner: Bool

    var id: String { playerId }
}

struct MatchHistorySummary: Codable, Equatable, Identifiable, Sendable {
    let matchId: String
    let result: MatchResult
    let opponent: MatchHistoryParticipant
    let startedAtMs: UInt64
    let completedAtMs: UInt64
    let myHitTotal: Int

    var id: String { matchId }
    var durationSeconds: Int { Int((completedAtMs - min(startedAtMs, completedAtMs)) / 1_000) }
}

struct MatchHistoryEvent: Codable, Equatable, Identifiable, Sendable {
    let eventId: String
    let type: String
    let playerId: String?
    let timestampMs: UInt64
    let detail: String?

    var id: String { eventId }
}

struct MatchHistoryDetail: Codable, Equatable, Identifiable, Sendable {
    let matchId: String
    let result: MatchResult
    let participants: [MatchHistoryParticipant]
    let startedAtMs: UInt64
    let completedAtMs: UInt64
    let events: [MatchHistoryEvent]

    var id: String { matchId }
}

struct MatchHistoryPage: Codable, Equatable, Sendable {
    let matches: [MatchHistorySummary]
    let nextCursor: String?
}

enum LoadState<Value> {
    case idle
    case loading
    case loaded(Value)
    case failed(String)
}

extension LoadState {
    var value: Value? {
        guard case .loaded(let value) = self else { return nil }
        return value
    }
}
