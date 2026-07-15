import XCTest
@testable import DeskLinkApp

@MainActor
final class HostBridgeTests: XCTestCase {
    func testHostBridgeStartsSafeAndApprovalIsNotImplicit() {
        let bridge = HostBridge.testing(permissionSnapshot: .denied)

        XCTAssertEqual(bridge.state, .idle)
        XCTAssertFalse(bridge.canStartCapture)
        XCTAssertFalse(bridge.canInjectInput)
        XCTAssertFalse(bridge.pendingApproval?.isApproved ?? false)
    }
}
