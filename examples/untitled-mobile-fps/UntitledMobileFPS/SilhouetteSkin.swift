import Foundation

/// A cosmetic pattern a player picks for their own in-game silhouette.
///
/// The pattern is drawn by the *opponent's* phone, not the owner's, so the
/// choice travels with the appearance profile rather than living in local
/// settings.
///
/// Raw values are both wire format and on-disk format (they are persisted in
/// the enrollment cache and stored server-side). Never change one; add a case
/// instead.
enum SilhouetteSkin: String, Codable, CaseIterable, Sendable {
    case redTartan = "red_tartan"
    case greenTartan = "green_tartan"
    case pinkCamo = "pink_camo"
    case greenCamo = "green_camo"

    /// Used when an opponent has no skin recorded, or recorded one this build
    /// does not know about. Matching the pre-skins red keeps old profiles
    /// looking the way they always did.
    static let fallback: SilhouetteSkin = .redTartan

    var displayName: String {
        switch self {
        case .redTartan: "Red Tartan"
        case .greenTartan: "Green Tartan"
        case .pinkCamo: "Pink Camo"
        case .greenCamo: "Green Camo"
        }
    }

    var family: SilhouetteSkinFamily {
        switch self {
        case .redTartan, .greenTartan: .tartan
        // The two camos deliberately use different families so all four skins
        // stay distinguishable across a room, which is the whole job of a
        // target marker.
        case .pinkCamo: .blobCamo
        case .greenCamo: .digitalCamo
        }
    }

    /// Colours in paint order: element 0 is the base fill, later elements are
    /// drawn over it.
    var palette: [SkinColor] {
        switch self {
        case .redTartan:
            [SkinColor(hex: 0xB4232A), SkinColor(hex: 0x1A1512), SkinColor(hex: 0xF2EAD8)]
        case .greenTartan:
            [SkinColor(hex: 0x1E5B34), SkinColor(hex: 0x12180F), SkinColor(hex: 0xEDE7D2)]
        case .pinkCamo:
            [
                SkinColor(hex: 0xF6D3DC), SkinColor(hex: 0xD98CA6),
                SkinColor(hex: 0xE8628C), SkinColor(hex: 0x2B1E1A)
            ]
        case .greenCamo:
            [
                SkinColor(hex: 0xC9CDBD), SkinColor(hex: 0x9FAE95),
                SkinColor(hex: 0x5E6B4F), SkinColor(hex: 0x3A3F35)
            ]
        }
    }

    /// Colour for chrome that has to read as "this skin" at a glance: the
    /// target's bounding box and the picker's selection ring.
    var accent: SkinColor {
        switch self {
        case .redTartan: SkinColor(hex: 0xB4232A)
        case .greenTartan: SkinColor(hex: 0x1E5B34)
        case .pinkCamo: SkinColor(hex: 0xE8628C)
        case .greenCamo: SkinColor(hex: 0x5E6B4F)
        }
    }

    /// Fixed per skin so the generated tile is byte-identical on every device.
    /// Both players must see the same pattern, and it must not shimmer between
    /// frames.
    var seed: UInt64 {
        switch self {
        case .redTartan: 0x5245_4454
        case .greenTartan: 0x4752_4E54
        case .pinkCamo: 0x504E_4B43
        case .greenCamo: 0x4752_4E43
        }
    }
}

enum SilhouetteSkinFamily: String, Equatable, Sendable {
    case tartan
    case blobCamo
    case digitalCamo
}

/// Framework-independent colour so palettes stay in the portable core, where
/// `swift test` can reach them. The renderer converts to CoreGraphics.
struct SkinColor: Equatable, Hashable, Sendable {
    let red: Double
    let green: Double
    let blue: Double

    init(red: Double, green: Double, blue: Double) {
        self.red = red
        self.green = green
        self.blue = blue
    }

    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}

/// Deterministic pseudo-random source for tile generation.
///
/// A fixed LCG rather than `SystemRandomNumberGenerator` because the pattern
/// has to be reproducible: the same skin must generate the same tile on every
/// device and on every launch.
struct SkinRandom {
    private var state: UInt64

    init(seed: UInt64) {
        // Any non-zero state works; the offset just avoids a zero seed.
        state = seed &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
    }

    mutating func next() -> UInt64 {
        state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
        return state
    }

    /// Uniform in `0..<1`.
    mutating func unit() -> Double {
        Double(next() >> 11) / Double(1 << 53)
    }

    mutating func double(in range: ClosedRange<Double>) -> Double {
        range.lowerBound + unit() * (range.upperBound - range.lowerBound)
    }

    mutating func int(below bound: Int) -> Int {
        guard bound > 0 else { return 0 }
        return Int(next() % UInt64(bound))
    }
}
