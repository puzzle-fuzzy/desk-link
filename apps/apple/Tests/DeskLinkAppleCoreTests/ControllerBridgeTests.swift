import Foundation
import DeskLinkC
import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class ControllerBridgeTests: XCTestCase {
    func testUnpresentedSurfaceCannotCaptureOrEncodeInput() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))
        bridge.recordInputsForTesting = true
        XCTAssertNil(bridge.captureInputContext())
        bridge.send(input: .unicode("pending", modifiers: []))
        XCTAssertTrue(bridge.sentInputsForTesting.isEmpty)
    }

    func testCapturedTextAndReleaseKeepOriginalWireContextAcrossPresentedEpochChange() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))
        bridge.recordInputsForTesting = true
        let old = CapturedInputContext(streamID: 4, inputEpoch: 9, configVersion: 1, runtimeGeneration: 0)
        bridge.acceptPresentedInputContextForTesting(CapturedInputContext(streamID: 4, inputEpoch: 10, configVersion: 2, runtimeGeneration: 0))
        bridge.send(input: .unicode("中😀", modifiers: []), context: old)
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.stream_id), [4, 4, 4, 4])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.input_epoch), [9, 9, 9, 9])
        bridge.releaseAll(context: old)
        XCTAssertEqual(bridge.releasedContextsForTesting, [old])
    }

    func testInputRestrictionAndRecoveryKeepSessionAndRepeatedAcknowledgementsAdvance() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))
        bridge.acceptControlForTesting(Data([14, 1, 0]))
        XCTAssertEqual(bridge.inputRestriction, .blocked)
        XCTAssertEqual(bridge.state, .connected(streamID: 4))
        bridge.acceptControlForTesting(Data([14, 0]))
        XCTAssertNil(bridge.inputRestriction)
        bridge.acceptControlForTesting(Data([10, 0, 1]))
        let first = bridge.qualityStateRevision
        bridge.acceptControlForTesting(Data([10, 0, 1]))
        XCTAssertEqual(bridge.qualityStateRevision, first + 1)
        bridge.acceptControlForTesting(Data([14, 1, 0]))
        bridge.disconnect()
        XCTAssertNil(bridge.inputRestriction)
        XCTAssertNil(bridge.confirmedQuality)
    }

    func testSameHandleReconnectReplaysSmallerStreamAndVersionOne() {
        let bridge = ControllerBridge.testing()
        bridge.stageDecodedMetadataForTesting(streamID: 9, version: 4, inputEpoch: 99)
        bridge.acceptStateForTesting(streamID: 9)
        bridge.retireDecodeStreamForTesting()
        bridge.stageDecodedMetadataForTesting(streamID: 1, version: 1, inputEpoch: 2)
        bridge.acceptReconnectingForTesting() // Delayed MainActor state after future metadata.
        bridge.acceptStateForTesting(streamID: 1)
        XCTAssertEqual(bridge.state, .connected(streamID: 1))
        XCTAssertEqual(bridge.activeConfigVersionForTesting, 1)
        XCTAssertNil(bridge.presentedInputContext)
    }

    func testDelayedReconnectDoesNotClearCurrentFuturePresentationMetadata() {
        let bridge = ControllerBridge.testing()
        bridge.stageDecodedMetadataForTesting(streamID: 9, version: 4, inputEpoch: 99)
        bridge.acceptStateForTesting(streamID: 9)
        bridge.retireDecodeStreamForTesting()
        bridge.stageDecodedMetadataForTesting(streamID: 9, version: 1, inputEpoch: 2)
        // Explicit metadata fixture only; this does not prove native presentation.
        let future = CapturedInputContext(streamID: 9, inputEpoch: 2, configVersion: 1, runtimeGeneration: 0)
        bridge.acceptPresentedInputContextForTesting(future)
        bridge.acceptReconnectingForTesting()
        XCTAssertEqual(bridge.state, .connected(streamID: 9))
        XCTAssertEqual(bridge.activeConfigVersionForTesting, 1)
        XCTAssertEqual(bridge.presentedInputContext, future)
    }

    func testQualityVersionDoesNotDiscardPresentedSurfaceContext() {
        let bridge = ControllerBridge.testing()
        bridge.stageDecodedMetadataForTesting(streamID: 9, version: 1, inputEpoch: 2)
        bridge.acceptStateForTesting(streamID: 9)
        let presented = CapturedInputContext(streamID: 9, inputEpoch: 2, configVersion: 1, runtimeGeneration: 0)
        bridge.acceptPresentedInputContextForTesting(presented)
        bridge.stageDecodedMetadataForTesting(streamID: 9, version: 2, inputEpoch: 2)
        XCTAssertEqual(bridge.presentedInputContext, presented)
        XCTAssertEqual(bridge.activeConfigVersionForTesting, 2)
    }

    func testDecodedConfigurationArrivingBeforeConnectedIsReplayed() {
        let bridge = ControllerBridge.testing()
        bridge.stageDecodedMetadataForTesting(streamID: 4, version: 1)
        XCTAssertNil(bridge.videoSize)
        bridge.acceptStateForTesting(streamID: 4)
        XCTAssertEqual(bridge.activeConfigVersionForTesting, 1)
        XCTAssertNil(bridge.videoSize) // Metadata is not a drawable presentation.
        XCTAssertNil(bridge.presentedInputContext)
    }

    func testDisconnectDiscardsDecodedConfigurationWaitingForState() {
        let bridge = ControllerBridge.testing()
        bridge.stageDecodedMetadataForTesting(streamID: 4, version: 1)
        bridge.disconnect()
        bridge.acceptStateForTesting(streamID: 4)
        XCTAssertNil(bridge.videoSize)
    }

    func testMultilineCommittedTextMapsCRLFAndTabToBalancedSpecialKeys() {
        let bridge = ControllerBridge.testing()
        bridge.recordInputsForTesting = true
        // Metadata fixture exercises encoding only, not drawable presentation.
        bridge.acceptPresentedInputContextForTesting(CapturedInputContext(streamID: 4, inputEpoch: 9, configVersion: 1, runtimeGeneration: 0))
        bridge.send(input: .unicode("中\r\n\t😀\n", modifiers: []))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0, 1, 0, 1, 0, 1, 0, 1, 0])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.key_code), [0, 0, 1, 1, 4, 4, 0, 0, 1, 1])
    }

    func testPhysicalCharacterEncodesPressedStateAndModifierMaskIntoFFIInput() {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 4))
        bridge.recordInputsForTesting = true
        // Metadata fixture exercises encoding only, not drawable presentation.
        bridge.acceptPresentedInputContextForTesting(CapturedInputContext(streamID: 4, inputEpoch: 9, configVersion: 1, runtimeGeneration: 0))
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
        // Metadata fixture exercises encoding only, not drawable presentation.
        bridge.acceptPresentedInputContextForTesting(CapturedInputContext(streamID: 4, inputEpoch: 9, configVersion: 1, runtimeGeneration: 0))
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
