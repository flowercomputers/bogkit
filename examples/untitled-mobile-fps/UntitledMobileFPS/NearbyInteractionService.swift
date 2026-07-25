import Foundation
import NearbyInteraction

struct NearbyReading: Equatable, Sendable {
    let distanceMeters: Float?
    let direction: [Float]?
    let sampledAtMs: UInt64
}

@MainActor
final class NearbyInteractionService: NSObject, ObservableObject {
    @Published private(set) var reading: NearbyReading?
    @Published private(set) var status = "UWB idle"
    var onDiscoveryToken: ((String) -> Void)?
    var onReading: ((NearbyReading) -> Void)?

    private var nearbySession: NISession?
    private var configuration: NINearbyPeerConfiguration?
    private var peerId: String?
    private var localDiscoveryToken: String?
    private var pendingPeerToken: String?
    private var configuredPeerToken: String?
    private var tokenRetryTask: Task<Void, Never>?
    private var staleReadingTask: Task<Void, Never>?
    private var sessionRestartTask: Task<Void, Never>?

    func start(peerId: String) {
        guard NISession.deviceCapabilities.supportsPreciseDistanceMeasurement else {
            stop()
            status = "UWB unsupported on this iPhone"
            return
        }
        guard self.peerId != peerId || nearbySession == nil else {
            beginTokenRelay()
            return
        }
        // A token can arrive after the coordinator knows the opponent but
        // before this local session exists. A nil peer ID therefore means the
        // buffered token belongs to the peer being started, not a stale match.
        let isNewPeer = self.peerId != nil && self.peerId != peerId
        invalidateSession()
        if isNewPeer {
            pendingPeerToken = nil
            configuredPeerToken = nil
        }
        self.peerId = peerId
        let session = NISession()
        session.delegate = self
        nearbySession = session
        status = "UWB exchanging tokens"
        if let token = session.discoveryToken,
           let data = try? NSKeyedArchiver.archivedData(withRootObject: token, requiringSecureCoding: true) {
            localDiscoveryToken = data.base64EncodedString()
            beginTokenRelay()
            configurePendingPeerTokenIfPossible()
        } else {
            status = "UWB could not create a discovery token"
        }
    }

    func acceptPeerToken(_ encoded: String) {
        guard NISession.deviceCapabilities.supportsPreciseDistanceMeasurement else {
            status = "UWB unsupported on this iPhone"
            return
        }
        guard encoded != configuredPeerToken else { return }
        pendingPeerToken = encoded
        guard nearbySession != nil else {
            if let peerId {
                // A NISession cannot be reused after invalidation. The peer's
                // one-second relay retry is the strongest recovery signal, so
                // recreate the local session immediately and consume this
                // buffered token instead of remaining stranded.
                start(peerId: peerId)
            } else {
                status = "UWB peer token buffered · waiting for match"
            }
            return
        }
        configurePendingPeerTokenIfPossible()
    }

    func discoveryTokenRelaySucceeded(_ token: String) {
        guard token == localDiscoveryToken, reading == nil else { return }
        status = configuration == nil
            ? "UWB token sent · waiting for peer"
            : "UWB ranging · waiting for distance"
    }

    func discoveryTokenRelayFailed(_ error: Error, token: String) {
        guard token == localDiscoveryToken, reading == nil else { return }
        status = "UWB token relay failed: \(error.localizedDescription)"
    }

    func proximityRelayFailed(_ error: Error, sampledAtMs: UInt64) {
        guard reading?.sampledAtMs == sampledAtMs else { return }
        status = "UWB report failed: \(error.localizedDescription)"
    }

    private func configurePendingPeerTokenIfPossible() {
        guard let encoded = pendingPeerToken,
              let nearbySession else { return }
        guard let data = Data(base64Encoded: encoded),
              let token = try? NSKeyedUnarchiver.unarchivedObject(
                ofClass: NIDiscoveryToken.self,
                from: data
              ) else {
            pendingPeerToken = nil
            status = "UWB token invalid"
            return
        }
        let configuration = NINearbyPeerConfiguration(peerToken: token)
        self.configuration = configuration
        configuredPeerToken = encoded
        pendingPeerToken = nil
        staleReadingTask?.cancel()
        staleReadingTask = nil
        reading = nil
        nearbySession.run(configuration)
        status = "UWB ranging · waiting for distance"
        beginTokenRelay()
    }

    private func beginTokenRelay() {
        tokenRetryTask?.cancel()
        guard reading == nil, let localDiscoveryToken else { return }
        tokenRetryTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, self.reading == nil else { return }
                self.onDiscoveryToken?(localDiscoveryToken)
                do {
                    try await Task.sleep(nanoseconds: 1_000_000_000)
                } catch {
                    return
                }
            }
        }
    }

    private func expireReadingIfStale(_ sampledAtMs: UInt64) {
        staleReadingTask?.cancel()
        staleReadingTask = Task { [weak self] in
            do {
                // The server rejects reports older than 1.5 seconds. Clear the
                // HUD slightly earlier so it never advertises a usable range
                // after the server would reject the same reading.
                try await Task.sleep(nanoseconds: 1_200_000_000)
            } catch {
                return
            }
            guard let self, self.reading?.sampledAtMs == sampledAtMs else { return }
            self.reading = nil
            self.status = "UWB reading stale"
            self.beginTokenRelay()
        }
    }

    private func invalidateSession() {
        sessionRestartTask?.cancel()
        sessionRestartTask = nil
        tokenRetryTask?.cancel()
        tokenRetryTask = nil
        staleReadingTask?.cancel()
        staleReadingTask = nil
        let session = nearbySession
        nearbySession = nil
        session?.invalidate()
        configuration = nil
        configuredPeerToken = nil
        localDiscoveryToken = nil
        reading = nil
    }

    func stop() {
        invalidateSession()
        peerId = nil
        pendingPeerToken = nil
        configuredPeerToken = nil
        status = "UWB idle"
    }
}

extension NearbyInteractionService: NISessionDelegate {
    nonisolated func session(_ session: NISession, didUpdate nearbyObjects: [NINearbyObject]) {
        guard let object = nearbyObjects.first else { return }
        let reading = NearbyReading(
            distanceMeters: object.distance,
            direction: object.direction.map { [$0.x, $0.y, $0.z] },
            sampledAtMs: .currentMilliseconds
        )
        Task { @MainActor [weak self] in
            guard let self, self.nearbySession === session else { return }
            self.reading = reading
            self.tokenRetryTask?.cancel()
            self.tokenRetryTask = nil
            self.status = reading.distanceMeters.map {
                String(format: "UWB live · %.2f m", $0)
            } ?? "UWB has direction but no distance"
            self.onReading?(reading)
            self.expireReadingIfStale(reading.sampledAtMs)
        }
    }

    nonisolated func sessionWasSuspended(_ session: NISession) {
        Task { @MainActor [weak self] in
            guard let self, self.nearbySession === session else { return }
            self.staleReadingTask?.cancel()
            self.staleReadingTask = nil
            self.reading = nil
            self.status = "UWB suspended"
        }
    }

    nonisolated func sessionSuspensionEnded(_ session: NISession) {
        Task { @MainActor [weak self] in
            guard let self,
                  self.nearbySession === session,
                  let configuration = self.configuration else { return }
            session.run(configuration)
            self.status = "UWB ranging resumed"
            self.beginTokenRelay()
        }
    }

    nonisolated func session(_ session: NISession, didInvalidateWith error: Error) {
        Task { @MainActor [weak self] in
            guard let self, self.nearbySession === session else { return }
            self.tokenRetryTask?.cancel()
            self.tokenRetryTask = nil
            self.staleReadingTask?.cancel()
            self.staleReadingTask = nil
            self.nearbySession = nil
            self.configuration = nil
            self.configuredPeerToken = nil
            self.localDiscoveryToken = nil
            self.reading = nil
            self.status = "UWB restarting: \(error.localizedDescription)"
            guard let peerId = self.peerId else { return }
            self.sessionRestartTask?.cancel()
            self.sessionRestartTask = Task { [weak self] in
                do {
                    try await Task.sleep(nanoseconds: 500_000_000)
                } catch {
                    return
                }
                guard let self, self.peerId == peerId else { return }
                self.sessionRestartTask = nil
                self.start(peerId: peerId)
            }
        }
    }

    nonisolated func session(
        _ session: NISession,
        didRemove nearbyObjects: [NINearbyObject],
        reason: NINearbyObject.RemovalReason
    ) {
        Task { @MainActor [weak self] in
            guard let self, self.nearbySession === session else { return }
            self.reading = nil
            self.status = "UWB peer removed: \(String(describing: reason))"
            self.beginTokenRelay()
        }
    }
}
