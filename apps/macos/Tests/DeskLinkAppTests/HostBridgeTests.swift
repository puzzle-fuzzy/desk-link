import XCTest
@testable import DeskLinkApp

@MainActor
final class HostBridgeTests: XCTestCase {
    func testUnknownEventIsCountedAndTransferDoesNotBecomeFatal() {
        let bridge = HostBridge.testing(permissionSnapshot: .denied)
        bridge.acceptEventKindForTesting(UInt32.max, poisonUnrelatedFields: true)
        XCTAssertEqual(bridge.ignoredHostEventCount, 1)
        XCTAssertEqual(bridge.state, .idle)
        XCTAssertNil(bridge.lastError)
        bridge.acceptEventKindForTesting(8) // Generated Rust/C Host Transfer kind.
        XCTAssertEqual(bridge.ignoredHostEventCount, 1)
        XCTAssertEqual(bridge.state, .idle)
        XCTAssertTrue(bridge.lastError?.contains("尚不支持") == true)
    }

    func testHostBridgeStartsSafeAndApprovalIsNotImplicit() {
        let bridge = HostBridge.testing(permissionSnapshot: .denied)

        XCTAssertEqual(bridge.state, .idle)
        XCTAssertFalse(bridge.canStartCapture)
        XCTAssertFalse(bridge.canInjectInput)
        XCTAssertFalse(bridge.pendingApproval?.isApproved ?? false)
    }
}
