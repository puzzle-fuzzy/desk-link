import CoreGraphics
import XCTest
@testable import DeskLinkApp

final class MacInputInjectorTests: XCTestCase {
    func testInputInjectorConvertsNormalizedCoordinatesAndUnicode() throws {
        let backend = RecordingCGEventBackend()
        let injector = MacInputInjector(
            backend: backend,
            displayFrame: CGRect(x: 100, y: 200, width: 800, height: 600)
        )

        try injector.inject(.move(normalizedX: 0.25, normalizedY: 0.5))
        try injector.inject(.unicode("中", modifiers: [.shift]))

        XCTAssertEqual(backend.moves, [CGPoint(x: 300, y: 500)])
        XCTAssertEqual(backend.unicodeEvents, ["中"])
        XCTAssertEqual(backend.unicodeModifiers, [.shift])
    }

    func testInputInjectorKeepsNormalizedMaximumInsideDisplayBounds() throws {
        let backend = RecordingCGEventBackend()
        let injector = MacInputInjector(
            backend: backend,
            displayFrame: CGRect(x: 100, y: 200, width: 800, height: 600)
        )

        try injector.inject(.move(normalizedX: 1, normalizedY: 1))

        XCTAssertEqual(backend.moves.count, 1)
        XCTAssertLessThan(backend.moves[0].x, 900)
        XCTAssertLessThan(backend.moves[0].y, 800)
    }

    func testInputInjectorReleaseAllClearsEveryPressedKeyAndButton() throws {
        let backend = RecordingCGEventBackend()
        let injector = MacInputInjector(backend: backend)
        try injector.inject(.key(code: 0x24, pressed: true, modifiers: []))
        try injector.inject(.mouseButton(.left, pressed: true))

        injector.releaseAll()

        XCTAssertEqual(backend.releasedKeys, [0x24])
        XCTAssertEqual(backend.releasedButtons, [.left])
        XCTAssertTrue(injector.pressedInputs.isEmpty)
    }

    func testInputInjectorRetainsInputsWhoseReleaseFails() throws {
        let backend = RecordingCGEventBackend()
        backend.failReleases = true
        let injector = MacInputInjector(backend: backend)
        try injector.inject(.key(code: 0x24, pressed: true, modifiers: []))
        try injector.inject(.mouseButton(.left, pressed: true))

        let failures = injector.releaseAll()

        XCTAssertEqual(failures.count, 2)
        XCTAssertEqual(injector.pressedInputs, [.key(0x24), .button(.left)])
    }
}

private final class RecordingCGEventBackend: CGEventBackend {
    var failReleases = false
    var moves: [CGPoint] = []
    var unicodeEvents: [String] = []
    var unicodeModifiers: [Modifiers] = []
    var releasedKeys: [UInt32] = []
    var releasedButtons: [MouseButton] = []

    func moveMouse(to point: CGPoint) throws { moves.append(point) }

    func postMouseButton(_ button: MouseButton, pressed: Bool, at point: CGPoint) throws {
        if !pressed, failReleases { throw MacInputInjectorError.eventCreationFailed }
        if !pressed { releasedButtons.append(button) }
    }

    func postScroll(deltaX: Int32, deltaY: Int32) throws {}

    func postKey(code: UInt32, pressed: Bool, modifiers: Modifiers) throws {
        if !pressed, failReleases { throw MacInputInjectorError.eventCreationFailed }
        if !pressed { releasedKeys.append(code) }
    }

    func postUnicode(_ text: String, modifiers: Modifiers) throws {
        unicodeEvents.append(text)
        unicodeModifiers.append(modifiers)
    }
}
