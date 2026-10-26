import CoreGraphics
import XCTest
@testable import DeskLinkApp

@MainActor
final class HostBridgeTests: XCTestCase {
    func testUnknownEventIsCountedAndTransferDoesNotBecomeFatal() {
        let bridge = HostBridge.testing(permissionSnapshot: .denied)
        bridge.acceptEventKindForTesting(UInt32.max, poisonUnrelatedFields: true)
        XCTAssertEqual(bridge.ignoredHostEventCount, 1)
        XCTAssertEqual(bridge.state, .idle)
        XCTAssertNil(bridge.lastError)
        bridge.acceptEventKindForTesting(8) // Generated Rust/C Host Transfer kind.
        XCTAssertEqual(bridge.ignoredHostEventCount, 1)
        XCTAssertEqual(bridge.state, .idle)
        XCTAssertTrue(bridge.lastError?.contains("尚不支持") == true)
    }

    func testHostBridgeStartsSafeAndApprovalIsNotImplicit() {
        let bridge = HostBridge.testing(permissionSnapshot: .denied)

        XCTAssertEqual(bridge.state, .idle)
        XCTAssertFalse(bridge.canStartCapture)
        XCTAssertFalse(bridge.canInjectInput)
        XCTAssertFalse(bridge.pendingApproval?.isApproved ?? false)
    }

    func testNativeGateRetiresOldDeliveryBeforeItsMainActorStateNotification() async throws {
        let factory = CaptureTestFactory()
        let bridge = syntheticBridge(factory)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 1)
        await bridge.waitForCaptureStartForTesting()
        let retired = try XCTUnwrap(factory.sessions.first)
        let oldOwner = retired.owner
        // Native callback has run, but Connecting has not reached the UI.
        let delayedConnecting = try XCTUnwrap(bridge.observeConnectionForTesting(.unavailable, mediaGeneration: 1, deliver: false))
        XCTAssertEqual(bridge.state, .connected)
        retired.emitConfiguration()
        retired.emitAccessUnit()
        retired.emitEncodingError()
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertTrue(bridge.admittedVideoEventsForTesting.isEmpty)
        XCTAssertNil(bridge.lastError)

        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 2)
        bridge.deliverConnectionForTesting(delayedConnecting)
        await bridge.waitForCaptureStartForTesting()
        let successor = try XCTUnwrap(factory.sessions.last)
        XCTAssertNotEqual(oldOwner, successor.owner)
        XCTAssertEqual(bridge.state, .connected)
        retired.emitEncodingError()
        retired.emitStopped()
        successor.emitConfiguration(version: 7)
        successor.emitAccessUnit(version: 7)
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertEqual(bridge.admittedVideoEventsForTesting.map(\.owner), [successor.owner, successor.owner])
        XCTAssertEqual(bridge.admittedVideoEventsForTesting.map(\.event), [successor.configuration(version: 7), successor.accessUnit(version: 7)])
        XCTAssertNil(bridge.lastError)
    }

    func testNewCaptureWaitsForCancelledStartAndCleanupBarrier() async throws {
        let startGate = CaptureTestGate()
        let stopGate = CaptureTestGate()
        let factory = CaptureTestFactory(plans: [.init(startGate: startGate, stopGate: stopGate), .init()])
        let bridge = syntheticBridge(factory)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 1)
        await startGate.waitUntilEntered()
        _ = bridge.observeConnectionForTesting(.unavailable, mediaGeneration: 1)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 2)
        let successor = try XCTUnwrap(factory.sessions.last)
        XCTAssertEqual(successor.startCalls, 0)
        await startGate.release()
        await stopGate.waitUntilEntered()
        XCTAssertEqual(successor.startCalls, 0)
        XCTAssertEqual(bridge.activeCaptureOwnerForTesting, successor.owner)
        await stopGate.release()
        await bridge.waitForCaptureStartForTesting()
        XCTAssertEqual(successor.startCalls, 1)
        XCTAssertEqual(successor.stopCalls, 0)
        XCTAssertTrue(bridge.captureRunningForTesting)
        XCTAssertNil(bridge.lastError)
    }

    func testLateStartFailureCannotClearSuccessorFlagsOrPublishError() async throws {
        let gate = CaptureTestGate()
        let factory = CaptureTestFactory(plans: [.init(startGate: gate, fails: true), .init()])
        let bridge = syntheticBridge(factory)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 1)
        await gate.waitUntilEntered()
        _ = bridge.observeConnectionForTesting(.unavailable, mediaGeneration: 1)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 2)
        await gate.release()
        await bridge.waitForCaptureStartForTesting()
        XCTAssertEqual(bridge.state, .connected)
        XCTAssertTrue(bridge.captureRunningForTesting)
        XCTAssertEqual(bridge.activeCaptureOwnerForTesting, factory.sessions.last?.owner)
        XCTAssertNil(bridge.lastError)
    }

    func testFailedStartBurnsStreamIDAndExistingPermissionRefreshCanRetry() async throws {
        let factory = CaptureTestFactory(plans: [.init(fails: true), .init()])
        let bridge = syntheticBridge(factory)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 1)
        await bridge.waitForCaptureStartForTesting()
        let first = try XCTUnwrap(factory.sessions.first)
        XCTAssertNil(bridge.activeCaptureOwnerForTesting)
        XCTAssertFalse(bridge.captureRunningForTesting)
        XCTAssertNotNil(bridge.lastError)
        XCTAssertEqual(bridge.state, .connected)
        bridge.refreshPermissions() // Static fake permissions, not OS queries.
        await bridge.waitForCaptureStartForTesting()
        let second = try XCTUnwrap(factory.sessions.last)
        XCTAssertGreaterThan(second.owner.streamID, first.owner.streamID)
        XCTAssertTrue(bridge.captureRunningForTesting)
        XCTAssertNil(bridge.lastError)
    }

    func testFinalAdmissionRejectsForeignStreamInsteadOfRetaggingIt() async throws {
        let factory = CaptureTestFactory()
        let bridge = syntheticBridge(factory)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 11)
        await bridge.waitForCaptureStartForTesting()
        let session = try XCTUnwrap(factory.sessions.first)
        session.callbacks.video(.configuration(streamID: session.owner.streamID + 1, version: 9, width: 640, height: 360, annexBParameterSets: Data([1])))
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertTrue(bridge.admittedVideoEventsForTesting.isEmpty)
        session.emitConfiguration(version: 9)
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertEqual(bridge.admittedVideoEventsForTesting.first?.owner.mediaGeneration, 11)
        XCTAssertEqual(bridge.admittedVideoEventsForTesting.first?.event, session.configuration(version: 9))
    }

    func testRustStaleGenerationResponseIsSilentWhileOtherSubmitErrorsAreRetryable() async throws {
        let factory = CaptureTestFactory()
        let bridge = syntheticBridge(factory)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 41)
        await bridge.waitForCaptureStartForTesting()
        let old = try XCTUnwrap(factory.sessions.first)
        // Simulates Rust already moving to another Noise before its native
        // state notification reaches Swift. It is not a live Rust transport.
        bridge.captureSubmitResultForTesting = 5 // DESKLINK_STALE_GENERATION
        old.emitConfiguration()
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertEqual(bridge.activeCaptureOwnerForTesting, old.owner)
        XCTAssertEqual(bridge.admittedVideoEventsForTesting.last?.owner.mediaGeneration, 41)
        XCTAssertNil(bridge.lastError)
        bridge.captureSubmitResultForTesting = 3 // DESKLINK_INVALID_STATE
        old.emitAccessUnit()
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertNil(bridge.activeCaptureOwnerForTesting)
        XCTAssertFalse(bridge.captureRunningForTesting)
        XCTAssertEqual(bridge.state, .connected)
        XCTAssertNotNil(bridge.lastError)
        bridge.captureSubmitResultForTesting = 0
        bridge.refreshPermissions()
        await bridge.waitForCaptureStartForTesting()
        XCTAssertTrue(bridge.captureRunningForTesting)
        XCTAssertNil(bridge.lastError)
    }

    func testCurrentEncoderErrorRetiresOnlyCaptureAndCanRetry() async throws {
        let factory = CaptureTestFactory()
        let bridge = syntheticBridge(factory)
        _ = bridge.observeConnectionForTesting(.connected, mediaGeneration: 1)
        await bridge.waitForCaptureStartForTesting()
        let first = try XCTUnwrap(factory.sessions.first)
        first.emitEncodingError()
        await bridge.flushCaptureCallbacksForTesting()
        XCTAssertEqual(bridge.state, .connected)
        XCTAssertNil(bridge.activeCaptureOwnerForTesting)
        XCTAssertNotNil(bridge.lastError)
        bridge.refreshPermissions()
        await bridge.waitForCaptureStartForTesting()
        XCTAssertTrue(bridge.captureRunningForTesting)
        XCTAssertGreaterThan(try XCTUnwrap(bridge.activeCaptureOwnerForTesting).streamID, first.owner.streamID)
        XCTAssertNil(bridge.lastError)
    }

    private func syntheticBridge(_ factory: CaptureTestFactory) -> HostBridge {
        let snapshot = MacPermissionSnapshot(screenRecording: .granted, accessibility: .granted,
            screenRecordingSettingsURL: MacPermissions.screenRecordingSettingsURL,
            accessibilitySettingsURL: MacPermissions.accessibilitySettingsURL)
        let bridge = HostBridge.testing(permissionSnapshot: snapshot, captureFactory: { owner, callbacks in factory.make(owner, callbacks) })
        bridge.recordCaptureEventsForTesting = true
        return bridge
    }
}

final class HostCaptureAdmissionTests: XCTestCase {
    func testOldReceiptsAndRetirementCannotChangeNewCaptureOwner() throws {
        let gate = HostCaptureAdmission(runtimeGeneration: 7)
        let first = try XCTUnwrap(gate.observe(.connected, mediaGeneration: 1))
        let old = try XCTUnwrap(gate.register(first, streamID: 1))
        let connecting = try XCTUnwrap(gate.observe(.unavailable, mediaGeneration: 1))
        XCTAssertFalse(gate.allows(old))
        let connected = try XCTUnwrap(gate.observe(.connected, mediaGeneration: 2))
        XCTAssertFalse(gate.matches(connecting))
        XCTAssertNil(gate.register(first, streamID: 2))
        let current = try XCTUnwrap(gate.register(connected, streamID: 3))
        gate.retire(old)
        XCTAssertTrue(gate.allows(current))
        XCTAssertNil(gate.observe(.connected, mediaGeneration: 1))
        XCTAssertNil(gate.observe(.unavailable, mediaGeneration: 1))
        XCTAssertTrue(gate.allows(current))
    }

    func testRetiredRuntimeGateCannotRetireAnotherRuntime() throws {
        let old = HostCaptureAdmission(runtimeGeneration: 7)
        old.close()
        let current = HostCaptureAdmission(runtimeGeneration: 8)
        let connected = try XCTUnwrap(current.observe(.connected, mediaGeneration: 1))
        let owner = try XCTUnwrap(current.register(connected, streamID: 1))
        XCTAssertNil(old.observe(.unavailable, mediaGeneration: 1))
        XCTAssertTrue(current.allows(owner))
    }

    func testStoppingSealsCaptureButStillDeliversFollowingClosedReceipt() throws {
        let gate = HostCaptureAdmission(runtimeGeneration: 7)
        let connected = try XCTUnwrap(gate.observe(.connected, mediaGeneration: 1))
        let owner = try XCTUnwrap(gate.register(connected, streamID: 1))
        let stopping = try XCTUnwrap(gate.observe(.terminal, mediaGeneration: 0))
        XCTAssertFalse(gate.allows(owner))
        let closed = try XCTUnwrap(gate.observe(.terminal, mediaGeneration: 0))
        XCTAssertFalse(gate.matches(stopping))
        XCTAssertTrue(gate.matches(closed))
        XCTAssertNil(gate.observe(.connected, mediaGeneration: 2))
        XCTAssertNil(gate.register(connected, streamID: 2))
    }
}

private actor CaptureTestGate {
    private var entered = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    private var continuation: CheckedContinuation<Void, Never>?
    func pause() async {
        entered = true
        for waiter in waiters { waiter.resume() }
        waiters.removeAll()
        await withCheckedContinuation { continuation = $0 }
    }
    func waitUntilEntered() async {
        if entered { return }
        await withCheckedContinuation { waiters.append($0) }
    }
    func release() { continuation?.resume(); continuation = nil }
}

private struct CaptureTestPlan {
    var startGate: CaptureTestGate?
    var stopGate: CaptureTestGate?
    var fails = false
}

@MainActor
private final class CaptureTestFactory {
    private var plans: [CaptureTestPlan]
    private(set) var sessions: [CaptureTestSession] = []
    init(plans: [CaptureTestPlan] = []) { self.plans = plans }
    func make(_ owner: HostCaptureOwner, _ callbacks: HostCaptureCallbacks) -> CaptureTestSession {
        let plan = plans.isEmpty ? CaptureTestPlan() : plans.removeFirst()
        let session = CaptureTestSession(owner, callbacks, plan)
        sessions.append(session)
        return session
    }
}

@MainActor
private final class CaptureTestSession: HostCaptureSession {
    let owner: HostCaptureOwner
    let callbacks: HostCaptureCallbacks
    private let plan: CaptureTestPlan
    private(set) var startCalls = 0
    private(set) var stopCalls = 0
    init(_ owner: HostCaptureOwner, _ callbacks: HostCaptureCallbacks, _ plan: CaptureTestPlan) {
        self.owner = owner; self.callbacks = callbacks; self.plan = plan
    }
    func start() async throws -> HostCaptureGeometry {
        startCalls += 1
        if let gate = plan.startGate { await gate.pause() }
        if plan.fails { throw ScreenCaptureSourceError.displayUnavailable }
        return HostCaptureGeometry(displayFrame: CGRect(x: 0, y: 0, width: 640, height: 360), videoSize: CGSize(width: 640, height: 360))
    }
    func stop() async {
        stopCalls += 1
        if let gate = plan.stopGate { await gate.pause() }
    }
    func requestKeyframe() {}
    func configuration(version: UInt32 = 1) -> EncodedVideoEvent {
        .configuration(streamID: owner.streamID, version: version, width: 640, height: 360, annexBParameterSets: Data([1]))
    }
    func accessUnit(version: UInt32 = 1) -> EncodedVideoEvent {
        .accessUnit(streamID: owner.streamID, frameID: 1, configVersion: version, isKeyframe: true, annexB: Data([2]))
    }
    func emitConfiguration(version: UInt32 = 1) { callbacks.video(configuration(version: version)) }
    func emitAccessUnit(version: UInt32 = 1) { callbacks.video(accessUnit(version: version)) }
    func emitEncodingError() { callbacks.encodingError(.encoding(-1)) }
    func emitStopped() { callbacks.captureStopped("synthetic old capture stopped") }
}
