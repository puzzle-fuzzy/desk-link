import Foundation
import XCTest
@testable import DeskLinkAppleCore

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
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
    }

    func testInvalidRuntimeConfigurationDoesNotAllocateRustHandle() {
        let bridge = ControllerBridge.testing(
            configuration: DeskLinkRuntimeConfiguration(
                relayURL: "",
                relayServerName: "localhost",
                platform: .ios
            )
        )

        bridge.connect(invite: Data(repeating: 0, count: 181))

        XCTAssertFalse(bridge.activeRuntimeForTesting)
        XCTAssertEqual(bridge.state, .failed("The DeskLink runtime configuration is invalid."))
    }
}
