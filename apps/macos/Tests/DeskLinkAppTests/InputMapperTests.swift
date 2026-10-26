import CoreGraphics
import AppKit
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

final class InputMapperTests: XCTestCase {
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
        XCTAssertEqual(keyboard.keyDown(keyCode: 0, characters: "A", modifiers: .shift, isRepeat: false), [
            .character(65, pressed: true, modifiers: .shift),
        ])
        _ = keyboard.flagsChanged([])
        XCTAssertEqual(keyboard.keyUp(keyCode: 0), [.character(65, pressed: false, modifiers: .shift)])
        XCTAssertTrue(keyboard.keyUp(keyCode: 0).isEmpty)
    }

    func testRepeatGlyphChangeReleasesOldIdentityBeforePressingNewIdentity() {
        var keyboard = KeyboardInputState()
        let down: [RemoteInputCommand] = [.character(97, pressed: true, modifiers: [])]
        XCTAssertEqual(keyboard.keyDown(keyCode: 0, characters: "a", modifiers: [], isRepeat: false), down)
        XCTAssertEqual(keyboard.keyDown(keyCode: 0, characters: "a", modifiers: [], isRepeat: true), down)
        XCTAssertEqual(keyboard.keyDown(keyCode: 0, characters: "A", modifiers: .shift, isRepeat: true), [
            .character(97, pressed: false, modifiers: []),
            .character(65, pressed: true, modifiers: .shift),
        ])
        XCTAssertTrue(keyboard.keyDown(keyCode: 0, characters: "a", modifiers: [], isRepeat: false).isEmpty)
        XCTAssertEqual(keyboard.keyUp(keyCode: 0), [.character(65, pressed: false, modifiers: .shift)])
    }

    func testCapsLockOnlyTogglesForItsPhysicalFlagsEvent() {
        var keyboard = KeyboardInputState()
        // Local Caps Lock was already on before focus. Pressing Shift must not
        // guess the host's lock state or toggle it as a side effect.
        XCTAssertEqual(keyboard.flagsChanged([.capsLock, .shift], keyCode: 0x38), [
            .key(code: 30, pressed: true, modifiers: []),
        ])
        XCTAssertEqual(keyboard.flagsChanged(.capsLock, keyCode: 0x38), [
            .key(code: 30, pressed: false, modifiers: []),
        ])
        let toggle: [RemoteInputCommand] = [
            .key(code: 15, pressed: true, modifiers: []), .key(code: 15, pressed: false, modifiers: []),
        ]
        XCTAssertEqual(keyboard.flagsChanged([], keyCode: 0x39), toggle)
        XCTAssertTrue(keyboard.flagsChanged([], keyCode: 0x39).isEmpty)
        keyboard.reset()
        XCTAssertEqual(keyboard.flagsChanged([], keyCode: 0x39), toggle)
    }

    func testFlagsChangedEmitsOnlyModifierDifferencesAndKeepsDragModifier() {
        var keyboard = KeyboardInputState()
        XCTAssertEqual(keyboard.flagsChanged(.control), [.key(code: 28, pressed: true, modifiers: [])])
        XCTAssertTrue(keyboard.flagsChanged(.control).isEmpty)
        _ = keyboard.keyDown(keyCode: 8, characters: "c", modifiers: .control, isRepeat: false)
        XCTAssertEqual(keyboard.keyUp(keyCode: 8), [
            .character(99, pressed: false, modifiers: .control),
            .key(code: 28, pressed: true, modifiers: []),
        ])
        XCTAssertEqual(keyboard.flagsChanged([]), [.key(code: 28, pressed: false, modifiers: [])])
    }

    func testResetDoesNotReleaseOldKeysIntoNewSession() {
        var keyboard = KeyboardInputState()
        _ = keyboard.flagsChanged(.shift)
        _ = keyboard.keyDown(keyCode: 0, characters: "A", modifiers: .shift, isRepeat: false)
        keyboard.reset()
        XCTAssertTrue(keyboard.keyUp(keyCode: 0).isEmpty)
        XCTAssertTrue(keyboard.flagsChanged([]).isEmpty)
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
        XCTAssertEqual(keyboard.flagsChanged(.command, policy: policy), [.key(code: 28, pressed: true, modifiers: [])])
        XCTAssertTrue(keyboard.flagsChanged([.command, .control], policy: policy).isEmpty)
        XCTAssertTrue(keyboard.flagsChanged(.control, policy: policy).isEmpty)
        XCTAssertEqual(keyboard.flagsChanged([], policy: policy), [.key(code: 28, pressed: false, modifiers: [])])
    }

    func testCopyPasteSelectAllUndoUseMappedModifiersInPhysicalState() {
        for (keyCode, text) in [(UInt16(8), "c"), (9, "v"), (0, "a"), (6, "z")] {
            var keyboard = KeyboardInputState()
            let policy = KeyboardMappingPolicy(remotePlatform: .windows)
            let scalar = text.unicodeScalars.first!.value
            XCTAssertEqual(keyboard.keyDown(keyCode: keyCode, characters: text, modifiers: .command, isRepeat: false, policy: policy), [
                .character(scalar, pressed: true, modifiers: .control),
            ])
            XCTAssertEqual(keyboard.keyUp(keyCode: keyCode), [.character(scalar, pressed: false, modifiers: .control)])
        }
    }

    func testScrollQuantizationNeverSendsInvalidZeroOrOversizedDeltas() {
        XCTAssertEqual(quantizeScrollDelta(0), 0)
        XCTAssertEqual(quantizeScrollDelta(0.25), 1)
        XCTAssertEqual(quantizeScrollDelta(-0.25), -1)
        XCTAssertEqual(quantizeScrollDelta(2_000), 1_200)
    }
}
