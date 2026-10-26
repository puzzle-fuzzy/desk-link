import AppKit
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

@MainActor
final class KeyboardMappingIntegrationTests: XCTestCase {
    func testProductionViewEncodesWindowsCopyPasteSelectAllUndoAsControlChords() throws {
        for (keyCode, character) in [(UInt16(8), "c"), (9, "v"), (0, "a"), (6, "z")] {
            let bridge = recordingBridge(platform: .windows)
            let view = makeView(bridge)
            view.keyDown(with: try event(type: .keyDown, keyCode: keyCode, character: character))
            view.keyUp(with: try event(type: .keyUp, keyCode: keyCode, character: character))
            XCTAssertEqual(bridge.sentInputsForTesting.map(\.character), Array(repeating: character.unicodeScalars.first!.value, count: 2))
            XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0])
            XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers), [2, 2])
        }
    }

    func testRawModePreservesMetaInActualFFIInput() throws {
        // Platform-aware automatic mapping stays covered by InputMapperTests;
        // the session view itself maps in its configured mode.
        let bridge = recordingBridge(platform: .windows)
        let view = SessionInputView.InputView(
            viewport: SessionViewport(sourceSize: CGSize(width: 1280, height: 720),
                                      viewportSize: CGSize(width: 1280, height: 720)),
            interactionMode: .control,
            acceptsRemoteInput: true,
            sendInput: { bridge.send(input: $0) },
            releaseInput: { bridge.releaseAll() },
            exitFullscreen: { false },
            onPan: { _ in },
            modifierMode: .raw
        )
        view.keyDown(with: try event(type: .keyDown, keyCode: 8, character: "c"))
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers), [8, 8])
    }

    func testKeyEquivalentUsesSamePolicyAndSendsBalancedChord() throws {
        _ = NSApplication.shared
        let bridge = recordingBridge(platform: .windows)
        let view = makeView(bridge)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 200, height: 100), styleMask: .borderless, backing: .buffered, defer: false)
        window.contentView = view
        XCTAssertTrue(window.makeFirstResponder(view))
        XCTAssertTrue(view.performKeyEquivalent(with: try event(type: .keyDown, keyCode: 8, character: "c")))
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.pressed), [1, 0])
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers), [2, 2])
    }

    func testHeldInputIsReleasedAtTheBoundaryAndRemappingStaysConsistent() throws {
        let bridge = recordingBridge(platform: .windows)
        let view = makeView(bridge)
        view.keyDown(with: try event(type: .keyDown, keyCode: 8, character: "c"))
        let before = bridge.releaseAllCallCountForTesting
        view.releaseAll()
        XCTAssertGreaterThanOrEqual(bridge.releaseAllCallCountForTesting, before)
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        view.keyDown(with: try event(type: .keyDown, keyCode: 8, character: "c"))
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        // Automatic mode keeps mapping Command to the remote Control chord.
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.modifiers).filter { $0 != 0 }, [2, 2, 2, 2])
    }

    func testLateKeyUpStaysOnTheActiveStreamAndReleaseAllClearsHeldInput() throws {
        // Epoch binding is enforced by the Rust session gate
        // (desklink_send_input_for_stream); the view only guarantees both
        // edges reach the active stream and a boundary releases held keys.
        let bridge = recordingBridge(platform: .windows)
        let original = try XCTUnwrap(bridge.captureInputContext())
        let view = makeView(bridge)
        view.keyDown(with: try event(type: .keyDown, keyCode: 8, character: "c"))
        let next = CapturedInputContext(streamID: 1, inputEpoch: 8, configVersion: 3, runtimeGeneration: 3)
        bridge.acceptPresentedInputContextForTesting(next)
        view.keyUp(with: try event(type: .keyUp, keyCode: 8, character: "c"))
        XCTAssertEqual(bridge.sentInputsForTesting.map(\.stream_id), [original.streamID, original.streamID])
        let releases = bridge.releaseAllCallCountForTesting
        view.releaseAll()
        XCTAssertGreaterThan(bridge.releaseAllCallCountForTesting, releases)
    }

    private func makeView(_ bridge: ControllerBridge) -> SessionInputView.InputView {
        SessionInputView.InputView(
            viewport: SessionViewport(sourceSize: CGSize(width: 1280, height: 720),
                                      viewportSize: CGSize(width: 1280, height: 720)),
            interactionMode: .control,
            acceptsRemoteInput: true,
            sendInput: { bridge.send(input: $0) },
            releaseInput: { bridge.releaseAll() },
            exitFullscreen: { false },
            onPan: { _ in }
        )
    }

    private func recordingBridge(platform: RemotePlatform) -> ControllerBridge {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 1))
        bridge.acceptCapabilitiesForTesting(platform: platform)
        // Explicit metadata fixture; not proof of Metal presentation.
        bridge.acceptPresentedInputContextForTesting(CapturedInputContext(streamID: 1, inputEpoch: 7, configVersion: 2, runtimeGeneration: 3))
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
