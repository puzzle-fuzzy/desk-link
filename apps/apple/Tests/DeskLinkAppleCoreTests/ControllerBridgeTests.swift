import Foundation
import DeskLinkC
import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class ControllerBridgeTests: XCTestCase {
    func testMultilineCommittedTextMapsCRLFAndTabToBalancedSpecialKeys() {
        let bridge = ControllerBridge.testing()
        bridge.recordInputsForTesting = true
        bridge.send(input: .unicode("中\r\n\t😀\n", modifiers: []))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0, 1, 0, 1, 0, 1, 0, 1, 0])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.key_code), [0, 0, 1, 1, 4, 4, 0, 0, 1, 1])
    }

    func testPhysicalCharacterEncodesPressedStateAndModifierMaskIntoFFIInput() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))
        bridge.recordInputsForTesting = true
        bridge.send(input: .character(65, pressed: true, modifiers: [.shift, .control]))
        bridge.send(input: .character(65, pressed: false, modifiers: [.shift, .control]))
        let inputs = bridge.sentInputsForTesting
        XCTAssertEqual(inputs.count, 2)
        XCTAssertTrue(inputs.allSatisfy { $0.kind == DESKLINK_INPUT_KEY && $0.key_code == 0 })
        XCTAssertEqual(inputs.map(\.character), [65, 65])
        XCTAssertEqual(inputs.map(\.pressed), [1, 0])
        XCTAssertEqual(inputs.map(\.modifiers), [3, 3])
    }

    func testCommittedUnicodeEncodesOrderedDownUpForEveryScalar() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))
        bridge.recordInputsForTesting = true
        bridge.send(input: .unicode("中😀", modifiers: []))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.character), [0x4e2d, 0x4e2d, 0x1f600, 0x1f600])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0, 1, 0])
    }

    func testInputRecordingRequiresExplicitTestOptIn() {
        let bridge = ControllerBridge.testing()
        bridge.send(input: .unicode("test", modifiers: []))
        XCTAssertTrue(bridge.sentInputsForTesting.isEmpty)
    }

    func testRecoverableErrorPreservesRuntimeLifecycleAndCanReconnect() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))
        bridge.acceptErrorForTesting(stateValue: 10, recoverable: true)
        XCTAssertEqual(bridge.state, .reconnecting)
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 0)
        bridge.acceptStateForTesting(streamID: 5)
        XCTAssertEqual(bridge.state, .connected(streamID: 5))
        XCTAssertNil(bridge.lastError)
        bridge.acceptErrorForTesting(stateValue: 12, recoverable: false)
        if case .failed = bridge.state {} else { XCTFail("terminal error must fail") }
    }

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
