import Foundation
import XCTest
@testable import DeskLinkApp

@MainActor
final class ControllerBridgeTests: XCTestCase {
    func testControllerErrorDoesNotExposeRelayAuthentication() {
        let bridge = ControllerBridge.testing(error: "relay authentication failed: AUTH_KEY relay secret")

        XCTAssertFalse(bridge.userFacingError.contains("AUTH_KEY"))
        XCTAssertFalse(bridge.userFacingError.contains("relay secret"))
    }

    func testSavedHostEncodingRoundTripsApprovedMaterial() throws {
        let host = SavedHost(
            id: UUID(),
            serverName: "relay.example.com",
            sessionID: [UInt8](repeating: 1, count: 16),
            relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: 3, count: 32)
        )

        XCTAssertEqual(try SavedHostStore.decode(try SavedHostStore.encode([host])), [host])
    }

    func testDisconnectReleasesInputsAndClearsDisplayState() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))

        bridge.disconnect()

        XCTAssertEqual(bridge.state, .closed)
        XCTAssertNil(bridge.latestPixelBuffer)
    }
}
