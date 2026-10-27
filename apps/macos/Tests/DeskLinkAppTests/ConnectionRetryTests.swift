import XCTest
import DeskLinkAppleCore
@testable import DeskLinkApp

/// MVP-06: a terminal failure keeps the original connect target and the
/// one-click retry re-enters the connect flow with it unchanged.
@MainActor
final class ConnectionRetryTests: XCTestCase {
    func testFailedCredentialConnectCanRetryWithOriginalTarget() {
        let bridge = ControllerBridge.testing()
        let draft = DeskLinkConnectionDraft()
        draft.deviceID = "123456789012"
        draft.accessPassword = "p@ssw0rd"

        draft.connectDevice(using: bridge)
        XCTAssertEqual(bridge.state, .connecting)
        draft.observe(bridge.state)

        // Terminal failure surfaces the retry affordance, not a dead end.
        bridge.publishErrorForTesting("connection refused")
        XCTAssertEqual(bridge.state, .failed("连接失败，请检查网络和中继配置后重试。"))
        draft.observe(bridge.state)
        XCTAssertTrue(draft.canRetry, "failed connect must offer one-click retry")

        draft.retry(using: bridge)
        XCTAssertEqual(bridge.state, .connecting, "retry restarts the connect flow")
    }

    func testFailedSavedHostConnectRetryKeepsTheSavedTarget() {
        let bridge = ControllerBridge.testing()
        let draft = DeskLinkConnectionDraft()
        let host = SavedHost(id: UUID(), serverName: "localhost",
            sessionID: [UInt8](repeating: 1, count: 16),
            relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: 3, count: 32),
            expiresAtUnixS: 0, authorizationType: 2,
            deviceID: "123456789012", displayName: "工作电脑",
            lastConnectedAt: Date(timeIntervalSince1970: 1))

        draft.connect(host, using: bridge)
        XCTAssertEqual(bridge.state, .connecting)
        draft.observe(bridge.state)

        bridge.publishErrorForTesting("connection refused")
        draft.observe(bridge.state)
        XCTAssertTrue(draft.canRetry)
        XCTAssertEqual(draft.targetName, "工作电脑")

        draft.retry(using: bridge)
        XCTAssertEqual(bridge.state, .connecting)
    }
}
