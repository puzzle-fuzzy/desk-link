import SwiftUI
import XCTest
import DeskLinkAppleCore
@testable import DeskLinkApp

final class FirstUseGuideTests: XCTestCase {
    @MainActor
    func testGuideShowsOnlyWithoutSavedHosts() {
        let bridge = ControllerBridge.testing()
        let draft = DeskLinkConnectionDraft()
        XCTAssertTrue(
            ConnectView(bridge: bridge, draft: draft, hasSavedHosts: false).firstUseGuideVisible)
        XCTAssertFalse(
            ConnectView(bridge: bridge, draft: draft, hasSavedHosts: true).firstUseGuideVisible)
    }
}
