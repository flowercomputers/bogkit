import Foundation
import UIKit

enum AppStage: Equatable {
    case connecting
    case serverSelection
    case registration
    case hub
}

enum AppCover: Identifiable, Equatable {
    case appearance
    case calibration
    case lobby
    case gameplay
    case result

    var id: String {
        switch self {
        case .appearance: "appearance"
        case .calibration: "calibration"
        case .lobby: "lobby"
        case .gameplay: "gameplay"
        case .result: "result"
        }
    }
}

enum HubTab: Hashable {
    case play
    case friends
    case history
    case profile
}

private struct EnrollmentCache: Codable {
    let profile: AppearanceProfile
    let bodyModel: String
    let faceModel: String
}

@MainActor
final class AppSession: ObservableObject {
    @Published private(set) var stage: AppStage = .serverSelection
    @Published private(set) var activeServer: ServerEndpoint?
    @Published private(set) var serverInfo: ServerInfo?
    @Published private(set) var account: PlayerAccount?
    @Published private(set) var credential: String?
    @Published private(set) var appearanceProfile: AppearanceProfile?
    @Published private(set) var calibrated = false
    @Published var cover: AppCover?
    @Published var selectedTab: HubTab = .play
    @Published private(set) var busy = false
    @Published private(set) var message: String?

    @Published private(set) var friendsState: LoadState<[FriendSummary]> = .idle
    @Published private(set) var requestsState: LoadState<[FriendRequestSummary]> = .idle
    @Published private(set) var invitationsState: LoadState<[MatchInvitation]> = .idle
    @Published private(set) var playerSearchState: LoadState<PlayerSearchResult?> = .idle
    @Published private(set) var historyState: LoadState<[MatchHistorySummary]> = .idle
    @Published private(set) var selectedHistoryState: LoadState<MatchHistoryDetail> = .idle
    @Published private(set) var loadingMoreHistory = false

    let camera = CameraService()
    let game = GameplayCoordinator()
    let defaultServer: ServerEndpoint?
    private(set) var recentServers: [ServerEndpoint]

    private let credentials: CredentialStoring
    private let analyzer = AppearanceAnalyzer()
    private var historyCursor: String?
    private var serverGeneration: UInt64 = 0
    private var availabilityTask: Task<Void, Never>?

    init(credentials: CredentialStoring = KeychainCredentialStore()) {
        self.credentials = credentials
        defaultServer = Self.configuredDefaultServer()
        recentServers = Self.loadRecents()
        calibrated = {
            if case .calibrated = camera.calibrationState { return true }
            return false
        }()
        game.realtime.onMessage = { [weak self, weak game] message in
            game?.receive(message)
            switch message {
            case .socialRevision:
                Task { await self?.refreshFriends() }
            case .invitationRevision:
                Task { await self?.refreshFriends() }
            default:
                break
            }
        }
    }

    var readiness: MatchReadiness {
        MatchReadiness(
            connected: activeServer != nil && serverInfo != nil,
            registered: account != nil && credential != nil,
            hasBodyAppearance: appearanceProfile != nil,
            hasFaceAppearance: appearanceProfile?.faceEmbeddings.isEmpty == false
                && appearanceProfile?.briefingThumbnail != nil,
            calibrated: calibrated
        )
    }

    var canSwitchServerImmediately: Bool { game.match == nil }
    var canLoadMoreHistory: Bool { historyCursor != nil && !loadingMoreHistory }

    func syncCalibration(_ state: CalibrationState) {
        let next: Bool
        if case .calibrated = state { next = true } else { next = false }
        if calibrated != next { calibrated = next }
    }

    func connect(to endpoint: ServerEndpoint) async {
        guard endpoint.url.scheme == "https" || endpoint.allowsInsecureDevelopmentTransport else {
            message = "Public servers must use HTTPS. HTTP is available only for localhost or private LAN servers."
            return
        }
        serverGeneration &+= 1
        let generation = serverGeneration
        stopAvailability(clearRemote: true)
        busy = true
        stage = .connecting
        message = "Connecting to \(endpoint.displayName)…"
        defer {
            if serverGeneration == generation { busy = false }
        }
        do {
            let info = try await game.realtime.checkServer(baseURL: endpoint.url)
            guard serverGeneration == generation else { return }
            let resolved = ServerEndpoint(
                serverId: info.serverId,
                displayName: info.displayName,
                url: endpoint.url
            )
            activeServer = resolved
            serverInfo = info
            remember(resolved)
            appearanceProfile = nil
            if let token = credentials.credential(for: info.serverId) {
                do {
                    let restoredAccount = try await game.realtime.fetchMe(baseURL: endpoint.url, token: token)
                    guard serverGeneration == generation else { return }
                    appearanceProfile = loadEnrollment(
                        serverId: info.serverId,
                        playerId: restoredAccount.playerId
                    )?.profile
                    finishAuthentication(account: restoredAccount, token: token)
                    return
                } catch MatchClientError.server(let status, _) where status == 401 {
                    guard serverGeneration == generation else { return }
                    credentials.removeCredential(for: info.serverId)
                    removeEnrollment(serverId: info.serverId)
                    appearanceProfile = nil
                } catch {
                    guard serverGeneration == generation else { return }
                    activeServer = nil
                    serverInfo = nil
                    stage = .serverSelection
                    message = "Could not restore this server account: \(error.localizedDescription)"
                    return
                }
            }
            guard serverGeneration == generation else { return }
            credential = nil
            account = nil
            stage = .registration
            message = "Choose a handle for this server."
        } catch {
            guard serverGeneration == generation else { return }
            activeServer = nil
            serverInfo = nil
            stage = .serverSelection
            message = error.localizedDescription
        }
    }

    func register(handle: String, displayName: String) async {
        guard let activeServer, let serverInfo else { return }
        let generation = serverGeneration
        busy = true
        message = "Creating @\(handle)…"
        defer {
            if serverGeneration == generation { busy = false }
        }
        do {
            let response = try await game.realtime.createAccount(
                baseURL: activeServer.url,
                registration: AccountRegistration(handle: handle, displayName: displayName)
            )
            guard serverGeneration == generation,
                  self.serverInfo?.serverId == serverInfo.serverId else { return }
            guard let token = response.token else { throw MatchClientError.invalidResponse }
            try credentials.setCredential(token, for: serverInfo.serverId)
            appearanceProfile = loadEnrollment(
                serverId: serverInfo.serverId,
                playerId: response.account.playerId
            )?.profile
            finishAuthentication(account: response.account, token: token)
        } catch {
            guard serverGeneration == generation else { return }
            message = error.localizedDescription
        }
    }

    func enrollAppearance(bodyImage: UIImage, faceImage: UIImage) async -> Bool {
        guard let activeServer, let account, let credential, let serverInfo else { return false }
        let generation = serverGeneration
        busy = true
        message = "Generating your outfit and briefing descriptors on this phone…"
        defer {
            if serverGeneration == generation { busy = false }
        }
        do {
            let analysis = try await analyzer.analyze(bodyImage: bodyImage, faceImage: faceImage)
            guard serverGeneration == generation else { return false }
            let candidate = analysis.profile(
                playerId: account.playerId,
                displayName: account.displayName,
                skin: preferredSkin
            )
            let profile = try await game.realtime.uploadAppearance(
                baseURL: activeServer.url,
                token: credential,
                profile: candidate
            )
            guard serverGeneration == generation,
                  self.account?.playerId == account.playerId,
                  self.credential == credential else { return false }
            appearanceProfile = profile
            saveEnrollment(
                EnrollmentCache(
                    profile: profile,
                    bodyModel: AppearanceAnalysis.embeddingModel,
                    faceModel: AppearanceAnalysis.descriptorModel
                ),
                serverId: serverInfo.serverId
            )
            game.setAppearanceProfile(profile)
            message = "Appearance ready. Source photos were discarded."
            return true
        } catch {
            guard serverGeneration == generation else { return false }
            message = error.localizedDescription
            return false
        }
    }

    /// The skin this player's silhouette is drawn with on opponents' phones.
    ///
    /// Kept locally as well as on the profile so a player can pick one before
    /// enrolling, and so the picker has something to show while offline.
    var preferredSkin: SilhouetteSkin {
        appearanceProfile?.silhouetteSkin
            ?? UserDefaults.standard.string(forKey: Self.preferredSkinKey)
                .flatMap(SilhouetteSkin.init(rawValue:))
            ?? .fallback
    }

    /// Changing a skin re-uploads the cached profile rather than re-running
    /// enrollment: the descriptors are unchanged, only the cosmetic differs, so
    /// the player never has to retake the source photos.
    func setPreferredSkin(_ skin: SilhouetteSkin) async {
        UserDefaults.standard.set(skin.rawValue, forKey: Self.preferredSkinKey)
        guard let activeServer, let credential, let serverInfo,
              let existing = appearanceProfile, existing.silhouetteSkin != skin else { return }
        let generation = serverGeneration
        busy = true
        message = "Updating silhouette skin…"
        defer {
            if serverGeneration == generation { busy = false }
        }
        do {
            let profile = try await game.realtime.uploadAppearance(
                baseURL: activeServer.url,
                token: credential,
                profile: existing.withSkin(skin)
            )
            guard serverGeneration == generation,
                  self.credential == credential else { return }
            appearanceProfile = profile
            saveEnrollment(
                EnrollmentCache(
                    profile: profile,
                    bodyModel: AppearanceAnalysis.embeddingModel,
                    faceModel: AppearanceAnalysis.descriptorModel
                ),
                serverId: serverInfo.serverId
            )
            game.setAppearanceProfile(profile)
            message = "Silhouette skin set to \(skin.displayName)."
        } catch {
            guard serverGeneration == generation else { return }
            message = error.localizedDescription
        }
    }

    private static let preferredSkinKey = "onboarding.silhouetteSkin"

    func open(_ requirement: SetupRequirement) {
        switch requirement {
        case .connection:
            stage = .serverSelection
        case .account:
            stage = .registration
        case .bodyAppearance, .faceAppearance:
            cover = .appearance
        case .calibration:
            cover = .calibration
        }
    }

    func refreshFriends(showLoading: Bool = true) async {
        guard let activeServer, let credential, let account,
              let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        if showLoading {
            friendsState = .loading
            requestsState = .loading
        }
        do {
            async let friends = game.realtime.fetchFriends(baseURL: activeServer.url, token: credential)
            async let requests = game.realtime.fetchFriendRequests(baseURL: activeServer.url, token: credential)
            async let invitations = game.realtime.fetchMatchInvitations(baseURL: activeServer.url, token: credential)
            let values = try await (friends, requests, invitations)
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            friendsState = .loaded(values.0)
            requestsState = .loaded(values.1.filter {
                $0.status == .pending && $0.sender.playerId != account.playerId
            })
            invitationsState = .loaded(values.2.filter {
                $0.status == .pending && $0.toPlayerId == account.playerId
            })
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            if !showLoading { return }
            friendsState = .failed(error.localizedDescription)
            requestsState = .failed(error.localizedDescription)
            invitationsState = .failed(error.localizedDescription)
        }
    }

    func searchPlayer(handle: String) async {
        guard let activeServer, let credential, let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        playerSearchState = .loading
        do {
            let result = try await game.realtime.findPlayer(
                baseURL: activeServer.url,
                token: credential,
                handle: handle
            )
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            playerSearchState = .loaded(result)
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            playerSearchState = .failed(error.localizedDescription)
        }
    }

    func sendFriendRequest(to player: PlayerSearchResult) async {
        guard let activeServer, let credential, let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        do {
            try await game.realtime.sendFriendRequest(
                baseURL: activeServer.url,
                token: credential,
                playerId: player.playerId
            )
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            playerSearchState = .idle
            await refreshFriends()
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            playerSearchState = .failed(error.localizedDescription)
        }
    }

    func resolve(_ request: FriendRequestSummary, accept: Bool) async {
        guard let activeServer, let credential, let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        do {
            try await game.realtime.resolveFriendRequest(
                baseURL: activeServer.url,
                token: credential,
                requestId: request.requestId,
                accept: accept
            )
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            await refreshFriends()
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            message = error.localizedDescription
        }
    }

    func remove(_ friend: FriendSummary) async {
        guard let activeServer, let credential, let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        do {
            try await game.realtime.removeFriend(
                baseURL: activeServer.url,
                token: credential,
                playerId: friend.playerId
            )
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            await refreshFriends()
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            message = error.localizedDescription
        }
    }

    func resolve(_ invitation: MatchInvitation, accept: Bool) async {
        guard !accept || readiness.canEnterMatch else {
            message = "Finish appearance setup and calibration before accepting a challenge."
            return
        }
        guard let activeServer, let credential, let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        do {
            let snapshot = try await game.realtime.resolveMatchInvitation(
                baseURL: activeServer.url,
                token: credential,
                invitationId: invitation.invitationId,
                accept: accept
            )
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            if let snapshot {
                try await game.enter(snapshot)
            }
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            await refreshFriends()
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            message = error.localizedDescription
        }
    }

    func loadHistory(reset: Bool = true) async {
        guard let activeServer, let credential, let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        let requestedCursor: String?
        if reset {
            historyCursor = nil
            historyState = .loading
            requestedCursor = nil
        } else {
            guard !loadingMoreHistory, let historyCursor else { return }
            loadingMoreHistory = true
            requestedCursor = historyCursor
        }
        defer {
            if isCurrent(serverId: serverId, token: credential, generation: generation) {
                loadingMoreHistory = false
            }
        }
        do {
            let page = try await game.realtime.fetchMatchHistory(
                baseURL: activeServer.url,
                token: credential,
                cursor: requestedCursor
            )
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            let existing = reset ? [] : (historyState.value ?? [])
            var seen = Set(existing.map(\.matchId))
            historyState = .loaded(existing + page.matches.filter { seen.insert($0.matchId).inserted })
            historyCursor = page.nextCursor
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            historyState = .failed(error.localizedDescription)
        }
    }

    func loadHistoryDetail(_ matchId: String) async {
        guard let activeServer, let credential, let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        selectedHistoryState = .loading
        do {
            let detail = try await game.realtime.fetchMatchDetail(
                baseURL: activeServer.url,
                token: credential,
                matchId: matchId
            )
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            selectedHistoryState = .loaded(detail)
        } catch {
            guard isCurrent(serverId: serverId, token: credential, generation: generation) else { return }
            selectedHistoryState = .failed(error.localizedDescription)
        }
    }

    func switchServer() {
        serverGeneration &+= 1
        stopAvailability(clearRemote: true)
        game.leaveMatch()
        activeServer = nil
        serverInfo = nil
        account = nil
        credential = nil
        appearanceProfile = nil
        friendsState = .idle
        requestsState = .idle
        invitationsState = .idle
        playerSearchState = .idle
        historyState = .idle
        selectedHistoryState = .idle
        historyCursor = nil
        loadingMoreHistory = false
        busy = false
        message = nil
        stage = .serverSelection
        cover = nil
    }

    func setForeground(_ foreground: Bool) {
        if foreground {
            startAvailability()
        } else {
            stopAvailability(clearRemote: true)
        }
    }

    private func finishAuthentication(account: PlayerAccount, token: String) {
        self.account = account
        credential = token
        if let activeServer {
            game.configure(
                serverURL: activeServer.url,
                account: account,
                token: token,
                profile: appearanceProfile
            )
        }
        stage = .hub
        message = nil
        startAvailability()
        Task {
            await refreshFriends()
            await loadHistory()
        }
    }

    private func remember(_ endpoint: ServerEndpoint) {
        recentServers.removeAll { $0.canonicalAddress == endpoint.canonicalAddress }
        recentServers.insert(endpoint, at: 0)
        recentServers = Array(recentServers.prefix(5))
        guard let data = try? JSONEncoder().encode(recentServers) else { return }
        UserDefaults.standard.set(data, forKey: "onboarding.recentServers")
    }

    private static func loadRecents() -> [ServerEndpoint] {
        guard let data = UserDefaults.standard.data(forKey: "onboarding.recentServers") else { return [] }
        return (try? JSONDecoder().decode([ServerEndpoint].self, from: data)) ?? []
    }

    private static func configuredDefaultServer() -> ServerEndpoint? {
        let dictionary = Bundle.main.infoDictionary ?? [:]
        let name = dictionary["FPSDefaultServerName"] as? String ?? "Bogkit Server"
        let address = dictionary["FPSDefaultServerURL"] as? String ?? ""
        return ServerEndpoint.parse(address, displayName: name)
    }

    private func saveEnrollment(_ cache: EnrollmentCache, serverId: String) {
        guard let data = try? JSONEncoder().encode(cache) else { return }
        UserDefaults.standard.set(data, forKey: "onboarding.enrollment.\(serverId)")
    }

    private func loadEnrollment(serverId: String, playerId: String) -> EnrollmentCache? {
        let key = "onboarding.enrollment.\(serverId)"
        guard let data = UserDefaults.standard.data(forKey: key) else { return nil }
        guard let cache = try? JSONDecoder().decode(EnrollmentCache.self, from: data),
              cache.profile.playerId == playerId,
              cache.bodyModel == AppearanceAnalysis.embeddingModel,
              cache.faceModel == AppearanceAnalysis.descriptorModel else {
            UserDefaults.standard.removeObject(forKey: key)
            return nil
        }
        return cache
    }

    private func removeEnrollment(serverId: String) {
        UserDefaults.standard.removeObject(forKey: "onboarding.enrollment.\(serverId)")
    }

    private func isCurrent(serverId: String, token: String, generation: UInt64) -> Bool {
        serverGeneration == generation
            && activeServer?.serverId == serverId
            && credential == token
    }

    private func startAvailability() {
        availabilityTask?.cancel()
        guard let activeServer, let credential, let account,
              let serverId = activeServer.serverId else { return }
        let generation = serverGeneration
        availabilityTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self,
                      self.isCurrent(serverId: serverId, token: credential, generation: generation) else {
                    return
                }
                try? await self.game.realtime.publishAvailability(
                    baseURL: activeServer.url,
                    token: credential,
                    playerId: account.playerId
                )
                await self.refreshFriends(showLoading: false)
                try? await Task.sleep(for: .seconds(10))
            }
        }
    }

    private func stopAvailability(clearRemote: Bool) {
        availabilityTask?.cancel()
        availabilityTask = nil
        guard clearRemote, let activeServer, let credential else { return }
        Task {
            try? await game.realtime.clearAvailability(baseURL: activeServer.url, token: credential)
        }
    }
}
