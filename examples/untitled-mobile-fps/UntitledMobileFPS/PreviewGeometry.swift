import CoreGraphics

struct PreviewGeometry: Equatable {
    let viewSize: CGSize
    let imageSize: CGSize

    func point(fromVisionNormalized point: CGPoint) -> CGPoint {
        guard imageSize.width > 0, imageSize.height > 0 else { return .zero }
        let scale = max(viewSize.width / imageSize.width, viewSize.height / imageSize.height)
        let renderedSize = CGSize(width: imageSize.width * scale, height: imageSize.height * scale)
        let offset = CGPoint(
            x: (viewSize.width - renderedSize.width) / 2,
            y: (viewSize.height - renderedSize.height) / 2
        )
        return CGPoint(
            x: offset.x + point.x * renderedSize.width,
            y: offset.y + (1 - point.y) * renderedSize.height
        )
    }

    func rect(fromVisionNormalized rect: CGRect) -> CGRect {
        let topLeft = point(fromVisionNormalized: CGPoint(x: rect.minX, y: rect.maxY))
        let bottomRight = point(fromVisionNormalized: CGPoint(x: rect.maxX, y: rect.minY))
        return CGRect(
            x: topLeft.x,
            y: topLeft.y,
            width: bottomRight.x - topLeft.x,
            height: bottomRight.y - topLeft.y
        )
    }
}
