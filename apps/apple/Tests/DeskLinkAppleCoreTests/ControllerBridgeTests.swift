import Foundation
import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class ControllerBridgeTests: XCTestCase {
    func testControllerErrorDoesNotExposeRelayAuthentication() {
        let bridge = ControllerBridge.testing(error: "relay authentication failed: AUTH_KEY relay secret")

        XCTAssertFalse(bridge.userFacingError.contains("AUTH_KEY"))
        XCTAssertFalse(bridge.userFacingError.contains("relay secret"))
        XCTAssertTrue(bridge.userFacingError.contains("安全连接校验失败"))
    }

    func testControllerErrorsAreLocalizedForCommonPairingFailures() {
        let configuration = ControllerBridge.testing(error: "The DeskLink runtime configuration is invalid.")
        let invite = ControllerBridge.testing(error: "The pairing invitation is invalid.")

        XCTAssertEqual(configuration.userFacingError, "DeskLink 连接配置无效，请检查中继地址和服务器名称。")
        XCTAssertEqual(invite.userFacingError, "连接码无效，请重新扫描二维码或粘贴完整连接码。")
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
        XCTAssertEqual(bridge.state, .failed("DeskLink 连接配置无效，请检查中继地址和服务器名称。"))
    }

    func testInvalidInviteDoesNotStartRustRuntime() {
        let bridge = ControllerBridge.testing(
            configuration: DeskLinkRuntimeConfiguration(
                relayURL: "quic://127.0.0.1:4433",
                relayServerName: "localhost",
                platform: .ios
            )
        )

        bridge.connect(invite: Data("not-a-desklink-invite".utf8))

        XCTAssertEqual(bridge.state, .failed("连接码无效，请重新扫描二维码或粘贴完整连接码。"))
        XCTAssertFalse(bridge.activeRuntimeForTesting)
    }
}
