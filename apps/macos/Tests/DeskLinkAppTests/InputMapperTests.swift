import CoreGraphics
import AppKit
import XCTest
@testable import DeskLinkApp

final class InputMapperTests: XCTestCase {
    func testNormalisedPointUsesOnlyVisibleVideoRect() {
        let mapper = InputMapper(videoRect: CGRect(x: 100, y: 50, width: 800, height: 450))

        XCTAssertEqual(
            mapper.normalizedPoint(for: CGPoint(x: 500, y: 275)),
            CGPoint(x: 0.5, y: 0.5)
        )
        XCTAssertNil(mapper.normalizedPoint(for: CGPoint(x: 50, y: 275)))
    }

    func testCommandMapsToRemoteControlWhenAutomaticMappingIsEnabled() {
        let mapper = InputMapper(videoRect: .zero, modifierMode: .automatic)

        XCTAssertEqual(mapper.remoteModifier(for: .command), .control)
    }

    func testKeyboardMapperPreservesUnicodeAndModifierFlags() {
        XCTAssertEqual(KeyboardMapper.map(
            keyCode: 0x00,
            characters: "中",
            modifiers: [.command, .shift],
            isDown: true
        ), [
            .unicode("中", modifiers: [.meta, .shift]),
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
            [.unicode("a", modifiers: [])]
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
        XCTAssertNil(MacKeyCodeMapper.appKitKeyCode(forProtocolCode: 0))
    }

    func testScrollQuantizationNeverSendsInvalidZeroOrOversizedDeltas() {
        XCTAssertEqual(quantizeScrollDelta(0), 0)
        XCTAssertEqual(quantizeScrollDelta(0.25), 1)
        XCTAssertEqual(quantizeScrollDelta(-0.25), -1)
        XCTAssertEqual(quantizeScrollDelta(2_000), 1_200)
    }
}
