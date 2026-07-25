import Foundation
import Network

struct DemoSession: Codable, Equatable, Sendable {
    let playerId: String
    let token: String
    let displayName: String
}

enum MatchClientError: LocalizedError {
    case invalidServerURL
    case invalidResponse
    case incompatibleServer(Int)
    case incompatibleCalibrationModel(String?)
    case transport(server: String, code: URLError.Code)
    case server(status: Int, message: String)
    case disconnected

    var errorDescription: String? {
        switch self {
        case .invalidServerURL: return "Enter a valid server URL, such as http://192.168.1.4:3000."
        case .invalidResponse: return "The game server returned an unreadable response."
        case .incompatibleServer(let version):
            return "The game server uses protocol \(version), but this app expects \(multiplayerProtocolVersion)."
        case .incompatibleCalibrationModel(let version):
            return "The server requires calibration model \(version ?? "unknown"), but this app uses \(VisionAimCalibration.modelVersion)."
        case .transport(let server, let code):
            switch code {
            case .timedOut:
                return "Timed out connecting directly to \(server). Open \(server)/health in Safari on this phone, then check iOS Local Network access."
            case .cannotConnectToHost, .cannotFindHost, .networkConnectionLost:
                return "Cannot reach \(server). Confirm the server is running, the phone and Mac share Wi-Fi, and \(server)/health opens in Safari."
            case .notConnectedToInternet:
                return "The phone has no route to \(server). Check Wi-Fi and Settings › Privacy & Security › Local Network."
            case .appTransportSecurityRequiresSecureConnection:
                return "iOS blocked the HTTP server. Reinstall the current development build so its local-network transport settings take effect."
            default:
                return "Network error connecting to \(server): \(code.rawValue)."
            }
        case .server(let status, let message): return "Server \(status): \(message)"
        case .disconnected: return "The realtime connection is not active."
        }
    }
}

@MainActor
final class RealtimeMatchClient: ObservableObject {
    @Published private(set) var connected = false
    var onMessage: ((MultiplayerServerMessage) -> Void)?
    var onDisconnect: ((Error?) -> Void)?

    private let session: URLSession
    private var socket: URLSessionWebSocketTask?
    private var localNetworkBrowser: NWBrowser?
    private var localNetworkBrowserCleanup: Task<Void, Never>?
    private var currentRealtimeTicket: String?
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 8
        configuration.timeoutIntervalForResource = 15
        configuration.waitsForConnectivity = false
        // The match service is a peer on the current LAN. Sending its private
        // address through an auto-configured Wi-Fi proxy causes a silent timeout.
        configuration.connectionProxyDictionary = [:]
        session = URLSession(configuration: configuration)
    }

    func requestLocalNetworkAccess() {
        guard localNetworkBrowser == nil else { return }
        let parameters = NWParameters()
        parameters.includePeerToPeer = true
        let browser = NWBrowser(
            for: .bonjour(type: "_untitledfps._tcp", domain: nil),
            using: parameters
        )
        browser.stateUpdateHandler = { [weak self] state in
            Task { @MainActor [weak self] in
                switch state {
                case .ready, .failed:
                    self?.stopLocalNetworkBrowser()
                default:
                    break
                }
            }
        }
        localNetworkBrowser = browser
        browser.start(queue: DispatchQueue(label: "multiplayer.local-network-authorization"))
        localNetworkBrowserCleanup = Task { [weak self] in
            try? await Task.sleep(for: .seconds(20))
            guard !Task.isCancelled else { return }
            self?.stopLocalNetworkBrowser()
        }
    }

    func checkServer(baseURL: URL) async throws -> ServerInfo {
        let response: ServerHealth = try await request(
            baseURL: baseURL,
            path: "/health",
            method: "GET",
            token: nil,
            body: Optional<EmptyBody>.none
        )
        guard response.status == "ok", response.protocolVersion == multiplayerProtocolVersion else {
            throw MatchClientError.incompatibleServer(response.protocolVersion)
        }
        guard let serverInfo = response.serverInfo else { throw MatchClientError.invalidResponse }
        let requiredCalibration = serverInfo.capabilities
            .first(where: { $0.hasPrefix("calibrationModel:") })
            .map { String($0.dropFirst("calibrationModel:".count)) }
        guard requiredCalibration == VisionAimCalibration.modelVersion else {
            throw MatchClientError.incompatibleCalibrationModel(requiredCalibration)
        }
        return serverInfo
    }

    func createAccount(baseURL: URL, registration: AccountRegistration) async throws -> AccountEnvelope {
        let response: AccountRegistrationResponse = try await request(
            baseURL: baseURL,
            path: "/v1/accounts",
            method: "POST",
            token: nil,
            body: registration
        )
        return AccountEnvelope(account: response.account, token: response.token)
    }

    func fetchMe(baseURL: URL, token: String) async throws -> PlayerAccount {
        try await request(
            baseURL: baseURL,
            path: "/v1/me",
            method: "GET",
            token: token,
            body: Optional<EmptyBody>.none
        )
    }

    func uploadAppearance(baseURL: URL, token: String, profile: AppearanceProfile) async throws -> AppearanceProfile {
        try await request(baseURL: baseURL, path: "/v1/me/appearance", method: "PUT", token: token, body: profile)
    }

    func createInvite(baseURL: URL, token: String) async throws -> MultiplayerMatchSnapshot {
        let response: InviteEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/invites",
            method: "POST",
            token: token,
            body: EmptyBody()
        )
        return response.snapshot
    }

    func challenge(baseURL: URL, token: String, playerId: String) async throws -> MultiplayerMatchSnapshot {
        let response: MatchInvitationResolutionEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/match-invitations",
            method: "POST",
            token: token,
            body: ["friendId": playerId]
        )
        guard let snapshot = response.snapshot else { throw MatchClientError.invalidResponse }
        return snapshot
    }

    func joinInvite(baseURL: URL, code: String, token: String) async throws -> MultiplayerMatchSnapshot {
        let response: InviteEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/invites/\(code.uppercased())/join",
            method: "POST",
            token: token,
            body: EmptyBody()
        )
        return response.snapshot
    }

    func fetchAppearance(baseURL: URL, token: String, playerId: String) async throws -> AppearanceProfile {
        let body: EmptyBody? = nil
        return try await request(
            baseURL: baseURL,
            path: "/v1/players/\(playerId)/appearance",
            method: "GET",
            token: token,
            body: body
        )
    }

    func fetchFriends(baseURL: URL, token: String) async throws -> [FriendSummary] {
        let response: FriendsEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/me/friends",
            method: "GET",
            token: token,
            body: Optional<EmptyBody>.none
        )
        return response.friends
    }

    func fetchFriendRequests(baseURL: URL, token: String) async throws -> [FriendRequestSummary] {
        let response: FriendRequestsEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/me/friend-requests",
            method: "GET",
            token: token,
            body: Optional<EmptyBody>.none
        )
        return response.requests
    }

    func findPlayer(baseURL: URL, token: String, handle: String) async throws -> PlayerSearchResult? {
        guard let encoded = handle.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) else {
            throw MatchClientError.invalidResponse
        }
        do {
            let response: PlayerEnvelope = try await request(
                baseURL: baseURL,
                path: "/v1/players?handle=\(encoded)",
                method: "GET",
                token: token,
                body: Optional<EmptyBody>.none
            )
            let account = response.player
            return PlayerSearchResult(
                playerId: account.playerId,
                handle: account.handle,
                displayName: account.displayName
            )
        } catch MatchClientError.server(let status, _) where status == 404 {
            return nil
        }
    }

    func sendFriendRequest(baseURL: URL, token: String, playerId: String) async throws {
        let _: FriendRequestWire = try await request(
            baseURL: baseURL,
            path: "/v1/me/friend-requests",
            method: "POST",
            token: token,
            body: ["playerId": playerId]
        )
    }

    func resolveFriendRequest(
        baseURL: URL,
        token: String,
        requestId: String,
        accept: Bool
    ) async throws {
        let action = accept ? "accept" : "decline"
        let _: FriendRequestWire = try await request(
            baseURL: baseURL,
            path: "/v1/me/friend-requests/\(requestId)/\(action)",
            method: "POST",
            token: token,
            body: EmptyBody()
        )
    }

    func removeFriend(baseURL: URL, token: String, playerId: String) async throws {
        let _: EmptyBody = try await request(
            baseURL: baseURL,
            path: "/v1/me/friends/\(playerId)",
            method: "DELETE",
            token: token,
            body: Optional<EmptyBody>.none
        )
    }

    func fetchMatchInvitations(baseURL: URL, token: String) async throws -> [MatchInvitation] {
        try await request(
            baseURL: baseURL,
            path: "/v1/me/match-invitations",
            method: "GET",
            token: token,
            body: Optional<EmptyBody>.none
        )
    }

    func publishAvailability(
        baseURL: URL,
        token: String,
        playerId: String
    ) async throws {
        let _: EmptyBody = try await request(
            baseURL: baseURL,
            path: "/v1/me/presence",
            method: "PUT",
            token: token,
            body: PlayerPresence(
                playerId: playerId,
                latitude: 0,
                longitude: 0,
                horizontalAccuracy: -1,
                foreground: true,
                updatedAtMs: .currentMilliseconds
            )
        )
    }

    /// Publishes availability with a real coordinate so the server's presence HNSW can
    /// pair the player with a nearby opponent. The regular heartbeat stays location-free;
    /// this is sent only for an explicit Quick Match request.
    func publishLocatedAvailability(
        baseURL: URL,
        token: String,
        playerId: String,
        latitude: Double,
        longitude: Double,
        accuracy: Double
    ) async throws {
        let _: EmptyBody = try await request(
            baseURL: baseURL,
            path: "/v1/me/presence",
            method: "PUT",
            token: token,
            body: PlayerPresence(
                playerId: playerId,
                latitude: latitude,
                longitude: longitude,
                horizontalAccuracy: accuracy,
                foreground: true,
                updatedAtMs: .currentMilliseconds
            )
        )
    }

    /// Requests a random-nearby match. Returns a one-player lobby the caller now hosts
    /// (still waiting) or a two-player lobby if an opponent was already waiting nearby.
    func matchNearby(baseURL: URL, token: String) async throws -> MultiplayerMatchSnapshot {
        let response: InviteEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/match/nearby",
            method: "POST",
            token: token,
            body: EmptyBody()
        )
        return response.snapshot
    }

    func clearAvailability(baseURL: URL, token: String) async throws {
        let _: EmptyBody = try await request(
            baseURL: baseURL,
            path: "/v1/me/presence",
            method: "DELETE",
            token: token,
            body: Optional<EmptyBody>.none
        )
    }

    func resolveMatchInvitation(
        baseURL: URL,
        token: String,
        invitationId: String,
        accept: Bool
    ) async throws -> MultiplayerMatchSnapshot? {
        let action = accept ? "accept" : "decline"
        let response: MatchInvitationResolutionEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/match-invitations/\(invitationId)/\(action)",
            method: "POST",
            token: token,
            body: EmptyBody()
        )
        return response.snapshot
    }

    func fetchMatch(baseURL: URL, token: String, matchId: String) async throws -> MultiplayerMatchSnapshot {
        try await request(
            baseURL: baseURL,
            path: "/v1/matches/\(matchId)",
            method: "GET",
            token: token,
            body: Optional<EmptyBody>.none
        )
    }

    func fetchMatchHistory(
        baseURL: URL,
        token: String,
        cursor: String? = nil,
        limit: Int = 25
    ) async throws -> MatchHistoryPage {
        var path = "/v1/me/matches?limit=\(limit)"
        if let cursor, let encoded = cursor.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) {
            path += "&cursor=\(encoded)"
        }
        return try await request(
            baseURL: baseURL,
            path: path,
            method: "GET",
            token: token,
            body: Optional<EmptyBody>.none
        )
    }

    func fetchMatchDetail(
        baseURL: URL,
        token: String,
        matchId: String
    ) async throws -> MatchHistoryDetail {
        try await request(
            baseURL: baseURL,
            path: "/v1/me/matches/\(matchId)",
            method: "GET",
            token: token,
            body: Optional<EmptyBody>.none
        )
    }

    func prepareRealtime(baseURL: URL, token: String, matchId: String) async throws {
        let response: RealtimeTicketEnvelope = try await request(
            baseURL: baseURL,
            path: "/v1/realtime/tickets",
            method: "POST",
            token: token,
            body: ["matchId": matchId]
        )
        currentRealtimeTicket = response.ticket
    }

    func connect(baseURL: URL, matchId: String) throws {
        let ticket = currentRealtimeTicket
        disconnect()
        guard let ticket else { throw MatchClientError.disconnected }
        guard var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false) else {
            throw MatchClientError.invalidServerURL
        }
        components.scheme = components.scheme == "https" ? "wss" : "ws"
        components.path = "/v1/realtime"
        components.queryItems = [
            URLQueryItem(name: "ticket", value: ticket),
            URLQueryItem(name: "matchId", value: matchId)
        ]
        guard let url = components.url else { throw MatchClientError.invalidServerURL }
        let socket = session.webSocketTask(with: url)
        self.socket = socket
        socket.resume()
        connected = true
        receiveNext()
    }

    func send(_ message: MultiplayerClientMessage) async throws {
        guard let socket else { throw MatchClientError.disconnected }
        let data = try encoder.encode(message)
        guard let text = String(data: data, encoding: .utf8) else { throw MatchClientError.invalidResponse }
        try await socket.send(.string(text))
    }

    func disconnect() {
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        connected = false
        currentRealtimeTicket = nil
    }

    private func stopLocalNetworkBrowser() {
        localNetworkBrowserCleanup?.cancel()
        localNetworkBrowserCleanup = nil
        localNetworkBrowser?.cancel()
        localNetworkBrowser = nil
    }

    private func receiveNext() {
        guard let socket else { return }
        Task { [weak self] in
            do {
                let value = try await socket.receive()
                guard let self else { return }
                let data: Data
                switch value {
                case .data(let payload): data = payload
                case .string(let text): data = Data(text.utf8)
                @unknown default: throw MatchClientError.invalidResponse
                }
                let message = try decoder.decode(MultiplayerServerMessage.self, from: data)
                onMessage?(message)
                receiveNext()
            } catch {
                guard let self, self.socket === socket else { return }
                connected = false
                self.socket = nil
                onDisconnect?(error)
            }
        }
    }

    private func request<Response: Decodable, Body: Encodable>(
        baseURL: URL,
        path: String,
        method: String,
        token: String?,
        body: Body?
    ) async throws -> Response {
        guard let url = URL(string: path, relativeTo: baseURL)?.absoluteURL else {
            throw MatchClientError.invalidServerURL
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let body { request.httpBody = try encoder.encode(body) }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch let error as URLError {
            var server = baseURL.absoluteString
            while server.hasSuffix("/") { server.removeLast() }
            throw MatchClientError.transport(server: server, code: error.code)
        }
        guard let http = response as? HTTPURLResponse else { throw MatchClientError.invalidResponse }
        guard (200..<300).contains(http.statusCode) else {
            throw MatchClientError.server(
                status: http.statusCode,
                message: String(data: data, encoding: .utf8) ?? "Unknown error"
            )
        }
        let payload = data.isEmpty ? Data("{}".utf8) : data
        return try decoder.decode(Response.self, from: payload)
    }
}

private struct InviteEnvelope: Codable {
    let snapshot: MultiplayerMatchSnapshot
}

private struct ServerHealth: Codable {
    let status: String
    let protocolVersion: Int
    let serverId: String?
    let displayName: String?
    let environment: String?
    let capabilities: [String]?
    let minimumClientVersion: String?

    var serverInfo: ServerInfo? {
        guard let serverId, !serverId.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }
        return ServerInfo(
            serverId: serverId,
            displayName: displayName ?? "Bogkit Server",
            environment: environment ?? "development",
            protocolVersion: protocolVersion,
            capabilities: capabilities ?? [],
            minimumClientVersion: minimumClientVersion
        )
    }
}

private struct PlayerEnvelope: Codable {
    let player: PlayerAccount
}

private struct AccountRegistrationResponse: Codable {
    let account: PlayerAccount
    let token: String?
}

private struct FriendsEnvelope: Codable {
    let friends: [FriendSummary]
}

private struct FriendRequestsEnvelope: Codable {
    let requests: [FriendRequestSummary]
}

private struct FriendRequestWire: Codable {
    let requestId: String
    let fromPlayerId: String
    let toPlayerId: String
    let status: FriendshipStatus
    let createdAtMs: UInt64
    let updatedAtMs: UInt64

    var summary: FriendRequestSummary {
        FriendRequestSummary(
            requestId: requestId,
            sender: FriendSummary(
                playerId: fromPlayerId,
                handle: String(fromPlayerId.prefix(12)),
                displayName: "Player \(fromPlayerId.prefix(6))",
                available: false,
                lastSeenAtMs: nil
            ),
            status: status,
            createdAtMs: createdAtMs
        )
    }
}

private struct MatchInvitationResolutionEnvelope: Codable {
    let invitation: MatchInvitation
    let snapshot: MultiplayerMatchSnapshot?
}

private struct RealtimeTicketEnvelope: Codable {
    let ticket: String
}

private struct EmptyBody: Codable {}
