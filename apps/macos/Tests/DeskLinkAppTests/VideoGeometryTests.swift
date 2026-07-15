import CoreGraphics
import XCTest
@testable import DeskLinkApp

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
}
