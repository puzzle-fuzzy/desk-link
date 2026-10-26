import CoreGraphics
import AppKit
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

final class InputMapperTests: XCTestCase {
    private let inputContext = CapturedInputContext(streamID: 7, inputEpoch: 11, configVersion: 2, runtimeGeneration: 3)
    func testNormalisedPointUsesOnlyVisibleVideoRect() {
        let mapper = InputMapper(videoRect: CGRect(x: 100, y: 50, width: 800, height: 450))

        XCTAssertEqual(
            mapper.normalizedPoint(for: CGPoint(x: 500, y: 275)),
            CGPoint(x: 0.5, y: 0.5)
        )
        XCTAssertEqual(
            mapper.normalizedPoint(for: CGPoint(x: 100, y: 50)),
            CGPoint(x: 0, y: 1)
        )
        XCTAssertEqual(
            mapper.normalizedPoint(for: CGPoint(x: 900, y: 500)),
            CGPoint(x: 1, y: 0)
        )
        XCTAssertNil(mapper.normalizedPoint(for: CGPoint(x: 50, y: 275)))
    }

    func testCaptureVideoDimensionsFitProtocolBounds() {
        XCTAssertEqual(
            captureVideoDimensions(displayWidth: 3840, displayHeight: 2160),
            CaptureVideoDimensions(width: 2560, height: 1440)
        )
        XCTAssertEqual(
            captureVideoDimensions(displayWidth: 2560, displayHeight: 1600),
            CaptureVideoDimensions(width: 2304, height: 1440)
        )
    }

    func testH264TargetBitrateScalesWithVideoArea() {
        XCTAssertEqual(h264TargetBitrate(width: 1920, height: 1080), 10_368_000)
        XCTAssertEqual(h264TargetBitrate(width: 2560, height: 1440), 18_432_000)
        XCTAssertEqual(h264TargetBitrate(width: 64, height: 64), 2_000_000)
        XCTAssertEqual(h264TargetBitrate(width: 8_000, height: 8_000), 20_000_000)
    }

    func testCommandMapsToRemoteControlWhenAutomaticMappingIsEnabled() {
        let mapper = InputMapper(videoRect: .zero, modifierMode: .automatic, remotePlatform: .windows)

        XCTAssertEqual(mapper.remoteModifier(for: .command), .control)
    }

    func testKeyboardMapperPreservesCharacterAndModifierFlags() {
        XCTAssertEqual(KeyboardMapper.map(
            keyCode: 0x00,
            characters: "中",
            modifiers: [.command, .shift],
            isDown: true
        ), [
            .character(0x4e2d, pressed: true, modifiers: [.meta, .shift]),
        ])
    }

    func testKeyboardMapperDoesNotSendUnicodeForKeyUp() {
        XCTAssertEqual(KeyboardMapper.map(
            keyCode: 0x24,
            characters: "中",
            modifiers: [.command],
            isDown: false
        ), [.key(code: 1, pressed: false, modifiers: [.meta])])
    }

    func testKeyboardMapperSendsOrdinaryASCIIAsProtocolCharacter() {
        XCTAssertEqual(
            KeyboardMapper.map(keyCode: 0, characters: "a", modifiers: [], isDown: true),
            [.character(97, pressed: true, modifiers: [])]
        )
    }

    func testKeyboardMapperUsesLogicalCodesForSpecialKeysAndIgnoresCapsLockBit() {
        XCTAssertEqual(
            KeyboardMapper.map(keyCode: 0x7b, characters: nil, modifiers: [.capsLock], isDown: true),
            [.key(code: 7, pressed: true, modifiers: [])]
        )
    }

    func testProtocolSpecialKeyCodesRoundTripToMacVirtualKeys() {
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 1), 0x24)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 8), 0x7c)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 9), 0x75)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 11), 0x73)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 16), 0x7a)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 27), 0x6f)
        XCTAssertEqual(MacKeyCodeMapper.protocolCode(forAppKitKeyCode: 0x74), 13)
        XCTAssertEqual(MacKeyCodeMapper.protocolCode(forAppKitKeyCode: 0x6d), 25)
        XCTAssertNil(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 0))
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 28), 0x3b)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 29), 0x3a)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 30), 0x38)
        XCTAssertEqual(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 31), 0x37)
        XCTAssertNil(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 32))
    }

    func testCharacterReleaseUsesOriginalUppercaseAfterShiftIsReleased() {
        var keyboard = KeyboardInputState()
        XCTAssertEqual(keyboard.keyDown(context: inputContext, keyCode: 0, characters: "A", modifiers: .shift, isRepeat: false).map(\.command), [
            .character(65, pressed: true, modifiers: .shift),
        ])
        _ = keyboard.flagsChanged(context: inputContext, []).map(\.command)
        XCTAssertEqual(keyboard.keyUp(keyCode: 0).map(\.command), [.character(65, pressed: false, modifiers: .shift)])
        XCTAssertTrue(keyboard.keyUp(keyCode: 0).map(\.command).isEmpty)
    }

    func testRepeatGlyphChangeReleasesOldIdentityBeforePressingNewIdentity() {
        var keyboard = KeyboardInputState()
        let down: [RemoteInputCommand] = [.character(97, pressed: true, modifiers: [])]
        XCTAssertEqual(keyboard.keyDown(context: inputContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: false).map(\.command), down)
        XCTAssertEqual(keyboard.keyDown(context: inputContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: true).map(\.command), down)
        XCTAssertEqual(keyboard.keyDown(context: inputContext, keyCode: 0, characters: "A", modifiers: .shift, isRepeat: true).map(\.command), [
            .character(97, pressed: false, modifiers: []),
            .character(65, pressed: true, modifiers: .shift),
        ])
        XCTAssertTrue(keyboard.keyDown(context: inputContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: false).map(\.command).isEmpty)
        XCTAssertEqual(keyboard.keyUp(keyCode: 0).map(\.command), [.character(65, pressed: false, modifiers: .shift)])
    }

    func testCapsLockOnlyTogglesForItsPhysicalFlagsEvent() {
        var keyboard = KeyboardInputState()
        // Local Caps Lock was already on before focus. Pressing Shift must not
        // guess the host's lock state or toggle it as a side effect.
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, [.capsLock, .shift], keyCode: 0x38).map(\.command), [
            .key(code: 30, pressed: true, modifiers: []),
        ])
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, .capsLock, keyCode: 0x38).map(\.command), [
            .key(code: 30, pressed: false, modifiers: []),
        ])
        let toggle: [RemoteInputCommand] = [
            .key(code: 15, pressed: true, modifiers: []), .key(code: 15, pressed: false, modifiers: []),
        ]
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, [], keyCode: 0x39).map(\.command), toggle)
        XCTAssertTrue(keyboard.flagsChanged(context: inputContext, [], keyCode: 0x39).map(\.command).isEmpty)
        keyboard.reset()
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, [], keyCode: 0x39).map(\.command), toggle)
    }

    func testFlagsChangedEmitsOnlyModifierDifferencesAndKeepsDragModifier() {
        var keyboard = KeyboardInputState()
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, .control).map(\.command), [.key(code: 28, pressed: true, modifiers: [])])
        XCTAssertTrue(keyboard.flagsChanged(context: inputContext, .control).map(\.command).isEmpty)
        _ = keyboard.keyDown(context: inputContext, keyCode: 8, characters: "c", modifiers: .control, isRepeat: false).map(\.command)
        XCTAssertEqual(keyboard.keyUp(keyCode: 8).map(\.command), [
            .character(99, pressed: false, modifiers: .control),
            .key(code: 28, pressed: true, modifiers: []),
        ])
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, []).map(\.command), [.key(code: 28, pressed: false, modifiers: [])])
    }

    func testResetDoesNotReleaseOldKeysIntoNewSession() {
        var keyboard = KeyboardInputState()
        _ = keyboard.flagsChanged(context: inputContext, .shift).map(\.command)
        _ = keyboard.keyDown(context: inputContext, keyCode: 0, characters: "A", modifiers: .shift, isRepeat: false).map(\.command)
        keyboard.reset()
        XCTAssertTrue(keyboard.keyUp(keyCode: 0).map(\.command).isEmpty)
        XCTAssertTrue(keyboard.flagsChanged(context: inputContext, []).map(\.command).isEmpty)
    }

    func testAutomaticMappingRequiresAuthenticatedWindowsPlatform() {
        XCTAssertEqual(KeyboardMappingPolicy(remotePlatform: .windows).modifiers(for: .command), .control)
        for platform in [RemotePlatform.unknown, .macos, .ios] {
            XCTAssertEqual(KeyboardMappingPolicy(remotePlatform: platform).modifiers(for: .command), .meta)
            XCTAssertEqual(InputMapper(videoRect: .zero, remotePlatform: platform).remoteModifier(for: .command), .command)
        }
        XCTAssertEqual(KeyboardMappingPolicy(mode: .raw, remotePlatform: .windows).modifiers(for: .command), .meta)
    }

    func testMappedCommandAndLocalControlShareOneBalancedRemoteModifier() {
        var keyboard = KeyboardInputState()
        let policy = KeyboardMappingPolicy(remotePlatform: .windows)
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, .command, policy: policy).map(\.command), [.key(code: 28, pressed: true, modifiers: [])])
        XCTAssertTrue(keyboard.flagsChanged(context: inputContext, [.command, .control], policy: policy).map(\.command).isEmpty)
        XCTAssertTrue(keyboard.flagsChanged(context: inputContext, .control, policy: policy).map(\.command).isEmpty)
        XCTAssertEqual(keyboard.flagsChanged(context: inputContext, [], policy: policy).map(\.command), [.key(code: 28, pressed: false, modifiers: [])])
    }

    func testCopyPasteSelectAllUndoUseMappedModifiersInPhysicalState() {
        for (keyCode, text) in [(UInt16(8), "c"), (9, "v"), (0, "a"), (6, "z")] {
            var keyboard = KeyboardInputState()
            let policy = KeyboardMappingPolicy(remotePlatform: .windows)
            let scalar = text.unicodeScalars.first!.value
            XCTAssertEqual(keyboard.keyDown(context: inputContext, keyCode: keyCode, characters: text, modifiers: .command, isRepeat: false, policy: policy).map(\.command), [
                .character(scalar, pressed: true, modifiers: .control),
            ])
            XCTAssertEqual(keyboard.keyUp(keyCode: keyCode).map(\.command), [.character(scalar, pressed: false, modifiers: .control)])
        }
    }

    func testKeyReleaseAndModifierRestoreKeepTheirOriginalPresentedContext() {
        var keyboard = KeyboardInputState()
        let original = inputContext
        _ = keyboard.flagsChanged(context: original, .control)
        let down = keyboard.keyDown(context: original, keyCode: 8, characters: "c", modifiers: .control, isRepeat: false)
        XCTAssertTrue(down.allSatisfy { $0.context == original })
        let up = keyboard.keyUp(keyCode: 8)
        XCTAssertEqual(up.map(\.command), [
            .character(99, pressed: false, modifiers: .control),
            .key(code: 28, pressed: true, modifiers: []),
        ])
        XCTAssertTrue(up.allSatisfy { $0.context == original })
        let newContext = CapturedInputContext(streamID: 7, inputEpoch: 12, configVersion: 3, runtimeGeneration: 3)
        // Even if the flags-up arrives after a new frame, release the old press.
        XCTAssertEqual(keyboard.flagsChanged(context: newContext, []).map(\.context), [original])
    }

    func testPresentationBarrierDoesNotRecreateHeldRepeatOnNewEpoch() {
        var keyboard = KeyboardInputState()
        _ = keyboard.keyDown(context: inputContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: false)
        let newContext = CapturedInputContext(streamID: 7, inputEpoch: 12, configVersion: 3, runtimeGeneration: 3)
        XCTAssertTrue(keyboard.keyDown(context: newContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: true).isEmpty)
        XCTAssertEqual(keyboard.keyUp(keyCode: 0).map(\.context), [inputContext])
        keyboard.reset()
        XCTAssertTrue(keyboard.keyDown(context: newContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: true).isEmpty)
        XCTAssertEqual(keyboard.keyDown(context: newContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: false).map(\.context), [newContext])
    }

    func testGlyphTransitionKeepsOneCapturedEpochForItsReleaseAndNewDown() {
        var keyboard = KeyboardInputState()
        _ = keyboard.keyDown(context: inputContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: false)
        let transition = keyboard.keyDown(context: inputContext, keyCode: 0, characters: "A", modifiers: .shift, isRepeat: true)
        XCTAssertEqual(transition.map(\.command), [.character(97, pressed: false, modifiers: []), .character(65, pressed: true, modifiers: .shift)])
        XCTAssertEqual(transition.map(\.context), [inputContext, inputContext])
    }

    func testOtherModifierFlagsDoNotResurrectAnOldHeldKeyAfterBarrier() {
        var keyboard = KeyboardInputState()
        _ = keyboard.flagsChanged(context: inputContext, .shift, keyCode: 0x38)
        keyboard.reset()
        let next = CapturedInputContext(streamID: 7, inputEpoch: 12, configVersion: 3, runtimeGeneration: 3)
        // Control was released while Shift was still held across the barrier.
        XCTAssertTrue(keyboard.flagsChanged(context: next, .shift, keyCode: 0x3b).isEmpty)
        XCTAssertTrue(keyboard.flagsChanged(context: next, [], keyCode: 0x38).isEmpty)
        XCTAssertEqual(keyboard.flagsChanged(context: next, .shift, keyCode: 0x38).map(\.context), [next])
    }

    func testQualityReconfigurationDoesNotRelabelHeldRepeatOrGlyphTransition() {
        var keyboard = KeyboardInputState()
        let quality = CapturedInputContext(streamID: 7, inputEpoch: 11, configVersion: 3, runtimeGeneration: 3)
        _ = keyboard.keyDown(context: inputContext, keyCode: 0, characters: "a", modifiers: [], isRepeat: false)
        let repeated = keyboard.keyDown(context: quality, keyCode: 0, characters: "a", modifiers: [], isRepeat: true)
        XCTAssertEqual(repeated.map(\.context), [inputContext])
        let glyph = keyboard.keyDown(context: quality, keyCode: 0, characters: "A", modifiers: .shift, isRepeat: true)
        XCTAssertEqual(glyph.map(\.context), [inputContext, inputContext])
        XCTAssertEqual(keyboard.keyUp(keyCode: 0).map(\.context), [inputContext])
    }

    func testModifierRestoreAfterQualityChangeKeepsItsOwnOriginalContext() {
        var keyboard = KeyboardInputState()
        let quality = CapturedInputContext(streamID: 7, inputEpoch: 11, configVersion: 3, runtimeGeneration: 3)
        _ = keyboard.flagsChanged(context: inputContext, .control, keyCode: 0x3b)
        _ = keyboard.keyDown(context: quality, keyCode: 8, characters: "c", modifiers: .control, isRepeat: false)
        XCTAssertEqual(keyboard.keyUp(keyCode: 8).map(\.context), [quality, inputContext])
        XCTAssertEqual(keyboard.flagsChanged(context: quality, [], keyCode: 0x3b).map(\.context), [inputContext])
    }

    func testScrollQuantizationNeverSendsInvalidZeroOrOversizedDeltas() {
        XCTAssertEqual(quantizeScrollDelta(0), 0)
        XCTAssertEqual(quantizeScrollDelta(0.25), 1)
        XCTAssertEqual(quantizeScrollDelta(-0.25), -1)
        XCTAssertEqual(quantizeScrollDelta(2_000), 1_200)
    }
}
