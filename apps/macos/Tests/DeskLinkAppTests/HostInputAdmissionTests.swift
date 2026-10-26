import CoreGraphics
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

/// Synthetic C metadata and scheduling only. No Rust host, OS capture,
/// permission grant, or SystemCGEventBackend injection is exercised here.
@MainActor
final class HostInputAdmissionTests: XCTestCase {
    func testQueuedInputIsRejectedAfterNativeConnectingWhileUIStillConnected() async throws {
        let (bridge, backend) = fixture()
        let first = try await connect(bridge, generation: 11)
        let pending = try XCTUnwrap(bridge.enqueueNativeInputForTesting(
            mediaGeneration: 11, streamID: first.streamID, inputEpoch: 101))
        XCTAssertEqual(pending.context, HostInputContext(streamID: first.streamID, inputEpoch: 101))
        XCTAssertEqual(pending.mediaGeneration, 11)
        XCTAssertEqual(pending.captureOwner, first)
        XCTAssertEqual(pending.command, .mouseButton(.left, pressed: true))
        bridge.enqueueNativeStateForTesting(.unavailable, mediaGeneration: 0)
        // Both callbacks are queued; only the native gate knows Connecting.
        XCTAssertEqual(bridge.state, .connected)
        XCTAssertEqual(bridge.activeCaptureOwnerForTesting, first)
        XCTAssertTrue(backend.events.isEmpty)
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertTrue(bridge.injectedInputsForTesting.isEmpty)
        XCTAssertTrue(backend.events.isEmpty)
        XCTAssertEqual(bridge.state, .connecting)

        let successor = try await connect(bridge, generation: 12)
        try await assertAccepted(bridge, backend: backend, owner: successor, epoch: 202)
        await bridge.shutdownAndWait()
    }

    func testQueuedInputIsRejectedAfterNativeNewNoiseWhileUIStillHasOldCapture() async throws {
        let (bridge, backend) = fixture()
        let first = try await connect(bridge, generation: 21)
        let pending = try XCTUnwrap(bridge.enqueueNativeInputForTesting(
            mediaGeneration: 21, streamID: first.streamID, inputEpoch: 303))
        bridge.enqueueNativeStateForTesting(.unavailable, mediaGeneration: 0)
        bridge.enqueueNativeStateForTesting(.connected, mediaGeneration: 22)
        // New Noise is observed natively before any queued MainActor delivery.
        XCTAssertEqual(bridge.state, .connected)
        XCTAssertEqual(bridge.activeCaptureOwnerForTesting, first)
        XCTAssertEqual(pending.connectionReceipt.mediaGeneration, 21)
        await bridge.flushCaptureCallbacksForTesting()
        await bridge.waitForCaptureStartForTesting()
        XCTAssertTrue(backend.events.isEmpty)
        XCTAssertTrue(bridge.injectedInputsForTesting.isEmpty)
        let successor = try XCTUnwrap(bridge.activeCaptureOwnerForTesting)
        XCTAssertNotEqual(successor, first)
        XCTAssertEqual(successor.mediaGeneration, 22)
        try await assertAccepted(bridge, backend: backend, owner: successor, epoch: 404)
        await bridge.shutdownAndWait()
    }

    func testOriginalRustOwnerAndContextAreNeverRelabelledByCurrentCapture() async throws {
        let (bridge, backend) = fixture()
        let first = try await connect(bridge, generation: 31)
        bridge.enqueueNativeStateForTesting(.unavailable, mediaGeneration: 0)
        await bridge.flushCaptureCallbacksForTesting()
        let successor = try await connect(bridge, generation: 32)
        XCTAssertNotEqual(first.streamID, successor.streamID)
        // A current Swift receipt cannot bless an event stamped by old Rust.
        XCTAssertNil(bridge.enqueueNativeInputForTesting(mediaGeneration: 31,
            streamID: successor.streamID, inputEpoch: 505))
        // Likewise, a new Noise owner cannot silently rewrite an old stream.
        XCTAssertNil(bridge.enqueueNativeInputForTesting(mediaGeneration: 32,
            streamID: first.streamID, inputEpoch: 505))
        XCTAssertNil(bridge.enqueueNativeInputForTesting(mediaGeneration: 32,
            streamID: successor.streamID, inputEpoch: 0))
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertTrue(backend.events.isEmpty)
        try await assertAccepted(bridge, backend: backend, owner: successor, epoch: 606)
        await bridge.shutdownAndWait()
    }

    func testRetiringCaptureRejectsAlreadyPreparedInputAtFinalGate() throws {
        let gate = HostCaptureAdmission(runtimeGeneration: 7)
        let connection = try XCTUnwrap(gate.observe(.connected, mediaGeneration: 71))
        let owner = try XCTUnwrap(gate.register(connection, streamID: 8))
        let input = try XCTUnwrap(gate.captureInput(mediaGeneration: 71,
            context: HostInputContext(streamID: 8, inputEpoch: 77), command: .mouseButton(.left, pressed: true)))
        gate.retire(owner)
        var injected = false
        XCTAssertNil(gate.withInputAdmission(input) { injected = true })
        XCTAssertFalse(injected)
    }

    func testReusedStreamCannotBlessAnOldNoiseInputOrOverwriteOriginalEpoch() throws {
        let gate = HostCaptureAdmission(runtimeGeneration: 9)
        let first = try XCTUnwrap(gate.observe(.connected, mediaGeneration: 81))
        _ = try XCTUnwrap(gate.register(first, streamID: 5))
        let old = try XCTUnwrap(gate.captureInput(mediaGeneration: 81,
            context: HostInputContext(streamID: 5, inputEpoch: 91), command: .wheel(deltaX: 1, deltaY: 2)))
        _ = gate.observe(.unavailable, mediaGeneration: 0)
        let next = try XCTUnwrap(gate.observe(.connected, mediaGeneration: 82))
        _ = try XCTUnwrap(gate.register(next, streamID: 5))
        XCTAssertNil(gate.captureInput(mediaGeneration: 81, context: old.context, command: old.command))
        XCTAssertNil(gate.withInputAdmission(old) { true })
        let current = try XCTUnwrap(gate.captureInput(mediaGeneration: 82,
            context: HostInputContext(streamID: 5, inputEpoch: 92), command: old.command))
        XCTAssertEqual(current.context.inputEpoch, 92)
        XCTAssertEqual(old.context.inputEpoch, 91)
        XCTAssertEqual(current.mediaGeneration, 82)
        XCTAssertEqual(gate.withInputAdmission(current) { current.command }, old.command)
        // A future same-stream geometry epoch needs its own authoritative
        // publication contract. These synthetic inputs do not establish one.
    }

    func testReleaseAllAndUserStopKeepTheirCleanupSemantics() async throws {
        let (bridge, backend) = fixture()
        let owner = try await connect(bridge, generation: 91)
        try await assertAccepted(bridge, backend: backend, owner: owner, epoch: 707)
        bridge.enqueueNativeReleaseAllForTesting()
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertEqual(backend.events, [.button(.left, true), .button(.left, false)])
        _ = bridge.enqueueNativeInputForTesting(mediaGeneration: 91, streamID: owner.streamID, inputEpoch: 707)
        bridge.stop() // Synchronous native admission retirement precedes cleanup.
        await bridge.flushCaptureCallbacksForTesting()
        await bridge.shutdownAndWait()
        XCTAssertEqual(backend.events, [.button(.left, true), .button(.left, false)])
        XCTAssertEqual(bridge.injectedInputsForTesting.count, 1)
        XCTAssertEqual(bridge.state, .closed)
    }

    private func fixture() -> (HostBridge, HostInputRecordingBackend) {
        let permissions = MacPermissionSnapshot(screenRecording: .granted, accessibility: .granted,
            screenRecordingSettingsURL: MacPermissions.screenRecordingSettingsURL,
            accessibilitySettingsURL: MacPermissions.accessibilitySettingsURL)
        let bridge = HostBridge.testing(permissionSnapshot: permissions,
            captureFactory: { _, _ in InputAdmissionCaptureSession() })
        let backend = HostInputRecordingBackend()
        bridge.installInputBackendForTesting(backend)
        bridge.recordInputDeliveriesForTesting = true
        return (bridge, backend)
    }

    private func connect(_ bridge: HostBridge, generation: UInt64) async throws -> HostCaptureOwner {
        bridge.enqueueNativeStateForTesting(.connected, mediaGeneration: generation)
        await bridge.flushCaptureCallbacksForTesting()
        await bridge.waitForCaptureStartForTesting()
        return try XCTUnwrap(bridge.activeCaptureOwnerForTesting)
    }

    private func assertAccepted(_ bridge: HostBridge, backend: HostInputRecordingBackend,
                                owner: HostCaptureOwner, epoch: UInt64) async throws {
        let prepared = try XCTUnwrap(bridge.enqueueNativeInputForTesting(mediaGeneration: owner.mediaGeneration,
            streamID: owner.streamID, inputEpoch: epoch))
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertEqual(backend.events, [.button(.left, true)])
        let injected = try XCTUnwrap(bridge.injectedInputsForTesting.last)
        XCTAssertTrue(injected.admission === prepared.admission)
        XCTAssertEqual(injected.connectionReceipt, prepared.connectionReceipt)
        XCTAssertEqual(injected.captureOwner, owner)
        XCTAssertEqual(injected.mediaGeneration, owner.mediaGeneration)
        XCTAssertEqual(injected.context, HostInputContext(streamID: owner.streamID, inputEpoch: epoch))
        XCTAssertEqual(injected.command, .mouseButton(.left, pressed: true))
    }
}

@MainActor
private final class InputAdmissionCaptureSession: HostCaptureSession {
    func start() async throws -> HostCaptureGeometry {
        HostCaptureGeometry(displayFrame: CGRect(x: 0, y: 0, width: 640, height: 360),
                            videoSize: CGSize(width: 640, height: 360))
    }
    func stop() async {}
    func requestKeyframe() {}
}

private final class HostInputRecordingBackend: CGEventBackend {
    enum Event: Equatable {
        case move(CGPoint)
        case button(MouseButton, Bool)
        case wheel(Int32, Int32)
        case key(UInt32, Bool, Modifiers)
        case unicode(String, Bool, Modifiers)
    }
    private(set) var events: [Event] = []
    func moveMouse(to point: CGPoint) { events.append(.move(point)) }
    func postMouseButton(_ button: MouseButton, pressed: Bool, at point: CGPoint) { events.append(.button(button, pressed)) }
    func postScroll(deltaX: Int32, deltaY: Int32) { events.append(.wheel(deltaX, deltaY)) }
    func postKey(code: UInt32, pressed: Bool, modifiers: Modifiers) { events.append(.key(code, pressed, modifiers)) }
    func postUnicode(_ text: String, pressed: Bool, modifiers: Modifiers) { events.append(.unicode(text, pressed, modifiers)) }
}
