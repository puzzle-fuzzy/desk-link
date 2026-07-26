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

    func testFourFingerPanInvertsVerticalViewportDirection() {
        XCTAssertEqual(
            IOSTouchMapper.fourFingerPanDelta(CGSize(width: 12, height: 34)),
            CGSize(width: 12, height: -34)
        )
    }

    func testTrackpadMovementDoesNotUseTouchDownPosition() {
        var mapper = IOSTouchMapper(
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: CGRect(x: 0, y: 0, width: 390, height: 844),
            mode: .trackpad
        )

        XCTAssertEqual(
            mapper.relativeCommand(delta: CGSize(width: 39, height: 0)),
            .move(normalizedX: 0.6, normalizedY: 0.5)
        )
    }

    func testTrackpadPointerCommandUsesPersistentPosition() {
        var mapper = IOSTouchMapper(
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: CGRect(x: 0, y: 0, width: 390, height: 844),
            mode: .trackpad
        )

        _ = mapper.relativeCommand(delta: CGSize(width: 39, height: 0))

        XCTAssertEqual(
            mapper.currentPointerCommand,
            .move(normalizedX: 0.6, normalizedY: 0.5)
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

    func testViewportMovesToKeepTrackpadPointerInsideSafeEdge() {
        var viewport = IOSVideoViewport()
        let bounds = CGSize(width: 390, height: 844)
        viewport.pinch(
            factor: 2,
            anchor: CGPoint(x: 195, y: 422),
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: bounds
        )

        viewport.keepPointerVisible(
            normalizedPosition: CGPoint(x: 1, y: 0.5),
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: bounds,
            edgeInset: 48
        )

        let baseRect = VideoGeometry.aspectFit(
            source: CGSize(width: 1920, height: 1080),
            in: CGRect(origin: .zero, size: bounds)
        )
        XCTAssertLessThan(viewport.panOffset.width, 0)
        XCTAssertTrue(
            viewport.renderRect(baseRect: baseRect)
                .intersects(CGRect(origin: .zero, size: bounds))
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

    func testReturnTextIsHandledAsEnterInsteadOfUnicodeNewline() {
        XCTAssertTrue(IOSKeyboardInput.isReturnText("\n"))
        XCTAssertTrue(IOSKeyboardInput.isReturnText("\r"))
        XCTAssertTrue(IOSKeyboardInput.isReturnText("\r\n"))
        XCTAssertFalse(IOSKeyboardInput.isReturnText("a"))
    }
}
