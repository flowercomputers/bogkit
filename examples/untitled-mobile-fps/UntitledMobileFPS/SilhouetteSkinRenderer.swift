import CoreGraphics
import SwiftUI
import UIKit

/// Generates the repeating pattern tile for each silhouette skin.
///
/// The tiles are drawn procedurally rather than shipped as art: a fixed seed
/// makes them identical on every device, the palettes stay in the testable
/// core, and there is no asset pipeline to keep in sync. If the generated camo
/// ever needs to be replaced with real artwork, `tile(for:)` is the only seam
/// that has to change.
enum SilhouetteSkinRenderer {
    /// Side of the generated tile, in pixels.
    static let tileSize: CGFloat = 256

    /// How large one tile is drawn on screen. Fixed in points rather than
    /// scaled by target distance so the pattern reads as a decal on the target
    /// and stays identifiable at any range.
    static let tileScreenSize: CGFloat = 110

    /// Opacity of the silhouette fill for a live target. Full opacity is the
    /// intended look; this is a single dial for backing it off if the
    /// segmentation edge proves too rough on device.
    static let fillOpacity: Double = 1.0

    /// Eliminated targets keep their shape but drain of colour, which is the
    /// "fades to grey" behaviour the product plan calls for.
    static let eliminatedOpacity: Double = 0.6

    private static let cache = NSCache<NSString, UIImage>()

    static func tile(for skin: SilhouetteSkin) -> UIImage {
        let key = skin.rawValue as NSString
        if let cached = cache.object(forKey: key) { return cached }
        let image = render(skin)
        cache.setObject(image, forKey: key)
        return image
    }

    private static func render(_ skin: SilhouetteSkin) -> UIImage {
        let side = tileSize
        let format = UIGraphicsImageRendererFormat.preferred()
        format.scale = 1
        format.opaque = true
        let renderer = UIGraphicsImageRenderer(
            size: CGSize(width: side, height: side),
            format: format
        )
        return renderer.image { context in
            let cgContext = context.cgContext
            var random = SkinRandom(seed: skin.seed)
            let palette = skin.palette
            cgContext.setFillColor(palette[0].cgColor)
            cgContext.fill(CGRect(x: 0, y: 0, width: side, height: side))
            switch skin.family {
            case .tartan:
                drawTartan(in: cgContext, side: side, palette: palette)
            case .blobCamo:
                drawBlobCamo(in: cgContext, side: side, palette: palette, random: &random)
            case .digitalCamo:
                drawDigitalCamo(in: cgContext, side: side, palette: palette, random: &random)
            }
        }
    }

    // MARK: - Families

    /// Crossed bands over a base colour, with paired light lines and a 45°
    /// hatch on the dark bands. Fully deterministic — tartan has no randomness.
    private static func drawTartan(in context: CGContext, side: CGFloat, palette: [SkinColor]) {
        let dark = palette[1]
        let light = palette[2]
        // Fractions of the tile, so the sett scales with `tileSize`.
        let bands: [(offset: CGFloat, width: CGFloat)] = [
            (0.06, 0.20), (0.44, 0.12), (0.68, 0.26)
        ]

        // Bands are drawn at partial alpha in both axes, so the crossings come
        // out darker than either band alone — the way a real sett reads.
        context.setFillColor(dark.cgColor.copy(alpha: 0.72) ?? dark.cgColor)
        for band in bands {
            context.fill(CGRect(x: band.offset * side, y: 0, width: band.width * side, height: side))
            context.fill(CGRect(x: 0, y: band.offset * side, width: side, height: band.width * side))
        }

        context.setFillColor(light.cgColor)
        let lineWidth = max(side / 128, 1)
        for offset in [0.34, 0.36, 0.60, 0.62] as [CGFloat] {
            context.fill(CGRect(x: offset * side, y: 0, width: lineWidth, height: side))
            context.fill(CGRect(x: 0, y: offset * side, width: side, height: lineWidth))
        }

        // 45° hatch across the whole tile, clipped to the dark bands so it only
        // textures them.
        context.saveGState()
        let bandPath = CGMutablePath()
        for band in bands {
            bandPath.addRect(CGRect(x: band.offset * side, y: 0, width: band.width * side, height: side))
            bandPath.addRect(CGRect(x: 0, y: band.offset * side, width: side, height: band.width * side))
        }
        context.addPath(bandPath)
        context.clip()
        context.setStrokeColor(light.cgColor.copy(alpha: 0.55) ?? light.cgColor)
        context.setLineWidth(lineWidth)
        let spacing = side / 16
        var diagonal = -side
        while diagonal < side * 2 {
            context.move(to: CGPoint(x: diagonal, y: 0))
            context.addLine(to: CGPoint(x: diagonal + side, y: side))
            diagonal += spacing
        }
        context.strokePath()
        context.restoreGState()
    }

    /// Organic camo: rounded blobs in palette order. Each blob is stamped nine
    /// times at ±tile offsets so shapes crossing an edge reappear on the
    /// opposite one and the tile repeats seamlessly.
    private static func drawBlobCamo(
        in context: CGContext,
        side: CGFloat,
        palette: [SkinColor],
        random: inout SkinRandom
    ) {
        let blobsPerColour = 5
        for colour in palette.dropFirst() {
            context.setFillColor(colour.cgColor)
            for _ in 0..<blobsPerColour {
                let centre = CGPoint(x: random.unit() * side, y: random.unit() * side)
                let radius = random.double(in: 0.10...0.22) * Double(side)
                let path = blobPath(radius: radius, random: &random)
                for dx in [-side, 0, side] {
                    for dy in [-side, 0, side] {
                        context.saveGState()
                        context.translateBy(x: centre.x + dx, y: centre.y + dy)
                        context.addPath(path)
                        context.fillPath()
                        context.restoreGState()
                    }
                }
            }
        }
    }

    /// Closed blob around the origin: points at jittered radii, joined with
    /// quadratic curves so the outline stays rounded rather than polygonal.
    private static func blobPath(radius: Double, random: inout SkinRandom) -> CGPath {
        let steps = 10
        var points: [CGPoint] = []
        for index in 0..<steps {
            let angle = (Double(index) / Double(steps)) * 2 * .pi
            let jittered = radius * random.double(in: 0.62...1.30)
            points.append(CGPoint(x: cos(angle) * jittered, y: sin(angle) * jittered))
        }
        let path = CGMutablePath()
        let midpoint = { (a: CGPoint, b: CGPoint) in
            CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
        }
        path.move(to: midpoint(points[steps - 1], points[0]))
        for index in 0..<steps {
            let control = points[index]
            let end = midpoint(points[index], points[(index + 1) % steps])
            path.addQuadCurve(to: end, control: control)
        }
        path.closeSubpath()
        return path
    }

    /// Fraction of the tile each palette entry covers, lightest first.
    private static func bandShares(count: Int) -> [Double] {
        switch count {
        case 4: [0.30, 0.35, 0.24, 0.11]
        case 3: [0.38, 0.40, 0.22]
        default: Array(repeating: 1 / Double(max(count, 1)), count: max(count, 1))
        }
    }

    /// Pixel camo: a coarse cell grid whose values are smoothed from seeded
    /// noise. The smoothing wraps modulo the grid, so the tile repeats without
    /// a visible seam.
    private static func drawDigitalCamo(
        in context: CGContext,
        side: CGFloat,
        palette: [SkinColor],
        random: inout SkinRandom
    ) {
        // Coarse enough that the cells read as deliberate pixels rather than
        // as noise once the tile is scaled down onto a distant target.
        let cells = 24
        let cellSide = side / CGFloat(cells)
        var noise = (0..<(cells * cells)).map { _ in random.unit() }
        // Box-blur passes over a torus turn white noise into blotches a few
        // cells across, which is the patch size real pixel camo uses.
        for _ in 0..<3 {
            var smoothed = noise
            for y in 0..<cells {
                for x in 0..<cells {
                    var total = 0.0
                    for dy in -1...1 {
                        for dx in -1...1 {
                            let sx = (x + dx + cells) % cells
                            let sy = (y + dy + cells) % cells
                            total += noise[sy * cells + sx]
                        }
                    }
                    smoothed[y * cells + x] = total / 9
                }
            }
            noise = smoothed
        }

        // Quantile thresholds rather than fixed cuts: blurred noise clusters
        // hard around its mean, so slicing by rank is what keeps every colour
        // present. The shares are uneven because real pixel camo is mostly its
        // two lighter tones, with the darkest used only as an accent.
        let shares = Self.bandShares(count: palette.count)
        let sorted = noise.sorted()
        var cumulative = 0.0
        let thresholds = shares.dropLast().map { share -> Double in
            cumulative += share
            return sorted[min(sorted.count - 1, Int(cumulative * Double(sorted.count)))]
        }
        var bands = noise.map { value in thresholds.filter { value >= $0 }.count }
        // Scatter a few cells into a neighbouring band. Clean quantile edges
        // look like a heightmap; the dither is what makes it read as camo.
        for index in bands.indices where random.unit() < 0.12 {
            let shift = random.unit() < 0.5 ? -1 : 1
            bands[index] = min(max(bands[index] + shift, 0), palette.count - 1)
        }
        for y in 0..<cells {
            for x in 0..<cells {
                context.setFillColor(palette[bands[y * cells + x]].cgColor)
                context.fill(CGRect(
                    x: CGFloat(x) * cellSide,
                    y: CGFloat(y) * cellSide,
                    width: cellSide + 0.5,
                    height: cellSide + 0.5
                ))
            }
        }
    }
}

extension SkinColor {
    var cgColor: CGColor {
        CGColor(srgbRed: red, green: green, blue: blue, alpha: 1)
    }

    var color: Color {
        Color(red: red, green: green, blue: blue)
    }
}

/// A skin preview: the tile pattern inside a person glyph. Used by the picker
/// so the player sees the actual generated pattern rather than a colour chip.
struct SilhouetteSkinSwatch: View {
    let skin: SilhouetteSkin
    var selected: Bool = false
    var side: CGFloat = 62

    var body: some View {
        Image(uiImage: SilhouetteSkinRenderer.tile(for: skin))
            .resizable(resizingMode: .tile)
            .frame(width: side, height: side)
            .mask {
                Image(systemName: "figure.stand")
                    .resizable()
                    .scaledToFit()
                    .padding(6)
                    .frame(width: side, height: side)
            }
            .background(.black.opacity(0.55), in: RoundedRectangle(cornerRadius: 10))
            .overlay {
                RoundedRectangle(cornerRadius: 10)
                    .stroke(
                        selected ? skin.accent.color : .white.opacity(0.22),
                        lineWidth: selected ? 2 : 1
                    )
            }
            .accessibilityLabel(skin.displayName)
    }
}
