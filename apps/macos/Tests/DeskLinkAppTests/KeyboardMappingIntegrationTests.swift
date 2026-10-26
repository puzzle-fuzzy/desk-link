import AppKit
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

@MainActor
final class KeyboardMappingIntegrationTests: XCTestCase {
    func testProductionViewEncodesWindowsCopyPasteSelectAllUndoAsControlChords() throws {
        for (keyCode, character) in [(UInt16(8), "c"), (9, "v"), (0, "a"), (6, "z")] {
            let bridge = recordingBridge(platform: .windows)
            let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
            view.keyDown(with: try event(type: .keyDown, keyCode: keyCode, character: character))
            view.keyUp(with: try event(type: .keyUp, keyCode: keyCode, character: character))
            XCTAssertEqual(bridge.sentInputsForTesting.map(\.character), Array(repeating: character.unicodeScalars.first!.value, count: 2))
            XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0])
            XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers), [2, 2])
        }
    }

    func testRawAndUnconfirmedPlatformsPreserveMetaInActualFFIInput() throws {
        for (platform, mode) in [(RemotePlatform.unknown, ModifierMode.automatic), (.macos, .automatic), (.ios, .automatic), (.windows, .raw)] {
            let bridge = recordingBridge(platform: platform)
            let view = SessionInputView.InputView(bridge: bridge, videoSize: nil, modifierMode: mode)
            view.keyDown(with: try event(type: .keyDown, keyCode: 8, character: "c"))
            view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
            XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers), [8, 8])
        }
    }

    func testKeyEquivalentUsesSamePolicyAndSendsBalancedChord() throws {
        _ = NSApplication.shared
        let bridge = recordingBridge(platform: .windows)
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 100), styleMask: .borderless, backing: .buffered, defer: false)
        window.contentView = view
        XCTAssertTrue(window.makeFirstResponder(view))
        XCTAssertTrue(view.performKeyEquivalent(with: try event(type: .keyDown, keyCode: 8, character: "c")))
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers), [2, 2])
    }

    func testPolicyChangeReleasesHeldInputBeforeNewMapping() throws {
        let bridge = recordingBridge(platform: .windows)
        let view = SessionInputView.InputView(bridge: bridge, videoSize: nil)
        view.keyDown(with: try event(type: .keyDown, keyCode: 8, character: "c"))
        let before = bridge.releaseAllCallCountForTesting
        view.synchronizeKeyboardPolicy(KeyboardMappingPolicy(mode: .raw, remotePlatform: .windows))
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, before + 1)
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        view.keyDown(with: try event(type: .keyDown, keyCode: 8, character: "c"))
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers), [2, 8, 8])
    }

    private func recordingBridge(platform: RemotePlatform) -> ControllerBridge {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 1))
        bridge.acceptCapabilitiesForTesting(platform: platform)
        bridge.recordInputsForTesting = true
        return bridge
    }

    private func event(type: NSEvent.EventType, keyCode: UInt16, character: String) throws -> NSEvent {
        try XCTUnwrap(NSEvent.keyEvent(
            with: type, location: .zero, modifierFlags: .command, timestamp: 0,
            windowNumber: 0, context: nil, characters: "\u{3}", charactersIgnoringModifiers: character,
            isARepeat: false, keyCode: keyCode
        ))
    }
}
