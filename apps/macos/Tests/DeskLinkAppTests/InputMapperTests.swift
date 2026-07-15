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
            keyCode: 0x24,
            characters: "中",
            modifiers: [.command, .shift],
            isDown: true
        ), [
            .key(code: 0x24, pressed: true, modifiers: [.meta, .shift]),
            .unicode("中", modifiers: [.meta, .shift]),
        ])
    }

    func testKeyboardMapperDoesNotSendUnicodeForKeyUp() {
        XCTAssertEqual(KeyboardMapper.map(
            keyCode: 0x24,
            characters: "中",
            modifiers: [.command],
            isDown: false
        ), [.key(code: 0x24, pressed: false, modifiers: [.meta])])
    }
}
