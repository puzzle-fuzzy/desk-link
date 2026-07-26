import CoreGraphics
import XCTest
@testable import DeskLinkAppleCore

final class VideoGeometryTests: XCTestCase {
    func testAspectFitLetterboxesWideVideoInsideSquareSurface() {
        let actual = VideoGeometry.aspectFit(
            source: CGSize(width: 1920, height: 1080),
            in: CGRect(x: 0, y: 0, width: 1000, height: 1000)
        )
        let expected = CGRect(x: 0, y: 218.75, width: 1000, height: 562.5)
        let accuracy = CGFloat(1e-12)

        XCTAssertEqual(actual.origin.x, expected.origin.x, accuracy: accuracy)
        XCTAssertEqual(actual.origin.y, expected.origin.y, accuracy: accuracy)
        XCTAssertEqual(actual.width, expected.width, accuracy: accuracy)
        XCTAssertEqual(actual.height, expected.height, accuracy: accuracy)
    }

    func testAspectFitRejectsEmptyDimensions() {
        XCTAssertEqual(
            VideoGeometry.aspectFit(source: .zero, in: CGRect(x: 0, y: 0, width: 100, height: 100)),
            .zero
        )
    }

    func testNormalizedTopLeftPointPreservesVerticalDirection() {
        let rect = CGRect(x: 20, y: 40, width: 800, height: 450)

        XCTAssertEqual(
            VideoGeometry.normalizedTopLeftPoint(
                for: CGPoint(x: 420, y: 40),
                in: rect
            ),
            CGPoint(x: 0.5, y: 0)
        )
        XCTAssertEqual(
            VideoGeometry.normalizedTopLeftPoint(
                for: CGPoint(x: 420, y: 490),
                in: rect
            ),
            CGPoint(x: 0.5, y: 1)
        )
        XCTAssertNil(VideoGeometry.normalizedTopLeftPoint(for: CGPoint(x: 10, y: 100), in: rect))
    }
}
