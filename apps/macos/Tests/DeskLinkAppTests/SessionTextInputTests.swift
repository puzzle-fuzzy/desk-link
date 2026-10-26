import AppKit
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

/// AppKit events are delivered only to an isolated view and protocol recorder.
/// These tests never post keyboard events or write the system clipboard.
@MainActor
final class SessionTextInputTests: XCTestCase {
    private let absent = NSRange(location: NSNotFound, length: 0)

    func testLatinAndCompositionCommitsShareOnePathWithoutFallbackEcho() {
        let fixture = Fixture { _, client in
            client.insertText("a", replacementRange: NSRange(location: NSNotFound, length: 0))
            return false // A handler that inserted text must still suppress fallback.
        }
        fixture.view.keyDown(with: key(.keyDown))
        fixture.view.keyUp(with: key(.keyUp))
        XCTAssertEqual(fixture.sent, [.unicode("a", modifiers: [])])
    }

    func testChineseCandidateConfirmationDoesNotLeakReturnOrPhysicalSpelling() {
        let fixture = Fixture { event, client in
            if event.keyCode == 0x24 {
                client.insertText("你好", replacementRange: NSRange(location: NSNotFound, length: 0))
                client.doCommand(by: #selector(NSResponder.insertNewline(_:)))
            } else {
                client.setMarkedText("nihao", selectedRange: NSRange(location: 5, length: 0),
                                     replacementRange: NSRange(location: NSNotFound, length: 0))
            }
            return true
        }
        fixture.view.keyDown(with: key(.keyDown, characters: "n"))
        fixture.view.keyUp(with: key(.keyUp, characters: "n"))
        XCTAssertTrue(fixture.view.hasMarkedText())
        XCTAssertEqual(fixture.view.markedRange(), NSRange(location: 0, length: 5))
        XCTAssertTrue(fixture.sent.isEmpty)
        fixture.view.keyDown(with: key(.keyDown, code: 0x24, characters: "\r"))
        fixture.view.keyUp(with: key(.keyUp, code: 0x24, characters: "\r"))
        XCTAssertEqual(fixture.sent, [.unicode("你好", modifiers: [])])
        XCTAssertFalse(fixture.view.hasMarkedText())
    }

    func testChineseEmojiAndMultilineAttributedCommitIsOneCommand() throws {
        let fixture = Fixture()
        let client = try XCTUnwrap(fixture.view.inputContext).client
        client.setMarkedText(NSAttributedString(string: "zhong"), selectedRange: NSRange(location: 5, length: 0), replacementRange: absent)
        client.insertText(NSAttributedString(string: "中文👨‍👩‍👧‍👦\n第二行"), replacementRange: absent)
        XCTAssertEqual(fixture.sent, [.unicode("中文👨‍👩‍👧‍👦\n第二行", modifiers: [])])
        XCTAssertFalse(client.hasMarkedText())
        XCTAssertEqual(client.markedRange().location, NSNotFound)
    }

    func testCandidateBackspaceAndEscapeStayLocal() throws {
        let fixture = Fixture { event, client in
            if event.keyCode == 0x33 {
                client.setMarkedText("n", selectedRange: NSRange(location: 1, length: 0),
                                     replacementRange: NSRange(location: NSNotFound, length: 0))
                client.doCommand(by: #selector(NSResponder.deleteBackward(_:)))
            } else if event.keyCode == 0x35 {
                client.doCommand(by: #selector(NSResponder.cancelOperation(_:)))
            }
            return true
        }
        let client = try XCTUnwrap(fixture.view.inputContext).client
        client.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: absent)
        fixture.view.keyDown(with: key(.keyDown, code: 0x33, characters: "\u{7f}"))
        fixture.view.keyUp(with: key(.keyUp, code: 0x33, characters: "\u{7f}"))
        XCTAssertEqual(fixture.view.markedRange().length, 1)
        fixture.view.keyDown(with: key(.keyDown, code: 0x35, characters: "\u{1b}"))
        fixture.view.keyUp(with: key(.keyUp, code: 0x35, characters: "\u{1b}"))
        XCTAssertTrue(fixture.sent.isEmpty)
        XCTAssertFalse(fixture.view.hasMarkedText())
    }

    func testFullscreenEscapeCancelsCompositionAndConsumesBothEdges() throws {
        let fixture = Fixture()
        fixture.view.exitFullscreen = { true }
        try XCTUnwrap(fixture.view.inputContext).client.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: absent)
        fixture.view.keyDown(with: key(.keyDown, code: 0x35, characters: "\u{1b}"))
        fixture.view.keyUp(with: key(.keyUp, code: 0x35, characters: "\u{1b}"))
        XCTAssertTrue(fixture.sent.isEmpty)
        XCTAssertFalse(fixture.view.hasMarkedText())
        XCTAssertEqual(fixture.releases, 1)
    }

    func testOldInputMethodCannotCommitAfterFocusLossAndNewFocus() throws {
        let fixture = Fixture()
        let oldClient = try XCTUnwrap(fixture.view.inputContext).client
        oldClient.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: absent)
        _ = fixture.view.resignFirstResponder()
        _ = fixture.view.becomeFirstResponder()
        oldClient.insertText("旧文本", replacementRange: absent)
        oldClient.setMarkedText("stale", selectedRange: NSRange(location: 0, length: 0), replacementRange: absent)
        XCTAssertTrue(fixture.sent.isEmpty)
        XCTAssertFalse(fixture.view.hasMarkedText())
        try XCTUnwrap(fixture.view.inputContext).client.insertText("新文本", replacementRange: absent)
        XCTAssertEqual(fixture.sent, [.unicode("新文本", modifiers: [])])
    }

    func testStreamChangeReconnectAndBrowseInvalidatePendingComposition() throws {
        let fixture = Fixture()
        let first = try XCTUnwrap(fixture.view.inputContext).client
        first.setMarkedText("ni", selectedRange: NSRange(location: 2, length: 0), replacementRange: absent)
        fixture.view.inputContextID = 7
        fixture.view.keyDown(with: key(.keyDown)) // creates a new input lifetime
        first.insertText("旧屏幕", replacementRange: absent)
        XCTAssertFalse(fixture.sent.contains(.unicode("旧屏幕", modifiers: [])))

        let second = try XCTUnwrap(fixture.view.inputContext).client
        fixture.view.acceptsRemoteInput = false
        fixture.view.acceptsRemoteInput = true
        fixture.view.keyDown(with: key(.keyDown))
        second.insertText("重连前", replacementRange: absent)
        XCTAssertFalse(fixture.sent.contains(.unicode("重连前", modifiers: [])))

        let third = try XCTUnwrap(fixture.view.inputContext).client
        fixture.view.interactionMode = .browse
        fixture.view.interactionMode = .control
        fixture.view.keyDown(with: key(.keyDown))
        third.insertText("浏览前", replacementRange: absent)
        XCTAssertFalse(fixture.sent.contains(.unicode("浏览前", modifiers: [])))
    }

    func testStandaloneModifiersMapCommandControlAndKeepOverlappingKeysHeld() {
        let fixture = Fixture()
        fixture.view.flagsChanged(with: key(.flagsChanged, code: 0x37, modifiers: [.command]))
        fixture.view.flagsChanged(with: key(.flagsChanged, code: 0x3b, modifiers: [.command, .control]))
        fixture.view.flagsChanged(with: key(.flagsChanged, code: 0x37, modifiers: [.control]))
        fixture.view.flagsChanged(with: key(.flagsChanged, code: 0x3b, modifiers: []))
        XCTAssertEqual(fixture.sent, [
            .key(code: DeskLinkKeyCode.control, pressed: true, modifiers: []),
            .key(code: DeskLinkKeyCode.control, pressed: false, modifiers: []),
        ])
    }

    func testRawCommandOptionShiftAndControlSendIndependentKeyEdges() {
        let fixture = Fixture()
        fixture.view.modifierMode = .raw
        _ = fixture.view.becomeFirstResponder()
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: [.command, .option, .shift, .control]))
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: []))
        XCTAssertEqual(fixture.sent.count, 8)
        for code in [DeskLinkKeyCode.control, DeskLinkKeyCode.alt, DeskLinkKeyCode.shift, DeskLinkKeyCode.meta] {
            XCTAssertTrue(fixture.sent.contains { if case let .key(actual, true, _) = $0 { return actual == code }; return false })
            XCTAssertTrue(fixture.sent.contains { if case let .key(actual, false, _) = $0 { return actual == code }; return false })
        }
    }

    func testReleasingOneModifierDoesNotReleaseOtherHeldModifiers() {
        let fixture = Fixture()
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: [.control, .shift]))
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: [.shift]))
        XCTAssertEqual(fixture.sent, [
            .key(code: DeskLinkKeyCode.control, pressed: true, modifiers: []),
            .key(code: DeskLinkKeyCode.shift, pressed: true, modifiers: []),
            .key(code: DeskLinkKeyCode.control, pressed: false, modifiers: []),
        ])
    }

    func testOptionLetterUsesAltBaseCharacterInBothPolicies() {
        let fixture = Fixture()
        fixture.view.keyDown(with: key(.keyDown, code: 3, characters: "ƒ", base: "f", modifiers: [.option]))
        XCTAssertEqual(fixture.sent, [.unicode("f", modifiers: [.option])])
        fixture.view.modifierMode = .raw
        fixture.view.keyDown(with: key(.keyDown, code: 3, characters: "ƒ", base: "f", modifiers: [.option]))
        XCTAssertEqual(fixture.sent.last, .unicode("f", modifiers: [.option]))
    }

    func testContextChangesCancelPendingPasteButTemporaryInputGateDoesNot() {
        let fixture = Fixture()
        var cancellations = 0
        fixture.view.onFocusLost = { cancellations += 1 }
        fixture.view.acceptsRemoteInput = false
        fixture.view.acceptsRemoteInput = true
        XCTAssertEqual(cancellations, 0, "A pending paste temporarily closes the gate and must not cancel itself.")
        _ = fixture.view.resignFirstResponder()
        XCTAssertEqual(cancellations, 1)
        fixture.view.interactionMode = .browse
        fixture.view.modifierMode = .raw
        fixture.view.inputContextID = 42
        XCTAssertEqual(cancellations, 4)
    }

    func testShortcutRestoresHeldModifierAfterAtomicCharacterTap() {
        let fixture = Fixture()
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: [.command]))
        fixture.view.keyDown(with: key(.keyDown, code: 8, characters: "c", modifiers: [.command]))
        XCTAssertEqual(fixture.sent, [
            .key(code: DeskLinkKeyCode.control, pressed: true, modifiers: []),
            .key(code: DeskLinkKeyCode.control, pressed: false, modifiers: []),
            .unicode("c", modifiers: [.control]),
            .key(code: DeskLinkKeyCode.control, pressed: true, modifiers: []),
        ])
    }

    func testControlArrowUpPreservesControlBeforeControlMouseDrag() {
        let fixture = Fixture()
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: [.control]))
        fixture.view.keyDown(with: key(.keyDown, code: 0x7b, characters: "", modifiers: [.control]))
        fixture.view.keyUp(with: key(.keyUp, code: 0x7b, characters: "", modifiers: [.control]))
        XCTAssertEqual(fixture.sent, [
            .key(code: DeskLinkKeyCode.control, pressed: true, modifiers: []),
            .key(code: DeskLinkKeyCode.arrowLeft, pressed: true, modifiers: [.control]),
            .key(code: DeskLinkKeyCode.arrowLeft, pressed: false, modifiers: []),
        ])
        let mouse = NSEvent.mouseEvent(with: .leftMouseDown, location: CGPoint(x: 100, y: 100),
                                       modifierFlags: [.control], timestamp: 0, windowNumber: 0,
                                       context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
        fixture.view.mouseDown(with: mouse)
        XCTAssertEqual(fixture.sent.suffix(2), [
            .move(normalizedX: 0.125, normalizedY: 5.0 / 6.0),
            .mouseButton(.left, pressed: true),
        ])
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: []))
        XCTAssertEqual(fixture.sent.last, .key(code: DeskLinkKeyCode.control, pressed: false, modifiers: []))
    }

    func testRepeatedOptionTabKeepsAltHeldUntilPhysicalOptionRelease() {
        let fixture = Fixture()
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: [.option]))
        for _ in 0..<2 {
            fixture.view.keyDown(with: key(.keyDown, code: 0x30, characters: "\t", modifiers: [.option]))
            fixture.view.keyUp(with: key(.keyUp, code: 0x30, characters: "\t", modifiers: [.option]))
        }
        XCTAssertEqual(fixture.sent, [
            .key(code: DeskLinkKeyCode.alt, pressed: true, modifiers: []),
            .key(code: DeskLinkKeyCode.tab, pressed: true, modifiers: [.option]),
            .key(code: DeskLinkKeyCode.tab, pressed: false, modifiers: []),
            .key(code: DeskLinkKeyCode.tab, pressed: true, modifiers: [.option]),
            .key(code: DeskLinkKeyCode.tab, pressed: false, modifiers: []),
        ])
        fixture.view.flagsChanged(with: key(.flagsChanged, modifiers: []))
        XCTAssertEqual(fixture.sent.last, .key(code: DeskLinkKeyCode.alt, pressed: false, modifiers: []))
    }

    func testShortcutsUseBaseCharactersAndAutomaticOrRawPolicy() {
        let fixture = Fixture()
        fixture.view.keyDown(with: key(.keyDown, code: 8, characters: "c", modifiers: [.command]))
        fixture.view.keyUp(with: key(.keyUp, code: 8, characters: "c", modifiers: [.command]))
        fixture.view.keyDown(with: key(.keyDown, characters: "\u{1}", base: "a", modifiers: [.control]))
        XCTAssertEqual(fixture.sent, [.unicode("c", modifiers: [.control]), .unicode("a", modifiers: [.control])])
        fixture.view.modifierMode = .raw
        fixture.view.keyDown(with: key(.keyDown, code: 37, characters: "l", modifiers: [.command]))
        XCTAssertEqual(fixture.sent.last, .unicode("l", modifiers: [.meta]))
    }

    func testPasteRequestsClipboardAcknowledgementWithoutSendingPasteKeys() {
        let fixture = Fixture()
        var pasteRequests = 0
        fixture.view.onPasteClipboard = { pasteRequests += 1 }
        fixture.view.keyDown(with: key(.keyDown, code: 9, characters: "v", modifiers: [.command]))
        fixture.view.keyUp(with: key(.keyUp, code: 9, characters: "v", modifiers: [.command]))
        fixture.view.keyDown(with: key(.keyDown, code: 9, characters: "\u{16}", base: "v", modifiers: [.control]))
        fixture.view.keyUp(with: key(.keyUp, code: 9, characters: "\u{16}", base: "v", modifiers: [.control]))
        XCTAssertEqual(pasteRequests, 2)
        XCTAssertTrue(fixture.sent.isEmpty)
    }

    func testFocusedSessionConsumesCommandMenuEquivalent() {
        _ = NSApplication.shared
        let fixture = Fixture()
        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 400, height: 300),
                              styleMask: [.titled], backing: .buffered, defer: true)
        window.contentView = fixture.view
        XCTAssertTrue(window.makeFirstResponder(fixture.view))
        XCTAssertTrue(fixture.view.performKeyEquivalent(with: key(.keyDown, code: 13, characters: "w", modifiers: [.command])))
        XCTAssertEqual(fixture.sent, [.unicode("w", modifiers: [.control])])
        fixture.view.interactionMode = .browse
        XCTAssertFalse(fixture.view.performKeyEquivalent(with: key(.keyDown, code: 13, characters: "w", modifiers: [.command])))
        window.contentView = nil
    }

    func testSpecialNavigationKeepsBothEdgesOutsideComposition() {
        let fixture = Fixture { _, client in
            client.doCommand(by: #selector(NSResponder.moveLeft(_:)))
            return true
        }
        fixture.view.keyDown(with: key(.keyDown, code: 0x7b, characters: ""))
        fixture.view.keyUp(with: key(.keyUp, code: 0x7b, characters: ""))
        XCTAssertEqual(fixture.sent, [
            .key(code: DeskLinkKeyCode.arrowLeft, pressed: true, modifiers: []),
            .key(code: DeskLinkKeyCode.arrowLeft, pressed: false, modifiers: []),
        ])
    }

    func testMarkedSubstringUsesUTF16RangesAndCandidateRectHasScreenGeometry() throws {
        let fixture = Fixture()
        let client = try XCTUnwrap(fixture.view.inputContext).client
        client.setMarkedText("中文😀", selectedRange: NSRange(location: 99, length: 99), replacementRange: absent)
        XCTAssertEqual(client.markedRange(), NSRange(location: 0, length: 4))
        XCTAssertEqual(client.selectedRange(), NSRange(location: 4, length: 0))
        var actual = absent
        XCTAssertEqual(client.attributedSubstring(forProposedRange: NSRange(location: 0, length: 2), actualRange: &actual)?.string, "中文")
        XCTAssertEqual(actual, NSRange(location: 0, length: 2))
        let candidateRect = client.firstRect(forCharacterRange: absent, actualRange: &actual)
        XCTAssertGreaterThan(candidateRect.height, 0)
        XCTAssertTrue(candidateRect.origin.x.isFinite)
    }

    private func key(_ type: NSEvent.EventType, code: UInt16 = 0, characters: String = "a",
                     base: String? = nil, modifiers: NSEvent.ModifierFlags = []) -> NSEvent {
        NSEvent.keyEvent(with: type, location: .zero, modifierFlags: modifiers, timestamp: 0,
                        windowNumber: 0, context: nil, characters: characters,
                        charactersIgnoringModifiers: base ?? characters,
                        isARepeat: false, keyCode: code)!
    }

    @MainActor private final class Fixture {
        var sent: [RemoteInputCommand] = []
        var releases = 0
        var view: SessionInputView.InputView!

        init(handler: @escaping (NSEvent, any NSTextInputClient) -> Bool = { _, _ in false }) {
            _ = NSApplication.shared
            let viewport = SessionViewport(sourceSize: CGSize(width: 800, height: 600),
                                           viewportSize: CGSize(width: 800, height: 600))
            view = SessionInputView.InputView(viewport: viewport, interactionMode: .control,
                                             acceptsRemoteInput: true,
                                             sendInput: { [weak self] in self?.sent.append($0) },
                                             releaseInput: { [weak self] in self?.releases += 1 },
                                             exitFullscreen: { false }, onPan: { _ in },
                                             makeTextInputContext: { TestInputContext(client: $0, handler: handler) })
            view.frame = CGRect(origin: .zero, size: viewport.viewportSize)
        }
    }

    @MainActor private final class TestInputContext: NSTextInputContext {
        // AppKit's override is nonisolated in the legacy SDK. Tests call it
        // synchronously on the UI thread, and this immutable closure is never
        // read from a worker or transferred to another task.
        nonisolated(unsafe) private let handler: (NSEvent, any NSTextInputClient) -> Bool

        init(client: any NSTextInputClient, handler: @escaping (NSEvent, any NSTextInputClient) -> Bool) {
            self.handler = handler
            super.init(client: client)
        }

        override func handleEvent(_ event: NSEvent) -> Bool {
            precondition(Thread.isMainThread)
            return handler(event, client)
        }
        override func discardMarkedText() {}
        override func deactivate() {}
    }
}
