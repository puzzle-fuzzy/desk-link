import DeskLinkAppleCore
import XCTest
@testable import DeskLinkIOS

final class IOSSessionPresentationTests: XCTestCase {
    func testSessionPresentationIncludesRecoveryStates() {
        XCTAssertTrue(IOSSessionPresentation.isActive(.connected(streamID: 7)))
        XCTAssertTrue(IOSSessionPresentation.isActive(.reconnecting))
        XCTAssertTrue(IOSSessionPresentation.isActive(.recovering))
        XCTAssertTrue(IOSSessionPresentation.isActive(.frozen))
        XCTAssertFalse(IOSSessionPresentation.isActive(.idle))
    }

    func testStatusTextDistinguishesApprovalFromConnectionSetup() {
        XCTAssertEqual(IOSSessionPresentation.statusText(.pairing), "等待用户审批")
        XCTAssertEqual(IOSSessionPresentation.statusText(.connecting), "正在建立安全连接")
    }

    func testAspectFitGeometryKeepsLetterboxOutsideVideo() {
        let rect = VideoGeometry.aspectFit(
            source: CGSize(width: 1920, height: 1080),
            in: CGRect(x: 0, y: 0, width: 390, height: 844)
        )

        XCTAssertFalse(rect.contains(CGPoint(x: 0, y: 0)))
        XCTAssertTrue(rect.contains(CGPoint(x: 195, y: 422)))
    }
}
