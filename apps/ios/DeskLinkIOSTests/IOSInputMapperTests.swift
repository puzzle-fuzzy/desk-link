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

    func testVideoViewportZoomsAroundPinchAnchorAndClampsBackToFit() {
        var viewport = IOSVideoViewport()
        let bounds = CGSize(width: 390, height: 844)
        let anchor = CGPoint(x: 195, y: 422)

        viewport.pinch(
            factor: 2,
            anchor: anchor,
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: bounds
        )

        let baseRect = VideoGeometry.aspectFit(
            source: CGSize(width: 1920, height: 1080),
            in: CGRect(origin: .zero, size: bounds)
        )
        XCTAssertEqual(viewport.zoomScale, 2)
        XCTAssertTrue(viewport.renderRect(baseRect: baseRect).contains(anchor))

        viewport.pinch(
            factor: 0.1,
            anchor: anchor,
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: bounds
        )
        XCTAssertEqual(viewport.zoomScale, 1)
        XCTAssertEqual(viewport.panOffset, .zero)
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

    func testReturnTextIsHandledAsEnterInsteadOfUnicodeNewline() {
        XCTAssertTrue(IOSKeyboardInput.isReturnText("\n"))
        XCTAssertTrue(IOSKeyboardInput.isReturnText("\r"))
        XCTAssertTrue(IOSKeyboardInput.isReturnText("\r\n"))
        XCTAssertFalse(IOSKeyboardInput.isReturnText("a"))
    }
}
