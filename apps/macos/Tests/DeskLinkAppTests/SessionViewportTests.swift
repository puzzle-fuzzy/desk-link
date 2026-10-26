import AppKit
import CoreGraphics
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

final class SessionViewportTests: XCTestCase {
    func testFitUsesOneTopLeftRectangleForCursorAndInput() {
        let viewport = SessionViewport(sourceSize: CGSize(width: 1920, height: 1080),
                                       viewportSize: CGSize(width: 800, height: 600))
        XCTAssertEqual(viewport.videoRect, CGRect(x: 0, y: 75, width: 800, height: 450))
        XCTAssertNil(viewport.normalizedPoint(for: CGPoint(x: 400, y: 40)))
        let remotePoint = CGPoint(x: 0.25, y: 0.2)
        let cursorPoint = viewport.cursorPoint(normalized: remotePoint)
        XCTAssertEqual(cursorPoint, CGPoint(x: 200, y: 165))
        XCTAssertEqual(viewport.normalizedPoint(for: cursorPoint), remotePoint)
        XCTAssertEqual(viewport.drawableVideoRect(drawableSize: CGSize(width: 1600, height: 1200)),
                       CGRect(x: 0, y: 150, width: 1600, height: 900))
    }

    func testActualSizeMeansOneSourcePixelPerRetinaPhysicalPixel() {
        let viewport = SessionViewport(sourceSize: CGSize(width: 3840, height: 2160),
                                       viewportSize: CGSize(width: 1200, height: 700),
                                       backingScale: 2, scalingMode: .actualSize)
        XCTAssertEqual(viewport.videoRect, CGRect(x: -360, y: -190, width: 1920, height: 1080))
        let drawableRect = viewport.drawableVideoRect(drawableSize: CGSize(width: 2400, height: 1400))
        XCTAssertEqual(drawableRect.size, CGSize(width: 3840, height: 2160))
        XCTAssertEqual(drawableRect.origin, CGPoint(x: -720, y: -380))
        let visiblePoint = CGPoint(x: 123, y: 456)
        let normalized = viewport.normalizedPoint(for: visiblePoint)!
        XCTAssertEqual(viewport.cursorPoint(normalized: normalized).x, visiblePoint.x, accuracy: 0.0001)
        XCTAssertEqual(viewport.cursorPoint(normalized: normalized).y, visiblePoint.y, accuracy: 0.0001)
        XCTAssertNil(viewport.normalizedPoint(for: CGPoint(x: -10, y: 200)))
    }

    func testPanningClampsAtEveryEdgeAndDoesNotExposeEmptySpace() {
        let viewport = SessionViewport(sourceSize: CGSize(width: 2000, height: 1200),
                                       viewportSize: CGSize(width: 800, height: 600),
                                       backingScale: 1, scalingMode: .actualSize,
                                       panOffset: CGSize(width: 9999, height: -9999))
        XCTAssertEqual(viewport.clampedPanOffset, CGSize(width: 600, height: -300))
        XCTAssertEqual(viewport.videoRect, CGRect(x: 0, y: -600, width: 2000, height: 1200))
        XCTAssertEqual(viewport.panned(by: CGSize(width: -9999, height: 9999)),
                       CGSize(width: -600, height: 300))
    }

    func testSmallActualSizeFrameStaysCenteredAndFitIgnoresPan() {
        let actual = SessionViewport(sourceSize: CGSize(width: 800, height: 600),
                                     viewportSize: CGSize(width: 800, height: 600),
                                     backingScale: 2, scalingMode: .actualSize,
                                     panOffset: CGSize(width: 100, height: 100))
        XCTAssertEqual(actual.videoRect, CGRect(x: 200, y: 150, width: 400, height: 300))
        XCTAssertEqual(actual.clampedPanOffset, .zero)
        let fit = SessionViewport(sourceSize: actual.sourceSize, viewportSize: actual.viewportSize,
                                  backingScale: 2, panOffset: CGSize(width: 100, height: 100))
        XCTAssertEqual(fit.videoRect, CGRect(x: 0, y: 0, width: 800, height: 600))
        XCTAssertEqual(SessionViewport(sourceSize: .zero, viewportSize: actual.viewportSize).videoRect, .zero)
    }
}

@MainActor
final class SessionInputSafetyTests: XCTestCase {
    private let viewport = SessionViewport(sourceSize: CGSize(width: 800, height: 600),
                                           viewportSize: CGSize(width: 800, height: 600))

    func testBrowseModeConsumesKeyboardMouseAndWheelWithoutRemoteCommands() throws {
        var sent: [RemoteInputCommand] = []
        var pans: [CGSize] = []
        let view = SessionInputView.InputView(viewport: viewport, interactionMode: .browse,
                                             acceptsRemoteInput: true, sendInput: { sent.append($0) },
                                             releaseInput: {}, exitFullscreen: { false }, onPan: { pans.append($0) })
        view.frame = CGRect(origin: .zero, size: viewport.viewportSize)
        view.keyDown(with: key(.keyDown))
        view.keyUp(with: key(.keyUp))
        view.mouseMoved(with: mouse(.mouseMoved, x: 100, y: 100))
        view.mouseDown(with: mouse(.leftMouseDown, x: 100, y: 100))
        view.mouseDragged(with: mouse(.leftMouseDragged, x: 120, y: 140))
        view.mouseUp(with: mouse(.leftMouseUp, x: 120, y: 140))
        view.rightMouseDown(with: mouse(.rightMouseDown, x: 120, y: 140))
        view.rightMouseUp(with: mouse(.rightMouseUp, x: 120, y: 140))
        let wheel = try XCTUnwrap(CGEvent(scrollWheelEvent2Source: nil, units: .pixel,
                                         wheelCount: 2, wheel1: 10, wheel2: 4, wheel3: 0))
        view.scrollWheel(with: try XCTUnwrap(NSEvent(cgEvent: wheel)))
        XCTAssertTrue(sent.isEmpty)
        XCTAssertEqual(pans.count, 2)
        XCTAssertNotEqual(pans[0], .zero)
    }

    func testChangingModeAndLosingFocusReleaseInputsAndStopNewCommands() {
        var sent: [RemoteInputCommand] = []
        var released = 0
        let view = SessionInputView.InputView(viewport: viewport, interactionMode: .control,
                                             acceptsRemoteInput: true, sendInput: { sent.append($0) },
                                             releaseInput: { released += 1 }, exitFullscreen: { false }, onPan: { _ in })
        view.frame = CGRect(origin: .zero, size: viewport.viewportSize)
        view.keyDown(with: key(.keyDown))
        XCTAssertFalse(sent.isEmpty)
        view.interactionMode = .browse
        XCTAssertEqual(released, 1)
        let count = sent.count
        view.keyDown(with: key(.keyDown))
        XCTAssertEqual(sent.count, count)
        _ = view.resignFirstResponder()
        XCTAssertEqual(released, 2)
        view.interactionMode = .control
        view.acceptsRemoteInput = false
        XCTAssertEqual(released, 4)
        view.keyDown(with: key(.keyDown))
        XCTAssertEqual(sent.count, count)
    }

    func testEscExitsFullscreenWithoutSendingEitherKeyEdgeToRemote() {
        var sent: [RemoteInputCommand] = []
        var exits = 0
        var released = 0
        let view = SessionInputView.InputView(viewport: viewport, interactionMode: .control,
                                             acceptsRemoteInput: true, sendInput: { sent.append($0) },
                                             releaseInput: { released += 1 },
                                             exitFullscreen: { exits += 1; return true }, onPan: { _ in })
        view.keyDown(with: key(.keyDown, code: 0x35))
        view.keyUp(with: key(.keyUp, code: 0x35))
        XCTAssertEqual(exits, 1)
        XCTAssertEqual(released, 1)
        XCTAssertTrue(sent.isEmpty)
    }

    func testPresentationModeChangesAndWindowResignReleaseInput() {
        _ = NSApplication.shared
        let presentation = SessionPresentation()
        let window = NSWindow(contentRect: CGRect(x: 0, y: 0, width: 800, height: 600),
                              styleMask: [.titled, .resizable], backing: .buffered, defer: true)
        var released = 0
        presentation.releaseInput = { released += 1 }
        presentation.attach(window: window)
        XCTAssertFalse(presentation.isFullscreen)
        XCTAssertTrue(presentation.toolbarIsVisible)
        presentation.panOffset = CGSize(width: 10, height: 20)
        presentation.setScalingMode(.actualSize)
        XCTAssertEqual(presentation.panOffset, .zero)
        presentation.setInteractionMode(.browse)
        XCTAssertEqual(released, 2)
        NotificationCenter.default.post(name: NSWindow.didResignKeyNotification, object: window)
        XCTAssertEqual(released, 3)
        presentation.cleanup()
        XCTAssertEqual(released, 4)
    }

    private func key(_ type: NSEvent.EventType, code: UInt16 = 0) -> NSEvent {
        NSEvent.keyEvent(with: type, location: .zero, modifierFlags: [], timestamp: 0,
                        windowNumber: 0, context: nil, characters: code == 0x35 ? "\u{1b}" : "a",
                        charactersIgnoringModifiers: "a", isARepeat: false, keyCode: code)!
    }

    private func mouse(_ type: NSEvent.EventType, x: CGFloat, y: CGFloat) -> NSEvent {
        NSEvent.mouseEvent(with: type, location: CGPoint(x: x, y: y), modifierFlags: [],
                          timestamp: 0, windowNumber: 0, context: nil, eventNumber: 0,
                          clickCount: 1, pressure: 1)!
    }
}

@MainActor
final class SessionFullscreenLifecycleTests: XCTestCase {
    func testCleanupDuringEnterKeepsObserverAliveUntilOneDeferredExitCompletes() {
        let window = makeWindow()
        var toggles = 0
        var presentation: SessionPresentation? = SessionPresentation(toggleWindowFullscreen: { _ in toggles += 1 })
        weak var retainedPresentation = presentation
        presentation?.attach(window: window)
        presentation?.toggleFullscreen()
        XCTAssertEqual(presentation?.fullscreenPhase, .entering)
        XCTAssertEqual(toggles, 1)
        presentation?.cleanup()
        presentation?.cleanup()
        presentation = nil
        XCTAssertNotNil(retainedPresentation, "Cleanup must keep the native transition observer alive after SwiftUI releases its view.")

        NotificationCenter.default.post(name: NSWindow.didEnterFullScreenNotification, object: window)
        XCTAssertEqual(toggles, 2, "Enter completion must request exactly one exit.")
        XCTAssertEqual(retainedPresentation?.fullscreenPhase, .exiting)
        retainedPresentation?.cleanup()
        _ = retainedPresentation?.exitFullscreen()
        XCTAssertEqual(toggles, 2)

        NotificationCenter.default.post(name: NSWindow.didExitFullScreenNotification, object: window)
        XCTAssertNil(retainedPresentation, "Exit completion must release the temporary lifetime owner.")
    }

    func testNativeEnterAndWindowDetachAlsoDeferCleanupUntilExit() {
        let window = makeWindow()
        var toggles = 0
        let presentation = SessionPresentation(toggleWindowFullscreen: { _ in toggles += 1 })
        presentation.attach(window: window)
        NotificationCenter.default.post(name: NSWindow.willEnterFullScreenNotification, object: window)
        presentation.attach(window: nil)
        XCTAssertTrue(presentation.cleanupRequested)
        XCTAssertEqual(presentation.fullscreenPhase, .entering)
        XCTAssertEqual(toggles, 0)
        NotificationCenter.default.post(name: NSWindow.didEnterFullScreenNotification, object: window)
        XCTAssertEqual(toggles, 1)
        NotificationCenter.default.post(name: NSWindow.didExitFullScreenNotification, object: window)
        XCTAssertEqual(presentation.fullscreenPhase, .windowed)
        XCTAssertFalse(presentation.isFullscreen)
    }

    func testEscDuringEnterWaitsForNativeCompletionAndDoesNotToggleTwice() {
        let window = makeWindow()
        var toggles = 0
        let presentation = SessionPresentation(toggleWindowFullscreen: { _ in toggles += 1 })
        presentation.attach(window: window)
        presentation.toggleFullscreen()
        XCTAssertTrue(presentation.exitFullscreen())
        presentation.toggleFullscreen()
        XCTAssertEqual(toggles, 1)
        NotificationCenter.default.post(name: NSWindow.didEnterFullScreenNotification, object: window)
        XCTAssertEqual(toggles, 2)
        XCTAssertTrue(presentation.exitFullscreen())
        XCTAssertEqual(toggles, 2)
        NotificationCenter.default.post(name: NSWindow.didExitFullScreenNotification, object: window)
        XCTAssertFalse(presentation.exitFullscreen())
        presentation.cleanup()
    }

    func testClosingWindowDuringEnterReleasesDeferredCleanupOwner() {
        let window = makeWindow()
        var presentation: SessionPresentation? = SessionPresentation(toggleWindowFullscreen: { _ in })
        weak var retainedPresentation = presentation
        presentation?.attach(window: window)
        presentation?.toggleFullscreen()
        presentation?.cleanup()
        presentation = nil
        XCTAssertNotNil(retainedPresentation)
        NotificationCenter.default.post(name: NSWindow.willCloseNotification, object: window)
        XCTAssertNil(retainedPresentation)
    }

    private func makeWindow() -> NSWindow {
        _ = NSApplication.shared
        return NSWindow(contentRect: CGRect(x: 0, y: 0, width: 800, height: 600),
                        styleMask: [.titled, .resizable], backing: .buffered, defer: true)
    }
}
