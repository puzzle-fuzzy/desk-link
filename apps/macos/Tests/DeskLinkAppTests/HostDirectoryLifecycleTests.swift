import DeskLinkAppleCore
import DeskLinkC
import Foundation
import XCTest
@testable import DeskLinkApp

@MainActor
final class HostDirectoryLifecycleTests: XCTestCase {
    func testIdentityAndFixedConfigurationDoNotRequireCaptureOrRegister() async throws {
        let store = TestHostFixedStore()
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(store: store, factory: factory, granted: false)
        await bridge.prepareAccessCredentials()
        XCTAssertEqual(bridge.publicDeviceID, "286 952 306 183")
        XCTAssertEqual(bridge.sharingStatus, .off)
        XCTAssertTrue(factory.runtimes.isEmpty)
        let saved = await bridge.setFixedPassword("ab cd-2345")
        XCTAssertTrue(saved)
        XCTAssertTrue(bridge.hasFixedPassword)
        XCTAssertTrue(factory.runtimes.isEmpty)
        XCTAssertNil(bridge.temporaryPassword)
        XCTAssertNotNil(try store.load())
    }

    func testMissingPermissionsCannotPublishAnInvitation() async {
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(factory: factory, granted: false)
        bridge.startSharing()
        await waitFor { if case .failed = bridge.sharingStatus { return true }; return false }
        XCTAssertTrue(factory.runtimes.isEmpty)
        XCTAssertNil(bridge.temporaryPassword)
        XCTAssertNil(bridge.pairingInvite)
        await bridge.shutdownAndWait()
    }

    func testRegistrationMustCompleteBeforeCredentialsBecomeUsable() async {
        let factory = TestHostRuntimeFactory(blockRegistration: true)
        let bridge = makeBridge(factory: factory)
        bridge.startSharing()
        await waitFor { factory.runtimes.first?.registrationStarted == true }
        XCTAssertEqual(bridge.sharingStatus, .registering)
        XCTAssertNil(bridge.temporaryPassword)
        XCTAssertNil(bridge.pairingInvite)
        factory.runtimes.first?.completeRegistration()
        await waitFor { bridge.sharingStatus == .ready }
        XCTAssertEqual(bridge.temporaryPassword, "ABCD2345")
        XCTAssertNotNil(bridge.pairingInvite)
        XCTAssertNotNil(bridge.temporaryPasswordExpiresAt)
        await bridge.shutdownAndWait()
    }

    func testStopDuringRegistrationRejectsLateSuccessAndDestroysRuntime() async {
        let factory = TestHostRuntimeFactory(blockRegistration: true)
        let bridge = makeBridge(factory: factory)
        bridge.startSharing()
        await waitFor { factory.runtimes.first?.registrationStarted == true }
        let runtime = factory.runtimes[0]
        let retired = bridge.callbackGenerationForTesting
        bridge.stop()
        runtime.completeRegistration()
        await bridge.shutdownAndWait()
        XCTAssertTrue(runtime.destroyed)
        XCTAssertNil(bridge.temporaryPassword)
        XCTAssertNil(bridge.pairingInvite)
        XCTAssertEqual(bridge.sharingStatus, .off)
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_STATE.rawValue,
            state: DESKLINK_HOST_CONNECTED.rawValue, generation: retired)
        XCTAssertEqual(bridge.state, .closed)
    }

    func testRefreshWaitsForRetiredRegistrationAndDestruction() async {
        let factory = TestHostRuntimeFactory(blockRegistration: true)
        let bridge = makeBridge(factory: factory)
        bridge.startSharing()
        await waitFor { factory.runtimes.first?.registrationStarted == true }
        let first = factory.runtimes[0]
        bridge.refreshTemporaryPassword()
        await Task.yield()
        XCTAssertEqual(factory.runtimes.count, 1)
        XCTAssertNil(bridge.temporaryPassword)
        first.completeRegistration()
        await waitFor { factory.runtimes.count == 2 && factory.runtimes[1].registrationStarted }
        XCTAssertTrue(first.destroyed)
        factory.runtimes[1].completeRegistration()
        await waitFor { bridge.sharingStatus == .ready }
        XCTAssertEqual(factory.lifecycle, ["create", "register", "destroy", "create", "register"])
        await bridge.shutdownAndWait()
    }

    func testFailedRegistrationDoesNotExposeGeneratedPassword() async {
        let factory = TestHostRuntimeFactory(resultCode: DESKLINK_INVALID_STATE.rawValue)
        let bridge = makeBridge(factory: factory)
        bridge.startSharing()
        await waitFor {
            if case .failed = bridge.sharingStatus { return factory.runtimes.first?.destroyed == true }
            return false
        }
        XCTAssertNil(bridge.temporaryPassword)
        XCTAssertNil(bridge.pairingInvite)
        if case .failed = bridge.sharingStatus {} else { XCTFail("failed registration must remain visible") }
    }

    func testFixedPasswordUsesStoredValueWithoutPublishingIt() async throws {
        let store = TestHostFixedStore(try HostAccessPassword("WXYZ6789"))
        let factory = TestHostRuntimeFactory(fixedExpiry: .distantPast)
        let bridge = makeBridge(store: store, factory: factory)
        await bridge.prepareAccessCredentials()
        bridge.setAccessMode(.fixed)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let request = try XCTUnwrap(factory.runtimes.first?.request)
        XCTAssertEqual(request.password, try store.load())
        XCTAssertEqual(request.mode, .fixed)
        XCTAssertEqual(request.deviceNumber, 286_952_306_183)
        XCTAssertTrue(bridge.hasFixedPassword)
        XCTAssertNil(bridge.temporaryPassword)
        XCTAssertNil(bridge.temporaryPasswordExpiresAt)
        XCTAssertNil(bridge.pairingInvite)
        await bridge.shutdownAndWait()
    }

    func testFailedFixedPasswordUpdateKeepsExistingRuntimeAndStoredValue() async throws {
        let original = try HostAccessPassword("WXYZ6789")
        let store = TestHostFixedStore(original)
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(store: store, factory: factory)
        bridge.setAccessMode(.fixed)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        store.failWrite = true
        let saved = await bridge.setFixedPassword("ABCD2345")
        XCTAssertFalse(saved)
        XCTAssertEqual(try store.load(), original)
        XCTAssertEqual(factory.runtimes.count, 1)
        XCTAssertFalse(factory.runtimes[0].destroyed)
        XCTAssertEqual(bridge.sharingStatus, .ready)
        XCTAssertNotNil(bridge.credentialsError)
        await bridge.shutdownAndWait()
    }

    func testSavingFixedPasswordRetiresOldRegistrationBeforeUsingNewValue() async throws {
        let store = TestHostFixedStore(try HostAccessPassword("WXYZ6789"))
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(store: store, factory: factory)
        bridge.setAccessMode(.fixed)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let first = factory.runtimes[0]
        let saved = await bridge.setFixedPassword("ABCD2345")
        XCTAssertTrue(saved)
        await waitFor { factory.runtimes.count == 2 && bridge.sharingStatus == .ready }
        XCTAssertTrue(first.destroyed)
        XCTAssertEqual(factory.runtimes[1].request?.password, try HostAccessPassword("ABCD2345"))
        XCTAssertNil(bridge.temporaryPassword)
        await bridge.shutdownAndWait()
    }

    func testRemovingFixedPasswordSwitchesActiveRegistrationToTemporary() async throws {
        let store = TestHostFixedStore(try HostAccessPassword("WXYZ6789"))
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(store: store, factory: factory)
        bridge.setAccessMode(.fixed)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let first = factory.runtimes[0]
        let removed = await bridge.removeFixedPassword()
        XCTAssertTrue(removed)
        await waitFor { factory.runtimes.count == 2 && bridge.sharingStatus == .ready }
        XCTAssertTrue(first.destroyed)
        XCTAssertFalse(bridge.hasFixedPassword)
        XCTAssertEqual(bridge.accessMode, .temporary)
        XCTAssertNil(try store.load())
        XCTAssertNotNil(bridge.temporaryPassword)
        await bridge.shutdownAndWait()
    }

    func testFixedStoreFailureDoesNotHideStableIdentity() async {
        let store = TestHostFixedStore()
        store.failRead = true
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(store: store, factory: factory, granted: false)
        await bridge.prepareAccessCredentials()
        XCTAssertEqual(bridge.publicDeviceID, "286 952 306 183")
        XCTAssertNotNil(bridge.credentialsError)
        XCTAssertTrue(factory.runtimes.isEmpty)
    }

    func testQRCreationReusesReadyTemporaryDirectoryRegistration() async {
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(factory: factory)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let invite = bridge.pairingInvite
        bridge.createInvite()
        await Task.yield()
        XCTAssertEqual(factory.runtimes.count, 1)
        XCTAssertEqual(bridge.pairingInvite, invite)
        await bridge.shutdownAndWait()
    }

    func testStoppingImmediatelyWithdrawsPendingLocalApproval() async {
        let bridge = makeBridge(factory: TestHostRuntimeFactory())
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_APPROVAL_REQUESTED.rawValue)
        XCTAssertNotNil(bridge.pendingApproval)
        XCTAssertTrue(bridge.canApprove)
        bridge.stop()
        XCTAssertNil(bridge.pendingApproval)
        XCTAssertFalse(bridge.canApprove)
        await bridge.shutdownAndWait()
    }

    func testExactTrustedIdentityReconnectsWithoutAnotherApprovalOrTrustWrite() async {
        let trusted = TestHostTrustStore(trustedController())
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(factory: factory, trusted: trusted)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let controller = trustedController()
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_APPROVAL_REQUESTED.rawValue,
            controllerDeviceID: controller.deviceID, controllerVerifyKey: controller.verifyKey)
        XCTAssertNil(bridge.pendingApproval)
        XCTAssertEqual(factory.runtimes[0].approvalCount, 1)
        XCTAssertEqual(trusted.trustWriteCount, 0)
        bridge.revoke(controller: controller)
        await bridge.shutdownAndWait()
        XCTAssertTrue(factory.runtimes[0].destroyed)
    }

    func testChangedKeyAndUnknownDeviceStillRequireLocalApproval() async {
        let trusted = TestHostTrustStore(trustedController())
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(factory: factory, trusted: trusted)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let controller = trustedController()
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_APPROVAL_REQUESTED.rawValue,
            controllerDeviceID: controller.deviceID, controllerVerifyKey: [UInt8](repeating: 99, count: 32))
        XCTAssertNotNil(bridge.pendingApproval)
        XCTAssertTrue(bridge.canApprove)
        XCTAssertEqual(factory.runtimes[0].approvalCount, 0)
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_APPROVAL_REQUESTED.rawValue,
            controllerDeviceID: [UInt8](repeating: 99, count: 16), controllerVerifyKey: controller.verifyKey)
        XCTAssertNotNil(bridge.pendingApproval)
        XCTAssertEqual(factory.runtimes[0].approvalCount, 0)
        XCTAssertEqual(trusted.trustWriteCount, 0)
        await bridge.shutdownAndWait()
    }

    func testRetiredGenerationCannotAutomaticallyApproveTrustedIdentity() async {
        let trusted = TestHostTrustStore(trustedController())
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(factory: factory, trusted: trusted)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let generation = bridge.callbackGenerationForTesting
        let controller = trustedController()
        bridge.stop()
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_APPROVAL_REQUESTED.rawValue,
            generation: generation, controllerDeviceID: controller.deviceID,
            controllerVerifyKey: controller.verifyKey)
        XCTAssertNil(bridge.pendingApproval)
        XCTAssertEqual(factory.runtimes[0].approvalCount, 0)
        await bridge.shutdownAndWait()
    }

    func testFailedAutomaticApprovalKeepsExplicitLocalApprovalAvailable() async {
        let trusted = TestHostTrustStore(trustedController())
        let factory = TestHostRuntimeFactory(approveResult: DESKLINK_INVALID_STATE.rawValue)
        let bridge = makeBridge(factory: factory, trusted: trusted)
        bridge.startSharing()
        await waitFor { bridge.sharingStatus == .ready }
        let controller = trustedController()
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_APPROVAL_REQUESTED.rawValue,
            controllerDeviceID: controller.deviceID, controllerVerifyKey: controller.verifyKey)
        XCTAssertNotNil(bridge.pendingApproval)
        XCTAssertTrue(bridge.canApprove)
        XCTAssertEqual(factory.runtimes[0].approvalCount, 1)
        XCTAssertEqual(trusted.trustWriteCount, 0)
        XCTAssertNotNil(bridge.lastError)
        await bridge.shutdownAndWait()
    }

    func testTrustedIdentityWithoutCapturePermissionsCannotBeAutoapproved() async {
        let trusted = TestHostTrustStore(trustedController())
        let factory = TestHostRuntimeFactory()
        let bridge = makeBridge(factory: factory, granted: false, trusted: trusted)
        let controller = trustedController()
        bridge.acceptEventForTesting(kind: DESKLINK_HOST_EVENT_APPROVAL_REQUESTED.rawValue,
            controllerDeviceID: controller.deviceID, controllerVerifyKey: controller.verifyKey)
        XCTAssertNotNil(bridge.pendingApproval)
        XCTAssertFalse(bridge.canApprove)
        XCTAssertTrue(factory.runtimes.isEmpty)
    }

    private func trustedController() -> TrustedController {
        TrustedController(deviceID: [UInt8](repeating: 5, count: 16),
            verifyKey: [UInt8](repeating: 6, count: 32), approvedAtUnixSeconds: 1_000,
            displayName: "Known controller")
    }

    private func makeBridge(store: TestHostFixedStore = TestHostFixedStore(),
        factory: TestHostRuntimeFactory, granted: Bool = true, trusted: TestHostTrustStore? = nil) -> HostBridge {
        HostBridge(permissions: MacPermissions(provider: StaticMacPermissionProvider(
            screenRecordingGranted: granted, accessibilityGranted: granted)),
            trustedControllerStore: trusted ?? TestHostTrustStore(), loadStoredTrust: trusted != nil,
            identityLoader: { HostIdentity(deviceID: Array(0..<16), secretKey: [UInt8](repeating: 9, count: 32)) },
            fixedAccessStore: store, runtimeFactory: { _, onEvent in factory.make(onEvent: onEvent) },
            passwordGenerator: { try HostAccessPassword("ABCD2345") })
    }

    private func waitFor(_ condition: @escaping @MainActor () -> Bool,
        file: StaticString = #filePath, line: UInt = #line) async {
        let deadline = Date().addingTimeInterval(4)
        while !condition(), Date() < deadline { try? await Task.sleep(for: .milliseconds(5)) }
        XCTAssertTrue(condition(), "asynchronous host lifecycle did not settle", file: file, line: line)
    }
}

private final class TestHostFixedStore: HostFixedAccessStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var value: HostAccessPassword?
    var failRead = false
    var failWrite = false

    init(_ value: HostAccessPassword? = nil) { self.value = value }
    func load() throws -> HostAccessPassword? {
        lock.lock(); defer { lock.unlock() }
        if failRead { throw HostAccessCredentialsError.storageUnavailable }
        return value
    }
    func save(_ password: HostAccessPassword) throws {
        lock.lock(); defer { lock.unlock() }
        if failWrite { throw HostAccessCredentialsError.storageUnavailable }
        value = password
    }
    func clear() throws {
        lock.lock(); defer { lock.unlock() }
        if failWrite { throw HostAccessCredentialsError.storageUnavailable }
        value = nil
    }
}

private final class TestHostRuntimeFactory: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [TestHostRuntime] = []
    private var events: [String] = []
    private let blockRegistration: Bool
    private let resultCode: UInt32
    private let fixedExpiry: Date
    private let approveResult: UInt32
    var runtimes: [TestHostRuntime] { lock.withLock { values } }
    var lifecycle: [String] { lock.withLock { events } }

    init(blockRegistration: Bool = false, resultCode: UInt32 = 0,
        fixedExpiry: Date = Date().addingTimeInterval(600), approveResult: UInt32 = 0) {
        self.blockRegistration = blockRegistration
        self.resultCode = resultCode
        self.fixedExpiry = fixedExpiry
        self.approveResult = approveResult
    }
    func make(onEvent: @escaping @Sendable (HostCallbackEvent) -> Void) -> TestHostRuntime {
        let runtime = TestHostRuntime(blockRegistration: blockRegistration,
            resultCode: resultCode, fixedExpiry: fixedExpiry, approveResult: approveResult) { [weak self] event in
                self?.lock.withLock { self?.events.append(event) }
            }
        lock.withLock { values.append(runtime); events.append("create") }
        return runtime
    }
}

private final class TestHostRuntime: HostDirectoryRuntime, @unchecked Sendable {
    var nativeHandle: OpaquePointer? { nil }
    private let lock = NSLock()
    private let gate = DispatchSemaphore(value: 0)
    private let blockRegistration: Bool
    private let resultCode: UInt32
    private let fixedExpiry: Date
    private let record: @Sendable (String) -> Void
    private var storedRequest: HostDirectoryStartRequest?
    private var isDestroyed = false
    private var approvals = 0
    private let approveResult: UInt32
    var approvalCount: Int { lock.withLock { approvals } }
    var request: HostDirectoryStartRequest? { lock.withLock { storedRequest } }
    var registrationStarted: Bool { request != nil }
    var destroyed: Bool { lock.withLock { isDestroyed } }

    init(blockRegistration: Bool, resultCode: UInt32, fixedExpiry: Date, approveResult: UInt32,
        record: @escaping @Sendable (String) -> Void) {
        self.blockRegistration = blockRegistration
        self.resultCode = resultCode
        self.fixedExpiry = fixedExpiry
        self.record = record
        self.approveResult = approveResult
    }
    func register(_ request: HostDirectoryStartRequest) -> HostDirectoryRegistration {
        lock.withLock { storedRequest = request }
        record("register")
        if blockRegistration { _ = gate.wait(timeout: .now() + 8) }
        return HostDirectoryRegistration(resultCode: resultCode,
            expiresAt: request.mode == .fixed ? fixedExpiry : Date().addingTimeInterval(600),
            encodedInvite: Data(repeating: 7, count: Int(DESKLINK_PAIRING_INVITE_BYTES)))
    }
    func approve(controllerDeviceID: [UInt8], controllerVerifyKey: [UInt8]) -> UInt32 {
        lock.withLock { approvals += 1 }
        return approveResult
    }
    func completeRegistration() { gate.signal() }
    func stopAndDestroy() { lock.withLock { isDestroyed = true }; record("destroy") }
}

private final class TestHostTrustStore: HostControllerTrustStoring {
    private var values: [TrustedController]
    private(set) var trustWriteCount = 0
    init(_ value: TrustedController? = nil) { values = value.map { [$0] } ?? [] }
    func list() throws -> [TrustedController] { values }
    func trust(_ controller: TrustedController) throws { trustWriteCount += 1; values.append(controller) }
    func revoke(deviceID: [UInt8]) throws -> Bool {
        let count = values.count
        values.removeAll { $0.deviceID == deviceID }
        return count != values.count
    }
}
