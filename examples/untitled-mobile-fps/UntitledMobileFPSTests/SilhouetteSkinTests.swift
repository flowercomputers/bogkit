import XCTest
@testable import UntitledMobileFPS

final class SilhouetteSkinTests: XCTestCase {
    /// Raw values are wire format and on-disk format. Renaming one silently
    /// resets every player who had that skin, so pin them explicitly.
    func testRawValuesAreStable() {
        XCTAssertEqual(SilhouetteSkin.redTartan.rawValue, "red_tartan")
        XCTAssertEqual(SilhouetteSkin.greenTartan.rawValue, "green_tartan")
        XCTAssertEqual(SilhouetteSkin.pinkCamo.rawValue, "pink_camo")
        XCTAssertEqual(SilhouetteSkin.greenCamo.rawValue, "green_camo")
        XCTAssertEqual(SilhouetteSkin.allCases.count, 4)
    }

    func testEverySkinHasADistinctPaletteAndSeed() {
        for skin in SilhouetteSkin.allCases {
            XCTAssertGreaterThanOrEqual(
                Set(skin.palette).count,
                3,
                "\(skin.rawValue) needs at least three distinct colours"
            )
            XCTAssertFalse(skin.displayName.isEmpty)
        }
        XCTAssertEqual(
            Set(SilhouetteSkin.allCases.map(\.seed)).count,
            SilhouetteSkin.allCases.count,
            "shared seeds would make two skins generate the same tile"
        )
    }

    func testHexInitialiserMatchesComponents() {
        let colour = SkinColor(hex: 0xB4232A)
        XCTAssertEqual(colour.red, 180.0 / 255, accuracy: 1e-9)
        XCTAssertEqual(colour.green, 35.0 / 255, accuracy: 1e-9)
        XCTAssertEqual(colour.blue, 42.0 / 255, accuracy: 1e-9)
    }

    /// Tiles must be identical on both players' phones, so the generator's
    /// random source has to be reproducible from the seed alone.
    func testSkinRandomIsDeterministicForASeed() {
        var first = SkinRandom(seed: SilhouetteSkin.pinkCamo.seed)
        var second = SkinRandom(seed: SilhouetteSkin.pinkCamo.seed)
        var third = SkinRandom(seed: SilhouetteSkin.greenCamo.seed)
        let firstRun = (0..<8).map { _ in first.unit() }
        let secondRun = (0..<8).map { _ in second.unit() }
        let thirdRun = (0..<8).map { _ in third.unit() }
        XCTAssertEqual(firstRun, secondRun)
        XCTAssertNotEqual(firstRun, thirdRun)
        XCTAssertTrue(firstRun.allSatisfy { $0 >= 0 && $0 < 1 })
    }

    func testProfileWithoutSkinDecodesToNil() throws {
        let json = Self.profileJSON(skinField: nil)
        let profile = try JSONDecoder().decode(AppearanceProfile.self, from: json)
        XCTAssertNil(profile.skin)
        XCTAssertNil(profile.silhouetteSkin)
    }

    func testProfileRoundTripsASkin() throws {
        let json = Self.profileJSON(skinField: "\"pink_camo\",")
        let profile = try JSONDecoder().decode(AppearanceProfile.self, from: json)
        XCTAssertEqual(profile.silhouetteSkin, .pinkCamo)

        let encoded = try JSONEncoder().encode(profile)
        let decoded = try JSONDecoder().decode(AppearanceProfile.self, from: encoded)
        XCTAssertEqual(decoded.silhouetteSkin, .pinkCamo)
        XCTAssertEqual(decoded, profile)
    }

    /// A skin added by a newer client must not fail the opponent's decode, and
    /// must survive a re-upload by this build rather than being dropped.
    func testUnknownSkinDecodesToNilAndSurvivesReEncoding() throws {
        let json = Self.profileJSON(skinField: "\"chrome_hexagons\",")
        let profile = try JSONDecoder().decode(AppearanceProfile.self, from: json)
        XCTAssertNil(profile.silhouetteSkin)
        XCTAssertEqual(profile.skin, "chrome_hexagons")

        let encoded = try JSONEncoder().encode(profile)
        let decoded = try JSONDecoder().decode(AppearanceProfile.self, from: encoded)
        XCTAssertEqual(decoded.skin, "chrome_hexagons")
    }

    func testWithSkinReplacesOnlyTheSkin() throws {
        let profile = try JSONDecoder().decode(
            AppearanceProfile.self,
            from: Self.profileJSON(skinField: "\"red_tartan\",")
        )
        let updated = profile.withSkin(.greenCamo)
        XCTAssertEqual(updated.silhouetteSkin, .greenCamo)
        XCTAssertEqual(updated.playerId, profile.playerId)
        XCTAssertEqual(updated.wholeBodyEmbedding, profile.wholeBodyEmbedding)
        XCTAssertEqual(updated.briefingThumbnail, profile.briefingThumbnail)
        XCTAssertEqual(updated.updatedAtMs, profile.updatedAtMs)
        XCTAssertNil(profile.withSkin(nil).silhouetteSkin)
    }

    private static func profileJSON(skinField: String?) -> Data {
        let skin = skinField.map { "\"skin\": \($0)" } ?? ""
        return Data("""
        {
            "playerId": "p1",
            "displayName": "Player One",
            "generatedDescription": "red jacket, dark jeans",
            "embeddingModel": "test-v1",
            "descriptorModel": "test-v1",
            "wholeBodyEmbedding": [0.1, 0.2],
            "faceEmbeddings": [],
            "upperBodyEmbeddings": [],
            "lowerBodyEmbeddings": [],
            "headAccessoryEmbeddings": [],
            "silhouetteDescriptor": [0.0],
            "briefingThumbnail": null,
            \(skin)
            "updatedAtMs": 1700000000000
        }
        """.utf8)
    }
}
