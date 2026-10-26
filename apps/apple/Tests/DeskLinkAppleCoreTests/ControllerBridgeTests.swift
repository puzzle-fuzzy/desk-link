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

    func testRecoverableErrorPreservesRuntimeAndRetiresInputsAndStream() {
        let bridge = localRuntimeBridge()
        bridge.connect(deviceID: "123456789012", temporaryPassword: "ABCD2345")
        bridge.acceptStateForTesting(streamID: 8)
        let generation = bridge.callbackGenerationForTesting
        XCTAssertTrue(bridge.activeRuntimeForTesting)

        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_ERROR.rawValue),
            stateValue: Int(DESKLINK_RECONNECTING.rawValue), message: "transport unavailable: secret must never be shown")

        XCTAssertEqual(bridge.state, .reconnecting)
        XCTAssertEqual(bridge.connectionStage, .reconnecting)
        XCTAssertTrue(bridge.activeRuntimeForTesting)
        XCTAssertEqual(bridge.callbackGenerationForTesting, generation)
        XCTAssertFalse(bridge.userFacingError.contains("secret"))
        // Input retirement belongs to the explicit reconnect boundary event.
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue),
            stateValue: Int(DESKLINK_RECONNECTING.rawValue))
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
        XCTAssertEqual(bridge.activeStreamIDForTesting, 0)
        bridge.acceptStateForTesting(streamID: 8)
        XCTAssertEqual(bridge.state, .reconnecting, "a retired stream cannot resurrect during retry")
        bridge.acceptStateForTesting(streamID: 9)
        XCTAssertEqual(bridge.state, .reconnecting, "no stream is accepted before the new handshake")
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue),
            stateValue: Int(DESKLINK_SECURE_HANDSHAKE.rawValue))
        bridge.acceptStateForTesting(streamID: 9)
        XCTAssertEqual(bridge.state, .connected(streamID: 9))
        XCTAssertNil(bridge.lastError)
        bridge.disconnect()
    }

    func testReconnectedHostCanReuseOrLowerItsStreamIDAfterNewHandshake() {
        for resumedStreamID in [UInt64(8), UInt64(1)] {
            let bridge = ControllerBridge.testing()
            bridge.acceptStateForTesting(streamID: 8)
            bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue),
                stateValue: Int(DESKLINK_RECONNECTING.rawValue))
            bridge.acceptStateForTesting(streamID: resumedStreamID)
            XCTAssertEqual(bridge.state, .reconnecting)

            for stage in [DESKLINK_SECURE_HANDSHAKE, DESKLINK_WAITING_FOR_APPROVAL, DESKLINK_NEGOTIATING_CAPABILITIES, DESKLINK_STARTING_VIDEO] {
                bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue), stateValue: Int(stage.rawValue))
                XCTAssertEqual(bridge.state, .reconnecting, "recovery must keep the existing session view")
                XCTAssertEqual(bridge.connectionStage.rawValue, Int(stage.rawValue))
            }
            bridge.acceptStateForTesting(streamID: resumedStreamID)
            XCTAssertEqual(bridge.state, .connected(streamID: resumedStreamID))
            XCTAssertEqual(bridge.activeStreamIDForTesting, resumedStreamID)
            XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
        }
    }

    func testFailedReconnectHandshakeCannotAdmitAStreamDuringNextRetry() {
        let bridge = ControllerBridge.testing()
        bridge.acceptStateForTesting(streamID: 8)
        for stage in [DESKLINK_RECONNECTING, DESKLINK_SECURE_HANDSHAKE, DESKLINK_RECONNECTING] {
            bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue), stateValue: Int(stage.rawValue))
        }
        for streamID in [UInt64(1), UInt64(8), UInt64(9)] { bridge.acceptStateForTesting(streamID: streamID) }
        XCTAssertEqual(bridge.state, .reconnecting)
        XCTAssertEqual(bridge.activeStreamIDForTesting, 0)
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue),
            stateValue: Int(DESKLINK_SECURE_HANDSHAKE.rawValue))
        bridge.acceptStateForTesting(streamID: 1)
        XCTAssertEqual(bridge.state, .connected(streamID: 1))
    }

    func testFatalErrorClosesRuntimeAndOldGenerationCannotChangeNextAttempt() {
        let bridge = localRuntimeBridge()
        bridge.connect(deviceID: "123456789012", temporaryPassword: "ABCD2345")
        bridge.acceptStateForTesting(streamID: 8)
        let retiredGeneration = bridge.callbackGenerationForTesting
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_ERROR.rawValue),
            stateValue: Int(DESKLINK_CLOSED.rawValue), streamID: 8, message: "authentication failed: private secret")

        XCTAssertFalse(bridge.activeRuntimeForTesting)
        XCTAssertEqual(bridge.connectionStage, .closed)
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
        guard case .failed = bridge.state else { return XCTFail("fatal errors must stay visible") }

        bridge.connect(deviceID: "123456789012", temporaryPassword: "ABCD2345")
        bridge.acceptStateForTesting(streamID: 1)
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_ERROR.rawValue),
            stateValue: Int(DESKLINK_CLOSED.rawValue), message: "old worker error", generation: retiredGeneration)
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue),
            stateValue: Int(DESKLINK_CONNECTED.rawValue), streamID: 99, generation: retiredGeneration)
        XCTAssertEqual(bridge.state, .connected(streamID: 1))
        XCTAssertTrue(bridge.activeRuntimeForTesting)
        bridge.disconnect()
    }

    func testOldStreamCannotCloseOrDegradeNewStream() {
        let bridge = ControllerBridge.testing()
        bridge.acceptStateForTesting(streamID: 8)
        bridge.acceptStateForTesting(streamID: 9)
        for state in [DESKLINK_CLOSED, DESKLINK_DEGRADED, DESKLINK_RECONNECTING] {
            bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue),
                stateValue: Int(state.rawValue), streamID: 8)
        }
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_ERROR.rawValue),
            stateValue: Int(DESKLINK_CLOSED.rawValue), streamID: 8, message: "retired stream failed")
        XCTAssertEqual(bridge.state, .connected(streamID: 9))
        XCTAssertEqual(bridge.connectionStage, .connected)
        XCTAssertNil(bridge.lastError)
    }

    func testRealConnectionStagesAndUnknownEvents() {
        let bridge = ControllerBridge.testing()
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue),
            stateValue: Int(DESKLINK_SECURE_HANDSHAKE.rawValue))
        XCTAssertEqual(bridge.connectionStage, .secureHandshake)
        for kind in [Int(DESKLINK_EVENT_TRANSFER.rawValue), Int(DESKLINK_EVENT_AUDIO.rawValue), 999] {
            bridge.acceptEventForTesting(eventKind: kind, stateValue: Int(DESKLINK_CLOSED.rawValue))
        }
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue), stateValue: 999)
        XCTAssertEqual(bridge.connectionStage, .secureHandshake)
        XCTAssertEqual(bridge.state, .connecting)
    }

    func testSavedHostReconnectUpdatesRecencyAndPreservesNameAndDeviceID() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain)
        let previous = SavedHost(id: UUID(), serverName: "localhost",
            sessionID: [UInt8](repeating: 1, count: 16), relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: 3, count: 32), expiresAtUnixS: 0, authorizationType: 2,
            deviceID: "123456789012", displayName: "工作电脑",
            lastConnectedAt: Date(timeIntervalSince1970: 1))
        try store.save(previous)
        let bridge = ControllerBridge(configuration: .init(relayURL: "quic://127.0.0.1:1", relayServerName: "localhost", platform: .macos),
            identityStore: ControllerIdentityStore(keychain: keychain), savedHostStore: store)
        bridge.connect(savedHost: previous)
        bridge.acceptStateForTesting(streamID: 1)
        let saved = try XCTUnwrap(store.loadAll().first)
        XCTAssertEqual(saved.id, previous.id)
        XCTAssertEqual(saved.deviceID, previous.deviceID)
        XCTAssertEqual(saved.displayName, previous.displayName)
        XCTAssertGreaterThan(try XCTUnwrap(saved.lastConnectedAt), try XCTUnwrap(previous.lastConnectedAt))
        let began = bridge.connectionAttemptStartedAt
        bridge.disconnect()
        XCTAssertEqual(bridge.connectionAttemptStartedAt, began, "completed attempt duration remains available to UI")
    }

    private func localRuntimeBridge() -> ControllerBridge {
        ControllerBridge.testing(configuration: .init(relayURL: "quic://127.0.0.1:1", relayServerName: "localhost", platform: .macos))
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
