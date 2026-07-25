import XCTest
@testable import UntitledMobileFPS

final class PreviewGeometryTests: XCTestCase {
    func testVisionYAxisIsFlipped() {
        let geometry = PreviewGeometry(viewSize: CGSize(width: 100, height: 200), imageSize: CGSize(width: 100, height: 200))
        XCTAssertEqual(geometry.point(fromVisionNormalized: CGPoint(x: 0, y: 1)), CGPoint(x: 0, y: 0))
        XCTAssertEqual(geometry.point(fromVisionNormalized: CGPoint(x: 1, y: 0)), CGPoint(x: 100, y: 200))
    }

    func testAspectFillUsesSameCenterCropAsPreview() {
        let geometry = PreviewGeometry(viewSize: CGSize(width: 100, height: 200), imageSize: CGSize(width: 200, height: 200))
        XCTAssertEqual(geometry.point(fromVisionNormalized: CGPoint(x: 0.5, y: 0.5)), CGPoint(x: 50, y: 100))
        XCTAssertEqual(geometry.point(fromVisionNormalized: CGPoint(x: 0, y: 0.5)).x, -50, accuracy: 0.001)
    }

    func testNormalizedRectConversion() {
        let geometry = PreviewGeometry(viewSize: CGSize(width: 100, height: 200), imageSize: CGSize(width: 100, height: 200))
        let rect = geometry.rect(fromVisionNormalized: CGRect(x: 0.2, y: 0.3, width: 0.4, height: 0.5))
        XCTAssertEqual(rect.origin.x, 20, accuracy: 0.001)
        XCTAssertEqual(rect.origin.y, 40, accuracy: 0.001)
        XCTAssertEqual(rect.width, 40, accuracy: 0.001)
        XCTAssertEqual(rect.height, 100, accuracy: 0.001)
    }
}
