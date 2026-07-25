import CoreLocation
import Foundation

@MainActor
final class GameplayCoordinator: ObservableObject {
    @Published var serverAddress: String {
        didSet { UserDefaults.standard.set(serverAddress, forKey: Self.serverKey) }
    }
    @Published private(set) var session: DemoSession?
    @Published private(set) var appearanceProfile: AppearanceProfile?
    @Published private(set) var opponentProfile: AppearanceProfile?
    @Published private(set) var match: MultiplayerMatchSnapshot?
    @Published private(set) var busy = false
    @Published private(set) var statusMessage = "Register a photo-derived appearance to play."
    @Published private(set) var lastShotResult: String?
    @Published private(set) var botFallbackEnabled = false

    let realtime = RealtimeMatchClient()
    let nearby = NearbyInteractionService()
    private let locationProvider = LocationProvider()
    private var simulatedProximityTask: Task<Void, Never>?

    private static let serverKey = "multiplayer.serverAddress"
    private static let matchKey = "multiplayer.selectedMatch"

    init() {
        serverAddress = "http://localhost:3000"
        session = nil
        appearanceProfile = nil

        realtime.onMessage = { [weak self] message in self?.handle(message) }
        realtime.onDisconnect = { [weak self] error in
            guard let self else { return }
            statusMessage = error.map { "Realtime disconnected: \($0.localizedDescription)" } ?? "Realtime disconnected."
        }
        nearby.onDiscoveryToken = { [weak self] token in self?.sendDiscoveryToken(token) }
        nearby.onReading = { [weak self] reading in self?.sendProximity(reading) }

    }

    deinit { simulatedProximityTask?.cancel() }

    var opponentId: String? {
        guard let playerId = session?.playerId else { return nil }
        return match?.players.first(where: { $0.playerId != playerId })?.playerId
    }

    var myState: PlayerMatchState? {
        guard let playerId = session?.playerId else { return nil }
        return match?.player(playerId)
    }

    var opponentState: PlayerMatchState? {
        guard let opponentId else { return nil }
        return match?.player(opponentId)
    }

    var botLaunchCommand: String {
        let port = baseURL?.port ?? 3000
        let code = match?.inviteCode ?? "INVITE_CODE"
        return "cargo run --manifest-path Backend/Cargo.toml -p fps-bot -- scenario full-match http://127.0.0.1:\(port) \(code)"
    }

    func configure(serverURL: URL, account: PlayerAccount, token: String, profile: AppearanceProfile?) {
        if serverAddress != serverURL.absoluteString {
            leaveMatch()
        }
        serverAddress = serverURL.absoluteString
        session = DemoSession(
            playerId: account.playerId,
            token: token,
            displayName: account.displayName
        )
        appearanceProfile = profile
        statusMessage = profile == nil
            ? "Finish appearance setup and calibration to play."
            : "Ready for a match."
    }

    func setAppearanceProfile(_ profile: AppearanceProfile) {
        appearanceProfile = profile
        statusMessage = "Appearance registered: \(profile.generatedDescription)"
    }

    func requestLocalNetworkAccess() {
        realtime.requestLocalNetworkAccess()
        statusMessage = "Allow Local Network access if iOS prompts, then test the server connection."
    }

    func receive(_ message: MultiplayerServerMessage) {
        handle(message)
    }

    func createInvite() async {
        guard let baseURL, let session, appearanceProfile != nil else {
            statusMessage = "Register an appearance and check the server URL first."
            return
        }
        busy = true
        defer { busy = false }
        do {
            let snapshot = try await realtime.createInvite(baseURL: baseURL, token: session.token)
            try await select(snapshot, baseURL: baseURL, session: session)
            statusMessage = "Invite \(snapshot.inviteCode) created. Share it with the second phone."
        } catch {
            statusMessage = error.localizedDescription
        }
    }

    func joinInvite(code: String) async {
        guard let baseURL, let session, appearanceProfile != nil else {
            statusMessage = "Register an appearance and check the server URL first."
            return
        }
        busy = true
        defer { busy = false }
        do {
            let snapshot = try await realtime.joinInvite(baseURL: baseURL, code: code, token: session.token)
            try await select(snapshot, baseURL: baseURL, session: session)
            statusMessage = "Joined \(snapshot.inviteCode). Both players can ready up."
        } catch {
            statusMessage = error.localizedDescription
        }
    }

    /// Joins a random match with a nearby player. Uploads a one-shot real-coordinate
    /// presence (the normal heartbeat is location-free) so the server's presence HNSW can
    /// pair us, then enters the returned lobby — either already matched, or waiting for a
    /// nearby opponent to be paired in via the live match snapshot.
    func quickMatchNearby() async {
        guard let baseURL, let session, appearanceProfile != nil else {
            statusMessage = "Register an appearance and check the server URL first."
            return
        }
        busy = true
        defer { busy = false }
        do {
            statusMessage = "Finding your location…"
            let location = try await locationProvider.currentLocation()
            try await realtime.publishLocatedAvailability(
                baseURL: baseURL,
                token: session.token,
                playerId: session.playerId,
                latitude: location.coordinate.latitude,
                longitude: location.coordinate.longitude,
                accuracy: max(location.horizontalAccuracy, 0)
            )
            let snapshot = try await realtime.matchNearby(baseURL: baseURL, token: session.token)
            try await select(snapshot, baseURL: baseURL, session: session)
            if snapshot.players.count >= 2 {
                statusMessage = "Matched with a nearby player. Both players can ready up."
            } else {
                statusMessage = "Searching for a nearby opponent — you'll be paired automatically."
            }
        } catch {
            statusMessage = error.localizedDescription
        }
    }

    func ready(setupComplete: Bool) {
        guard setupComplete, appearanceProfile != nil else {
            statusMessage = "Finish appearance setup and calibration before readying."
            return
        }
        guard let match else { return }
        Task {
            do {
                try await realtime.send(.readyWithMetadata(
                    commandId: UUID(),
                    matchId: match.matchId,
                    calibrationModelVersion: String(VisionAimCalibration.modelVersion)
                ))
                statusMessage = "Ready sent. Waiting for the opponent."
            } catch {
                statusMessage = error.localizedDescription
            }
        }
    }

    func acknowledgeBriefing() {
        guard let match, match.status == .briefing else { return }
        Task {
            do {
                try await realtime.send(.briefingAcknowledged(commandId: UUID(), matchId: match.matchId))
                statusMessage = "Briefing acknowledged. Waiting for your opponent."
            } catch {
                statusMessage = error.localizedDescription
            }
        }
    }

    func challenge(_ friend: FriendSummary) async {
        guard let baseURL, let session, appearanceProfile != nil else {
            statusMessage = "Finish setup before challenging a friend."
            return
        }
        busy = true
        defer { busy = false }
        do {
            let snapshot = try await realtime.challenge(
                baseURL: baseURL,
                token: session.token,
                playerId: friend.playerId
            )
            try await select(snapshot, baseURL: baseURL, session: session)
            statusMessage = "Challenge sent to @\(friend.handle)."
        } catch {
            statusMessage = error.localizedDescription
        }
    }

    func enter(_ snapshot: MultiplayerMatchSnapshot) async throws {
        guard let baseURL, let session else { throw MatchClientError.disconnected }
        try await select(snapshot, baseURL: baseURL, session: session)
    }

    func setBotFallback(_ enabled: Bool) {
        botFallbackEnabled = enabled
        restartSimulatedProximity()
        if enabled, opponentId == nil {
            statusMessage = "Proximity simulation is ready, but it does not create a bot. Run the displayed fps-bot command on the Mac."
        }
    }

    func handleShot(_ event: GameplayShotEvent) {
        guard match?.status == .active, let match, let targetId = opponentId else {
            if match != nil { lastShotResult = "WAITING · MATCH NOT ACTIVE" }
            return
        }
        let targeting = event.targeting
        lastShotResult = "CHECKING SHOT…"
        Task {
            do {
                try await realtime.send(.shot(
                    commandId: UUID(),
                    matchId: match.matchId,
                    targetId: targetId,
                    reticle: [Float(targeting.gameplayPoint.x), Float(targeting.gameplayPoint.y)],
                    maskContainsReticle: targeting.maskContainsReticle,
                    targetScore: targeting.targetScore,
                    firedAtMs: .currentMilliseconds
                ))
            } catch {
                statusMessage = error.localizedDescription
            }
        }
    }

    func leaveMatch() {
        simulatedProximityTask?.cancel()
        simulatedProximityTask = nil
        nearby.stop()
        realtime.disconnect()
        match = nil
        opponentProfile = nil
        lastShotResult = nil
        UserDefaults.standard.removeObject(forKey: Self.matchKey)
        statusMessage = "Match closed."
    }

    private var baseURL: URL? {
        guard let url = URL(string: serverAddress), url.scheme == "http" || url.scheme == "https" else { return nil }
        return url
    }

    private func connectRealtime(baseURL: URL, session: DemoSession, matchId: String) async throws {
        try await realtime.prepareRealtime(baseURL: baseURL, token: session.token, matchId: matchId)
        try realtime.connect(baseURL: baseURL, matchId: matchId)
    }

    private func select(_ snapshot: MultiplayerMatchSnapshot, baseURL: URL, session: DemoSession) async throws {
        simulatedProximityTask?.cancel()
        simulatedProximityTask = nil
        nearby.stop()
        opponentProfile = nil
        lastShotResult = nil
        realtime.disconnect()
        do {
            try await connectRealtime(baseURL: baseURL, session: session, matchId: snapshot.matchId)
            apply(snapshot)
        } catch {
            realtime.disconnect()
            match = nil
            UserDefaults.standard.removeObject(forKey: Self.matchKey)
            throw error
        }
    }

    private func handle(_ message: MultiplayerServerMessage) {
        switch message {
        case .hello:
            statusMessage = "Realtime connected."
        case .matchSnapshot(let snapshot):
            guard snapshot.matchId == match?.matchId else { return }
            apply(snapshot)
        case .socialRevision, .invitationRevision:
            break
        case .nearbyToken(let playerId, let token):
            guard match?.status != .completed,
                  playerId == opponentId else { return }
            // The relay can arrive after iOS has invalidated the local
            // NISession. Starting is idempotent while healthy and recreates
            // the session when it is gone.
            nearby.start(peerId: playerId)
            nearby.acceptPeerToken(token)
        case .shotResolution(_, let accepted, let reason, let snapshot):
            if let snapshot, snapshot.matchId == match?.matchId { apply(snapshot) }
            lastShotResult = MultiplayerShotFeedback.message(accepted: accepted, reason: reason)
        case .error(let message):
            statusMessage = message
        }
    }

    private func apply(_ snapshot: MultiplayerMatchSnapshot) {
        guard snapshot.protocolVersion == multiplayerProtocolVersion else {
            statusMessage = "Server protocol \(snapshot.protocolVersion) is not supported."
            return
        }
        let shouldClearShotResult = MultiplayerShotFeedback.shouldClear(previous: match, next: snapshot)
        let oldOpponent = opponentId
        match = snapshot
        Self.save(snapshot, key: Self.matchKey)
        if shouldClearShotResult {
            lastShotResult = nil
        }
        if snapshot.status == .briefing {
            statusMessage = "Review your opponent briefing before the match starts."
        } else if snapshot.status == .active {
            statusMessage = "Match active. Acquire the red target, then arm and fire."
        } else if snapshot.status == .completed {
            simulatedProximityTask?.cancel()
            nearby.stop()
            statusMessage = snapshot.winner == session?.playerId ? "Match complete — you won." : "Match complete."
            return
        }
        guard let opponentId else { return }
        // Every current snapshot is a lifecycle checkpoint. This is cheap when
        // the NISession is healthy and repairs a session invalidated between
        // snapshots even though the opponent identity did not change.
        nearby.start(peerId: opponentId)
        restartSimulatedProximity()
        guard opponentId != oldOpponent || opponentProfile == nil else { return }
        fetchOpponentAppearance(opponentId)
    }

    private func fetchOpponentAppearance(_ playerId: String) {
        guard let baseURL, let session else { return }
        Task {
            do {
                opponentProfile = try await realtime.fetchAppearance(
                    baseURL: baseURL,
                    token: session.token,
                    playerId: playerId
                )
            } catch {
                statusMessage = "Opponent joined; appearance pending."
            }
        }
    }

    private func sendDiscoveryToken(_ token: String) {
        guard let match, let opponentId else { return }
        Task {
            do {
                try await realtime.send(.nearbyToken(
                    commandId: UUID(),
                    matchId: match.matchId,
                    peerId: opponentId,
                    token: token
                ))
                nearby.discoveryTokenRelaySucceeded(token)
            } catch {
                nearby.discoveryTokenRelayFailed(error, token: token)
            }
        }
    }

    private func sendProximity(_ reading: NearbyReading) {
        guard let match, let opponentId else { return }
        Task {
            do {
                try await realtime.send(.proximity(
                    commandId: UUID(),
                    matchId: match.matchId,
                    peerId: opponentId,
                    distanceMeters: reading.distanceMeters,
                    direction: reading.direction,
                    sampledAtMs: reading.sampledAtMs
                ))
            } catch {
                nearby.proximityRelayFailed(error, sampledAtMs: reading.sampledAtMs)
            }
        }
    }

    private func restartSimulatedProximity() {
        simulatedProximityTask?.cancel()
        simulatedProximityTask = nil
        guard botFallbackEnabled, match != nil, opponentId != nil else { return }
        simulatedProximityTask = Task { [weak self] in
            while !Task.isCancelled {
                self?.sendProximity(NearbyReading(
                    distanceMeters: 1,
                    direction: [0, 0, -1],
                    sampledAtMs: .currentMilliseconds
                ))
                try? await Task.sleep(nanoseconds: 450_000_000)
            }
        }
    }

    private static func save<Value: Encodable>(_ value: Value, key: String) {
        guard let data = try? JSONEncoder().encode(value) else { return }
        UserDefaults.standard.set(data, forKey: key)
    }

    private static func load<Value: Decodable>(_ type: Value.Type, key: String) -> Value? {
        guard let data = UserDefaults.standard.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }
}

/// One-shot Core Location fetch for Quick Match. Requests When-In-Use authorization on
/// first use and resolves a single fix, so no always-on location tracking is added.
/// Callbacks arrive on the main thread; a lock guards the single in-flight continuation.
final class LocationProvider: NSObject, CLLocationManagerDelegate, @unchecked Sendable {
    enum LocationError: LocalizedError {
        case denied
        case unavailable

        var errorDescription: String? {
            switch self {
            case .denied:
                return "Location access is off. Enable it in Settings to match with nearby players."
            case .unavailable:
                return "Couldn't get your location. Try again with a clearer signal."
            }
        }
    }

    private let manager = CLLocationManager()
    private let lock = NSLock()
    private var continuation: CheckedContinuation<CLLocation, Error>?

    override init() {
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyHundredMeters
    }

    func currentLocation() async throws -> CLLocation {
        switch manager.authorizationStatus {
        case .denied, .restricted:
            throw LocationError.denied
        default:
            break
        }
        return try await withCheckedThrowingContinuation { continuation in
            lock.withLock { self.continuation = continuation }
            if manager.authorizationStatus == .notDetermined {
                manager.requestWhenInUseAuthorization()
            } else {
                manager.requestLocation()
            }
        }
    }

    func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        switch manager.authorizationStatus {
        case .authorizedWhenInUse, .authorizedAlways:
            manager.requestLocation()
        case .denied, .restricted:
            finish(.failure(LocationError.denied))
        default:
            break
        }
    }

    func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        if let location = locations.last {
            finish(.success(location))
        } else {
            finish(.failure(LocationError.unavailable))
        }
    }

    func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        finish(.failure(error))
    }

    private func finish(_ result: Result<CLLocation, Error>) {
        let pending = lock.withLock { () -> CheckedContinuation<CLLocation, Error>? in
            let pending = continuation
            continuation = nil
            return pending
        }
        pending?.resume(with: result)
    }
}
