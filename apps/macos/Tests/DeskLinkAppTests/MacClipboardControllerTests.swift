import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

@MainActor
final class MacClipboardControllerTests: XCTestCase {
    func testClipboardIsReadOnlyOnExplicitSendAndKeepsPasteIntent() {
        let clipboard = MemoryClipboard()
        let session = MemoryClipboardSession()
        let controller = MacClipboardController(clipboard: clipboard)
        controller.attach(to: session)
        XCTAssertEqual(clipboard.reads, 0)
        controller.sendLocalText(paste: true)
        XCTAssertEqual(clipboard.reads, 1)
        XCTAssertEqual(session.sentText, clipboard.text)
        XCTAssertTrue(session.paste)
        XCTAssertEqual(clipboard.writes, 0)
    }

    func testOnlyCurrentBindingCanWriteRemoteTextLocally() {
        let clipboard = MemoryClipboard()
        let first = MemoryClipboardSession()
        let second = MemoryClipboardSession()
        let controller = MacClipboardController(clipboard: clipboard)
        controller.attach(to: first)
        let lateResponse = first.onRemoteClipboardText
        controller.attach(to: second)
        lateResponse?("retired session")
        XCTAssertEqual(clipboard.writes, 0)
        controller.requestRemoteText()
        XCTAssertEqual(second.requests, 1)
        XCTAssertEqual(clipboard.reads, 0)
        second.onRemoteClipboardText?("中文\nsecond line 😀")
        XCTAssertEqual(clipboard.text, "中文\nsecond line 😀")
        XCTAssertEqual(clipboard.writes, 1)
        let afterDetach = second.onRemoteClipboardText
        controller.detach()
        afterDetach?("late")
        XCTAssertEqual(clipboard.writes, 1)
    }

    func testBusySessionDoesNotReadPrivateClipboardAndOversizeIsNotSent() {
        let clipboard = MemoryClipboard()
        let session = MemoryClipboardSession()
        let controller = MacClipboardController(clipboard: clipboard)
        controller.attach(to: session)
        session.isClipboardBusy = true
        controller.sendLocalText()
        XCTAssertEqual(clipboard.reads, 0)
        XCTAssertNil(session.sentText)
        session.isClipboardBusy = false
        clipboard.text = String(repeating: "中", count: 17_000)
        controller.sendLocalText()
        XCTAssertNil(session.sentText)
        XCTAssertNotNil(controller.localError)
    }

    func testWriteFailureIsVisibleAndDoesNotClaimSuccess() {
        let clipboard = MemoryClipboard()
        clipboard.failWrite = true
        let session = MemoryClipboardSession()
        let controller = MacClipboardController(clipboard: clipboard)
        controller.attach(to: session)
        controller.requestRemoteText()
        session.onRemoteClipboardText?("new text")
        XCTAssertNotNil(controller.localError)
        XCTAssertNil(controller.localStatus)
        XCTAssertEqual(clipboard.writes, 0)
    }
}

@MainActor
private final class MemoryClipboard: TextClipboard {
    var text = "本机剪贴板\n😀"
    var reads = 0
    var writes = 0
    var failWrite = false
    func readText() throws -> String { reads += 1; return text }
    func writeText(_ text: String) throws {
        if failWrite { throw TextClipboardError.writeFailed }
        writes += 1
        self.text = text
    }
}

@MainActor
private final class MemoryClipboardSession: RemoteClipboardSession {
    var canSendRemoteInput = true
    var isClipboardBusy = false
    var onRemoteClipboardText: ((String) -> Void)?
    var sentText: String?
    var paste = false
    var requests = 0
    func sendClipboardText(_ text: String, paste: Bool) -> Bool {
        sentText = text
        self.paste = paste
        return true
    }
    func requestRemoteClipboard() -> Bool { requests += 1; return true }
}
