import XCTest
@testable import UntitledMobileFPS

final class AppModelsTests: XCTestCase {
    func testServerCanonicalizationNormalizesEquivalentAddresses() throws {
        let first = try XCTUnwrap(ServerEndpoint.parse(" HTTPS://Example.COM:443/ "))
        let second = try XCTUnwrap(ServerEndpoint.parse("https://example.com"))
        XCTAssertEqual(first.canonicalAddress, second.canonicalAddress)
        XCTAssertEqual(first.canonicalAddress, "https://example.com")
    }

    func testServerCanonicalizationKeepsNonDefaultPort() throws {
        let endpoint = try XCTUnwrap(ServerEndpoint.parse("http://192.168.1.2:3319/"))
        XCTAssertEqual(endpoint.canonicalAddress, "http://192.168.1.2:3319")
        XCTAssertTrue(endpoint.allowsInsecureDevelopmentTransport)
    }

    func testServerBasePathIsRejectedInsteadOfSilentlyDiscarded() {
        XCTAssertNil(ServerEndpoint.parse("https://example.com/game"))
    }

    func testPublicHTTPServerIsNotAllowedAsDevelopmentTransport() throws {
        let endpoint = try XCTUnwrap(ServerEndpoint.parse("http://example.com"))
        XCTAssertFalse(endpoint.allowsInsecureDevelopmentTransport)
    }

    func testReadinessReportsRequirementsInSetupOrder() {
        let readiness = MatchReadiness(
            connected: true,
            registered: true,
            hasBodyAppearance: false,
            hasFaceAppearance: true,
            calibrated: false
        )
        XCTAssertEqual(readiness.missingRequirements, [.bodyAppearance, .calibration])
        XCTAssertFalse(readiness.canEnterMatch)
    }

    func testReadinessAllowsMatchOnlyWhenComplete() {
        XCTAssertTrue(MatchReadiness(
            connected: true,
            registered: true,
            hasBodyAppearance: true,
            hasFaceAppearance: true,
            calibrated: true
        ).canEnterMatch)
    }

    func testDecodesFlatProtocolTwoHealthAndAccountEnvelope() throws {
        let health = Data("""
        {
          "status": "ok",
          "serverId": "server-1",
          "displayName": "Bogkit Local",
          "environment": "development",
          "protocolVersion": 2,
          "capabilities": ["accounts", "friends", "matchHistory"],
          "minimumClientVersion": "0.1.0"
        }
        """.utf8)
        let server = try JSONDecoder().decode(ServerInfo.self, from: health)
        XCTAssertEqual(server.serverId, "server-1")
        XCTAssertEqual(server.protocolVersion, 2)

        let registration = Data("""
        {
          "account": {
            "playerId": "player-1",
            "handle": "test_player",
            "displayName": "Test Player",
            "appearanceStatus": "missing",
            "createdAtMs": 100,
            "updatedAtMs": 100
          },
          "token": "one-time-secret"
        }
        """.utf8)
        let envelope = try JSONDecoder().decode(AccountEnvelope.self, from: registration)
        XCTAssertEqual(envelope.account.handle, "test_player")
        XCTAssertEqual(envelope.token, "one-time-secret")
    }

    func testDecodesFriendInvitationAndRankedHistoryShapes() throws {
        let friend = try JSONDecoder().decode(FriendSummary.self, from: Data("""
        {
          "playerId": "friend-1",
          "handle": "bog_bot",
          "displayName": "Bog Bot",
          "available": true,
          "lastSeenAtMs": null
        }
        """.utf8))
        XCTAssertTrue(friend.available)

        let invitation = try JSONDecoder().decode(MatchInvitation.self, from: Data("""
        {
          "invitationId": "invite-1",
          "fromPlayerId": "friend-1",
          "toPlayerId": "player-1",
          "matchId": "match-1",
          "status": "pending",
          "createdAtMs": 100,
          "expiresAtMs": 600100,
          "updatedAtMs": 100
        }
        """.utf8))
        XCTAssertEqual(invitation.status, .pending)

        let history = try JSONDecoder().decode(MatchHistoryPage.self, from: Data("""
        {
          "matches": [{
            "matchId": "match-1",
            "result": "won",
            "opponent": {
              "playerId": "friend-1",
              "handle": "bog_bot",
              "displayName": "Bog Bot",
              "hitTotal": 0,
              "winner": false
            },
            "startedAtMs": 1000,
            "completedAtMs": 7000,
            "myHitTotal": 3
          }],
          "nextCursor": null
        }
        """.utf8))
        XCTAssertEqual(history.matches.first?.result, .won)
        XCTAssertEqual(history.matches.first?.durationSeconds, 6)
    }
}
