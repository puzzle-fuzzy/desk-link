import AppKit
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

// These verify the AppKit callback and FFI encoding contracts. Real input
// methods and candidate windows still require the manual Mac acceptance run.
@MainActor
final class NativeTextInputTests: XCTestCase {
    func testMarkedTextUsesUTF16SelectionWithoutSendingPinyin() {
        let bridge = readyBridge()
        bridge.recordInputsForTesting = true
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.setMarkedText("😀a", selectedRange: NSRange(location: 2, length: 1), replacementRange: noRange)
        XCTAssertTrue(view.hasMarkedText())
        XCTAssertEqual(view.markedRange(), NSRange(location: 0, length: 3))
        XCTAssertEqual(view.selectedRange(), NSRange(location: 2, length: 1))
        var actual = noRange
        XCTAssertEqual(view.attributedSubstring(forProposedRange: NSRange(location: 0, length: 2), actualRange: &actual)?.string, "😀")
        XCTAssertEqual(actual, NSRange(location: 0, length: 2))
        XCTAssertTrue(bridge.sentInputsForTesting.isEmpty)
    }

    func testIMECommitSendsOnlyFinalTextAndConsumesPinyinKeyUp() throws {
        let bridge = readyBridge()
        bridge.recordInputsForTesting = true
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.setMarkedText("zhong", selectedRange: NSRange(location: 5, length: 0), replacementRange: noRange)
        view.insertText(NSAttributedString(string: "中😀"), replacementRange: noRange)
        let up = try XCTUnwrap(NSEvent.keyEvent(
            with: .keyUp, location: .zero, modifierFlags: [], timestamp: 0,
            windowNumber: 0, context: nil, characters: "z", charactersIgnoringModifiers: "z",
            isARepeat: false, keyCode: 6
        ))
        view.keyUp(with: up)
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.character), [0x4e2d, 0x4e2d, 0x1f600, 0x1f600])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0, 1, 0])
        XCTAssertFalse(view.hasMarkedText())
    }

    func testCancelCandidateDoesNotForwardEscapeOrSpace() {
        let bridge = readyBridge()
        bridge.recordInputsForTesting = true
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.setMarkedText("zhong", selectedRange: NSRange(location: 5, length: 0), replacementRange: noRange)
        view.doCommand(by: #selector(NSResponder.cancelOperation(_:)))
        XCTAssertFalse(view.hasMarkedText())
        XCTAssertTrue(bridge.sentInputsForTesting.isEmpty)
    }

    func testReleaseAllDiscardsCompositionWithoutCommit() {
        let bridge = readyBridge()
        bridge.recordInputsForTesting = true
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.setMarkedText("pin", selectedRange: NSRange(location: 3, length: 0), replacementRange: noRange)
        view.releaseAll()
        XCTAssertFalse(view.hasMarkedText())
        XCTAssertTrue(bridge.sentInputsForTesting.isEmpty)
    }

    func testUnmarkAcceptsDeadKeyCommitOnce() {
        let bridge = readyBridge()
        bridge.recordInputsForTesting = true
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.setMarkedText("é", selectedRange: NSRange(location: 1, length: 0), replacementRange: noRange)
        view.unmarkText()
        view.unmarkText()
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.character), [0xe9, 0xe9])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0])
    }

    func testPresentationBarrierDiscardsOldCompositionInsteadOfRetaggingCommit() {
        let bridge = readyBridge()
        bridge.recordInputsForTesting = true
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.setMarkedText("zhong", selectedRange: NSRange(location: 5, length: 0), replacementRange: noRange)
        let next = CapturedInputContext(streamID: 1, inputEpoch: 8, configVersion: 3, runtimeGeneration: 3)
        bridge.acceptPresentedInputContextForTesting(next)
        view.synchronizeInputContext(next)
        view.insertText("中", replacementRange: noRange)
        XCTAssertTrue(bridge.sentInputsForTesting.isEmpty)
        XCTAssertFalse(view.hasMarkedText())
    }

    func testSameTargetQualityReconfigurationKeepsMarkedTextAndDoesNotRelease() {
        let bridge = readyBridge()
        bridge.recordInputsForTesting = true
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.setMarkedText("zhong", selectedRange: NSRange(location: 5, length: 0), replacementRange: noRange)
        let quality = CapturedInputContext(streamID: 1, inputEpoch: 7, configVersion: 3, runtimeGeneration: 3)
        bridge.acceptPresentedInputContextForTesting(quality)
        view.synchronizeInputContext(quality)
        view.synchronizeSession(.recovering)
        view.synchronizeSession(.connected(streamID: 1))
        XCTAssertTrue(view.hasMarkedText())
        XCTAssertTrue(bridge.releasedContextsForTesting.isEmpty)
        view.insertText("中", replacementRange: noRange)
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.input_epoch), [7, 7])
    }

    private func readyBridge() -> ControllerBridge {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 1))
        // Metadata fixture only; this does not represent a displayed Metal frame.
        bridge.acceptPresentedInputContextForTesting(CapturedInputContext(streamID: 1, inputEpoch: 7, configVersion: 2, runtimeGeneration: 3))
        return bridge
    }

    private var noRange: NSRange { NSRange(location: NSNotFound, length: 0) }
}
