import Foundation
import DeskLinkC
import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class ControllerSessionFeaturesTests: XCTestCase {
    func testDisplaySelectionWaitsForPeerConfirmationAndANewerPresentedFrame() throws {
        for frameFirst in [true, false] {
            let bridge = connectedBridge()
            var requests: [ControllerFeatureRequest] = []
            bridge.featureRequestForTesting = { requests.append($0); return true }
            acceptDisplays(bridge, selected: 0)
            let oldContext = bridge.inputContextID

            XCTAssertTrue(bridge.selectDisplay(1))
            XCTAssertEqual(bridge.selectedDisplayID, 0, "the local choice is not remote confirmation")
            XCTAssertEqual(bridge.pendingDisplayID, 1)
            XCTAssertTrue(bridge.isAwaitingDisplayFrame)
            XCTAssertFalse(bridge.canSendRemoteInput)
            XCTAssertGreaterThan(bridge.inputContextID, oldContext)
            XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
            XCTAssertEqual(requests, [.selectDisplay(streamID: 4, displayID: 1)])

            if frameFirst {
                bridge.confirmDisplayFrameForTesting()
                XCTAssertFalse(bridge.canSendRemoteInput)
                acceptDisplays(bridge, selected: 1)
                XCTAssertFalse(bridge.canSendRemoteInput, "a frame presented before the selection acknowledgement is insufficient")
                bridge.confirmDisplayFrameForTesting()
            } else {
                acceptDisplays(bridge, selected: 1)
                XCTAssertFalse(bridge.canSendRemoteInput, "a DisplayList alone cannot reopen old coordinates")
                bridge.confirmDisplayFrameForTesting()
            }
            XCTAssertEqual(bridge.selectedDisplayID, 1)
            XCTAssertNil(bridge.pendingDisplayID)
            XCTAssertFalse(bridge.isAwaitingDisplayFrame)
            XCTAssertTrue(bridge.canSendRemoteInput)
        }
    }

    func testDisplayTimeoutAllowsRetryAndKeepsCoordinatesBlocked() {
        let bridge = connectedBridge()
        bridge.featureRequestForTesting = { _ in true }
        acceptDisplays(bridge, selected: 0)
        XCTAssertTrue(bridge.selectDisplay(1))
        bridge.expireFeatureRequestsForTesting()
        XCTAssertNil(bridge.pendingDisplayID)
        XCTAssertNotNil(bridge.displayError)
        XCTAssertTrue(bridge.isAwaitingDisplayFrame)
        XCTAssertFalse(bridge.canSendRemoteInput)
        bridge.featureRequestForTesting = { _ in false }
        XCTAssertFalse(bridge.selectDisplay(1))
        XCTAssertFalse(bridge.canSendRemoteInput, "a failed retry must not enable coordinates from an unresolved switch")
        bridge.featureRequestForTesting = { _ in true }
        XCTAssertTrue(bridge.selectDisplay(1))
        acceptDisplays(bridge, selected: 1)
        bridge.confirmDisplayFrameForTesting()
        XCTAssertNil(bridge.displayError)
        XCTAssertTrue(bridge.canSendRemoteInput)
    }

    func testRejectedDisplaySubmissionPreservesInputAndInvalidDisplayDoesNotReleaseKeys() {
        let bridge = connectedBridge()
        bridge.featureRequestForTesting = { _ in false }
        acceptDisplays(bridge, selected: 0)
        XCTAssertFalse(bridge.selectDisplay(99))
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 0)
        XCTAssertFalse(bridge.selectDisplay(1))
        XCTAssertNotNil(bridge.displayError)
        XCTAssertFalse(bridge.isAwaitingDisplayFrame)
        XCTAssertTrue(bridge.canSendRemoteInput)
    }

    func testQualityUsesActualPeerStateAndConfirmationTimeoutIsRetryable() {
        let bridge = connectedBridge()
        bridge.featureRequestForTesting = { _ in true }
        acceptQuality(bridge, preference: "automatic", preset: "sharp")
        XCTAssertTrue(bridge.chooseQuality(.smooth))
        XCTAssertEqual(bridge.qualityPreference, .automatic)
        XCTAssertEqual(bridge.appliedQualityPreset, .sharp)
        XCTAssertEqual(bridge.pendingQuality, .smooth)
        acceptQuality(bridge, preference: "automatic", preset: "balanced")
        XCTAssertEqual(bridge.pendingQuality, .smooth, "an unrelated automatic update is not this request's acknowledgement")
        XCTAssertEqual(bridge.appliedQualityPreset, .balanced)
        bridge.expireFeatureRequestsForTesting()
        XCTAssertNil(bridge.pendingQuality)
        XCTAssertNotNil(bridge.qualityError)
        XCTAssertTrue(bridge.chooseQuality(.smooth))
        acceptQuality(bridge, preference: "smooth", preset: "smooth")
        XCTAssertEqual(bridge.qualityPreference, .smooth)
        XCTAssertEqual(bridge.appliedQualityPreset, .smooth)
        XCTAssertNil(bridge.pendingQuality)
        XCTAssertNil(bridge.qualityError)
    }

    func testFeatureStatesIgnoreMalformedUnknownAndRetiredStreamEvents() {
        let bridge = connectedBridge()
        acceptDisplays(bridge, selected: 0)
        acceptQuality(bridge, preference: "automatic", preset: "sharp")
        bridge.acceptEventForTesting(eventKind: 13, stateValue: 0, streamID: 4, message: "not json")
        acceptDisplays(bridge, selected: 1, streamID: 3)
        acceptQuality(bridge, preference: "smooth", preset: "smooth", streamID: 3)
        XCTAssertEqual(bridge.selectedDisplayID, 0)
        XCTAssertEqual(bridge.qualityPreference, .automatic)
        bridge.acceptStateForTesting(streamID: 5)
        XCTAssertTrue(bridge.displays.isEmpty)
        XCTAssertNil(bridge.selectedDisplayID)
        XCTAssertNil(bridge.appliedQualityPreset)
        acceptDisplays(bridge, selected: 1)
        XCTAssertTrue(bridge.displays.isEmpty)
    }

    func testClipboardReceiveMatchesRequestOperationStreamAndDoesNotWriteForUnsolicitedData() throws {
        let bridge = connectedBridge()
        var requests: [ControllerFeatureRequest] = []
        bridge.featureRequestForTesting = { requests.append($0); return true }
        var received: [String] = []
        bridge.onRemoteClipboardText = { received.append($0) }
        XCTAssertTrue(bridge.requestRemoteClipboard())
        guard case .requestClipboard(let streamID, let requestID) = requests.last else { return XCTFail("missing request") }
        acceptClipboard(bridge, requestID: requestID + 1, text: "unsolicited")
        acceptClipboard(bridge, requestID: requestID, operation: "send", text: "wrong operation")
        acceptClipboard(bridge, requestID: requestID, streamID: streamID - 1, text: "old stream")
        XCTAssertTrue(received.isEmpty)
        XCTAssertTrue(bridge.isClipboardBusy)
        acceptClipboard(bridge, requestID: requestID, text: "中文和 emoji 🧪\nsecond line")
        XCTAssertEqual(received, ["中文和 emoji 🧪\nsecond line"])
        XCTAssertFalse(bridge.isClipboardBusy)
        acceptClipboard(bridge, requestID: requestID, text: "duplicate")
        XCTAssertEqual(received.count, 1)
    }

    func testClipboardTimeoutAndDisconnectRejectLateResponsesAndUseFreshRequestIDs() throws {
        let bridge = connectedBridge()
        var requests: [ControllerFeatureRequest] = []
        bridge.featureRequestForTesting = { requests.append($0); return true }
        var received = 0
        bridge.onRemoteClipboardText = { _ in received += 1 }
        XCTAssertTrue(bridge.requestRemoteClipboard())
        guard case .requestClipboard(_, let oldID) = requests.last else { return XCTFail("missing request") }
        bridge.expireFeatureRequestsForTesting()
        XCTAssertFalse(bridge.isClipboardBusy)
        XCTAssertNotNil(bridge.clipboardError)
        XCTAssertTrue(bridge.requestRemoteClipboard())
        guard case .requestClipboard(_, let currentID) = requests.last else { return XCTFail("missing retry") }
        XCTAssertGreaterThan(currentID, oldID)
        acceptClipboard(bridge, requestID: oldID, text: "late")
        XCTAssertEqual(received, 0)
        let oldGeneration = bridge.callbackGenerationForTesting
        bridge.disconnect()
        bridge.acceptStateForTesting(streamID: 4)
        acceptClipboard(bridge, requestID: currentID, text: "stale", generation: oldGeneration)
        XCTAssertEqual(received, 0)
        XCTAssertFalse(bridge.isClipboardBusy)
    }

    func testSmartPasteSendsShortcutOnlyAfterCompletedClipboardWriteInExactOrder() throws {
        let bridge = connectedBridge()
        var requests: [ControllerFeatureRequest] = []
        bridge.featureRequestForTesting = { requests.append($0); return true }
        var keys: [(UInt32, UInt32, UInt8, UInt8, UInt64)] = []
        bridge.inputForTesting = { keys.append(($0.key_code, $0.character, $0.pressed, $0.modifiers, $1)) }
        XCTAssertTrue(bridge.sendClipboardText("你好吗", paste: true))
        XCTAssertFalse(bridge.canSendRemoteInput)
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
        XCTAssertTrue(keys.isEmpty)
        guard case .sendClipboard(_, let requestID, _) = requests.last else { return XCTFail("missing send") }
        acceptClipboard(bridge, requestID: requestID, operation: "send", status: "progress", result: "completed")
        XCTAssertTrue(keys.isEmpty)
        acceptClipboard(bridge, requestID: requestID, operation: "send")
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 3, "a final release is guaranteed even if the shortcut queue partially fails")
        XCTAssertEqual(keys.count, 4)
        XCTAssertEqual(keys.map { $0.0 }, [DeskLinkKeyCode.control, 0, 0, DeskLinkKeyCode.control])
        XCTAssertEqual(keys.map { $0.1 }, [0, 118, 118, 0])
        XCTAssertEqual(keys.map { $0.2 }, [1, 1, 0, 0])
        XCTAssertEqual(keys.map { $0.3 }, [0, 2, 2, 0])
        XCTAssertEqual(keys.map { $0.4 }, [4, 4, 4, 4])
        XCTAssertTrue(bridge.canSendRemoteInput)
    }

    func testFailedOrScreenCancelledSmartPasteCannotSendShortcut() throws {
        for cancelledByScreen in [false, true] {
            let bridge = connectedBridge()
            var requests: [ControllerFeatureRequest] = []
            bridge.featureRequestForTesting = { requests.append($0); return true }
            var keyCount = 0
            bridge.inputForTesting = { _, _ in keyCount += 1 }
            XCTAssertTrue(bridge.sendClipboardText("text", paste: true))
            guard case .sendClipboard(_, let requestID, _) = requests.last else { return XCTFail("missing send") }
            if cancelledByScreen {
                acceptDisplays(bridge, selected: 0)
                XCTAssertTrue(bridge.selectDisplay(1))
                acceptClipboard(bridge, requestID: requestID, operation: "send")
            } else {
                acceptClipboard(bridge, requestID: requestID, operation: "send", status: "failed", result: "permission_denied")
            }
            XCTAssertEqual(keyCount, 0)
            XCTAssertFalse(bridge.isClipboardBusy)
        }
    }

    func testClipboardBoundsUseUTF8BytesAndBatchTextPreservesCommitOrderAndStream() {
        let bridge = connectedBridge()
        var requests: [ControllerFeatureRequest] = []
        bridge.featureRequestForTesting = { requests.append($0); return true }
        XCTAssertFalse(bridge.sendClipboardText(String(repeating: "中", count: 16_385)))
        XCTAssertTrue(requests.isEmpty)
        XCTAssertFalse(bridge.isClipboardBusy)
        XCTAssertTrue(bridge.sendText("中文🧪"))
        bridge.send(input: .unicode("下一段", modifiers: []))
        XCTAssertFalse(bridge.sendText(String(repeating: "中", count: 1_366)))
        XCTAssertFalse(bridge.sendText("bad\0text"))
        XCTAssertEqual(requests, [.sendText(streamID: 4, text: "中文🧪"), .sendText(streamID: 4, text: "下一段")])
        bridge.acceptEventForTesting(eventKind: Int(DESKLINK_EVENT_STATE.rawValue), stateValue: Int(DESKLINK_RECONNECTING.rawValue))
        XCTAssertFalse(bridge.sendText("old composition"))
        XCTAssertEqual(requests.count, 2)
    }

    func testModifiedUnicodeShortcutSendsBothEdgesInOrder() {
        let bridge = connectedBridge()
        var keys: [DesklinkInput] = []
        bridge.inputForTesting = { input, _ in keys.append(input) }
        bridge.send(input: .unicode("c", modifiers: .control))
        XCTAssertEqual(keys.map(\.character), [99, 99])
        XCTAssertEqual(keys.map(\.pressed), [1, 0])
        XCTAssertEqual(keys.map(\.modifiers), [2, 2])
    }

    func testDisplayAndQualityRequestsAreMutuallyExclusive() {
        let bridge = connectedBridge()
        bridge.featureRequestForTesting = { _ in true }
        acceptDisplays(bridge, selected: 0)
        acceptQuality(bridge, preference: "automatic", preset: "sharp")
        XCTAssertTrue(bridge.chooseQuality(.smooth))
        XCTAssertFalse(bridge.selectDisplay(1))
        acceptQuality(bridge, preference: "smooth", preset: "smooth")
        XCTAssertTrue(bridge.selectDisplay(1))
        XCTAssertFalse(bridge.chooseQuality(.sharp))
    }

    func testFocusLossCancelsOnlyDeferredPasteAndLateAcknowledgementCannotSendKeys() throws {
        let bridge = connectedBridge()
        var requests: [ControllerFeatureRequest] = []
        bridge.featureRequestForTesting = { requests.append($0); return true }
        var keyCount = 0
        bridge.inputForTesting = { _, _ in keyCount += 1 }
        XCTAssertTrue(bridge.sendClipboardText("cancelled paste", paste: true))
        guard case .sendClipboard(_, let pasteID, _) = requests.last else { return XCTFail("missing paste") }
        bridge.cancelPendingPaste()
        XCTAssertFalse(bridge.isClipboardBusy)
        XCTAssertTrue(bridge.canSendRemoteInput)
        acceptClipboard(bridge, requestID: pasteID, operation: "send")
        XCTAssertEqual(keyCount, 0)

        XCTAssertTrue(bridge.sendClipboardText("ordinary send"))
        guard case .sendClipboard(_, let sendID, _) = requests.last else { return XCTFail("missing send") }
        bridge.cancelPendingPaste()
        XCTAssertTrue(bridge.isClipboardBusy, "focus loss does not cancel an explicit non-pasting send")
        acceptClipboard(bridge, requestID: sendID, operation: "send")
        XCTAssertFalse(bridge.isClipboardBusy)

        var received: [String] = []
        bridge.onRemoteClipboardText = { received.append($0) }
        XCTAssertTrue(bridge.requestRemoteClipboard())
        guard case .requestClipboard(_, let receiveID) = requests.last else { return XCTFail("missing receive") }
        bridge.cancelPendingPaste()
        XCTAssertTrue(bridge.isClipboardBusy, "focus loss does not cancel an explicit receive")
        acceptClipboard(bridge, requestID: receiveID, text: "received after focus loss")
        XCTAssertEqual(received, ["received after focus loss"])
        XCTAssertEqual(keyCount, 0)
    }

    func testSameTargetDisplayRetryReusesAlreadyReceivedConfiguration() {
        let bridge = connectedBridge()
        bridge.featureRequestForTesting = { _ in true }
        acceptDisplays(bridge, selected: 0)
        bridge.acceptVideoConfigurationForTesting(sequenceHeader, version: 7)
        XCTAssertTrue(bridge.selectDisplay(1))
        bridge.acceptVideoConfigurationForTesting(sequenceHeader, version: 8)
        XCTAssertEqual(bridge.videoConfigVersionForTesting, 8)
        bridge.expireFeatureRequestsForTesting()
        XCTAssertTrue(bridge.selectDisplay(1))
        XCTAssertEqual(bridge.minimumDisplayConfigVersionForTesting, 8, "a no-op host retry will not produce version 9")
        XCTAssertEqual(bridge.videoConfigVersionForTesting, 8, "the matching configuration stays available")
        acceptDisplays(bridge, selected: 1)
        bridge.confirmDisplayFrameForTesting()
        XCTAssertTrue(bridge.canSendRemoteInput)
    }

    func testReturningToOriginalDisplayCanReuseItsConfirmedConfigurationWithoutAcceptingIntermediateScreen() {
        let bridge = connectedBridge()
        bridge.featureRequestForTesting = { _ in true }
        acceptDisplays(bridge, selected: 0)
        bridge.acceptVideoConfigurationForTesting(sequenceHeader, version: 7)
        XCTAssertTrue(bridge.selectDisplay(1))
        bridge.acceptVideoConfigurationForTesting(sequenceHeader, version: 8)
        bridge.expireFeatureRequestsForTesting()
        XCTAssertTrue(bridge.selectDisplay(0))
        XCTAssertEqual(bridge.minimumDisplayConfigVersionForTesting, 9)
        acceptDisplays(bridge, selected: 0)
        XCTAssertEqual(bridge.videoConfigVersionForTesting, 7, "the host may already be on the original screen and emit no new config")
        let droppedFrames = bridge.metrics.droppedFrames
        bridge.acceptVideoConfigurationForTesting(sequenceHeader, version: 8)
        XCTAssertEqual(bridge.videoConfigVersionForTesting, 7, "an intermediate version belongs to the abandoned screen")
        XCTAssertGreaterThan(bridge.metrics.droppedFrames, droppedFrames)
        bridge.confirmDisplayFrameForTesting()
        XCTAssertTrue(bridge.canSendRemoteInput)
        bridge.acceptVideoConfigurationForTesting(sequenceHeader, version: 8)
        XCTAssertEqual(bridge.videoConfigVersionForTesting, 7, "abandoned-screen configs remain rejected after the input gate opens")
        bridge.acceptVideoConfigurationForTesting(sequenceHeader, version: 9)
        XCTAssertEqual(bridge.videoConfigVersionForTesting, 9)
        XCTAssertNil(bridge.minimumDisplayConfigVersionForTesting)
    }

    private var sequenceHeader: Data {
        Data([0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50,
              0x05, 0xbb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00,
              0x00, 0x03, 0x03, 0x20, 0xf1, 0x22, 0x6a,
              0, 0, 1, 0x68, 0xee, 0x3c, 0x80])
    }

    private func connectedBridge() -> ControllerBridge {
        let bridge = ControllerBridge.testing()
        bridge.acceptStateForTesting(streamID: 4)
        return bridge
    }

    private func acceptDisplays(_ bridge: ControllerBridge, selected: UInt32, streamID: UInt64 = 4) {
        sendJSON(bridge, event: Int(DESKLINK_EVENT_SESSION.rawValue), streamID: streamID, value: ["kind": "display_list", "selected_display_id": selected,
            "displays": [["id": 0, "width": 1920, "height": 1080, "is_primary": true],
                         ["id": 1, "width": 2560, "height": 1440, "is_primary": false]]])
    }

    private func acceptQuality(_ bridge: ControllerBridge, preference: String, preset: String, streamID: UInt64 = 4) {
        sendJSON(bridge, event: Int(DESKLINK_EVENT_SESSION.rawValue), streamID: streamID, value: ["kind": "video_quality_state",
            "quality_preference": preference, "applied_preset": preset])
    }

    private func acceptClipboard(_ bridge: ControllerBridge, requestID: UInt64, operation: String = "request",
                                 status: String = "completed", result: String = "completed", streamID: UInt64 = 4,
                                 text: String? = nil, generation: UInt64? = nil) {
        var value: [String: Any] = ["request_id": requestID, "operation": operation, "status": status, "result": result]
        if let text { value["text"] = text }
        sendJSON(bridge, event: Int(DESKLINK_EVENT_CLIPBOARD.rawValue), streamID: streamID, value: value, generation: generation)
    }

    private func sendJSON(_ bridge: ControllerBridge, event: Int, streamID: UInt64, value: [String: Any], generation: UInt64? = nil) {
        let data = try! JSONSerialization.data(withJSONObject: value)
        bridge.acceptEventForTesting(eventKind: event, stateValue: 0, streamID: streamID,
            message: String(decoding: data, as: UTF8.self), generation: generation)
    }
}
