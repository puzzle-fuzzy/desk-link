import DeskLinkAppleCore
import XCTest
@testable import DeskLinkIOS

final class IOSInputMapperTests: XCTestCase {
    func testDirectTouchMapsOnlyInsideAspectFitVideoRect() {
        let mapper = IOSTouchMapper(
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: CGRect(x: 0, y: 0, width: 390, height: 844),
            mode: .direct
        )

        XCTAssertNil(mapper.command(for: CGPoint(x: 0, y: 0), phase: .moved))
        XCTAssertEqual(
            mapper.command(for: CGPoint(x: 195, y: 422), phase: .moved),
            .move(normalizedX: 0.5, normalizedY: 0.5)
        )
    }

    func testTrackpadTwoFingerPanBecomesBoundedWheelInput() {
        XCTAssertEqual(
            IOSTouchMapper.wheel(deltaX: 0, deltaY: -1_201),
            .wheel(deltaX: 0, deltaY: -1_200)
        )
    }
}

@MainActor
final class IOSKeyboardInputTests: XCTestCase {
    func testKeyboardResignReleasesPressedRemoteKeys() {
        let input = TestableIOSKeyboardInput()
        input.sendSpecialKey(.control, pressed: true)
        input.resign()

        XCTAssertEqual(input.releaseAllCallCount, 1)
    }
}
