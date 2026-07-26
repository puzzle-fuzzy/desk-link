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

    func testDirectTouchKeepsUIKitTopLeftVerticalCoordinate() {
        let videoRect = CGRect(x: 0, y: 312, width: 390, height: 220)
        let mapper = IOSTouchMapper(
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: CGRect(x: 0, y: 0, width: 390, height: 844),
            mode: .direct,
            visibleVideoRect: videoRect
        )

        guard case let .move(topX, topY) = mapper.command(
            for: CGPoint(x: 195, y: videoRect.minY + 10),
            phase: .began
        ),
        case let .move(bottomX, bottomY) = mapper.command(
            for: CGPoint(x: 195, y: videoRect.maxY - 10),
            phase: .began
        ) else {
            return XCTFail("视频区域内的直接触控应该生成鼠标位置")
        }

        XCTAssertEqual(topX, 0.5, accuracy: 0.000_001)
        XCTAssertEqual(topY, Float(10.0 / 220.0), accuracy: 0.000_001)
        XCTAssertEqual(bottomX, 0.5, accuracy: 0.000_001)
        XCTAssertEqual(bottomY, Float(210.0 / 220.0), accuracy: 0.000_001)
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

        guard case let .move(normalizedX, normalizedY) = mapper.relativeCommand(
            delta: CGSize(width: 39, height: 26)
        ) else {
            return XCTFail("轨迹板移动应该生成相对鼠标位置")
        }

        XCTAssertEqual(normalizedX, Float(0.5 + 39 / 1920), accuracy: 0.000_001)
        XCTAssertEqual(normalizedY, Float(0.5 + 26 / 1080), accuracy: 0.000_001)
    }

    func testTrackpadPointerCommandUsesPersistentPosition() {
        var mapper = IOSTouchMapper(
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: CGRect(x: 0, y: 0, width: 390, height: 844),
            mode: .trackpad
        )

        _ = mapper.relativeCommand(delta: CGSize(width: 39, height: 0))

        guard case let .move(normalizedX, normalizedY) = mapper.currentPointerCommand else {
            return XCTFail("轨迹板应该保留当前鼠标位置")
        }

        XCTAssertEqual(normalizedX, Float(0.5 + 39 / 1920), accuracy: 0.000_001)
        XCTAssertEqual(normalizedY, 0.5, accuracy: 0.000_001)
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

    func testVideoViewportKeepsUIKitPinchAnchorInPlace() {
        let bounds = CGSize(width: 390, height: 844)
        let touchAnchor = CGPoint(x: 195, y: 360)

        var viewport = IOSVideoViewport()
        viewport.pinch(
            factor: 2,
            anchor: touchAnchor,
            videoSize: CGSize(width: 1920, height: 1080),
            bounds: bounds
        )

        // The UIKit touch point must stay attached to the same visual content
        // after zooming, without a second vertical coordinate conversion.
        let baseRect = VideoGeometry.aspectFit(
            source: CGSize(width: 1920, height: 1080),
            in: CGRect(origin: .zero, size: bounds)
        )
        XCTAssertTrue(viewport.renderRect(baseRect: baseRect).contains(touchAnchor))
        XCTAssertGreaterThan(viewport.panOffset.height, 0)
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
