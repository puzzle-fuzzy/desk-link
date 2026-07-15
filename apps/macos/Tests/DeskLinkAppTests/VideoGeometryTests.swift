import CoreGraphics
import XCTest
@testable import DeskLinkApp

final class VideoGeometryTests: XCTestCase {
    func testAspectFitLetterboxesWideVideoInsideSquareSurface() {
        XCTAssertEqual(
            VideoGeometry.aspectFit(
                source: CGSize(width: 1920, height: 1080),
                in: CGRect(x: 0, y: 0, width: 1000, height: 1000)
            ),
            CGRect(x: 0, y: 218.75, width: 1000, height: 562.5)
        )
    }

    func testAspectFitRejectsEmptyDimensions() {
        XCTAssertEqual(
            VideoGeometry.aspectFit(source: .zero, in: CGRect(x: 0, y: 0, width: 100, height: 100)),
            .zero
        )
    }
}
