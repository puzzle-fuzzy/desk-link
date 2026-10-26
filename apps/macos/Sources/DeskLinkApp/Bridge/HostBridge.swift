import AppKit
import CoreGraphics
import DeskLinkAppleCore
import DeskLinkC
import Foundation

private final class HostCallbackContext: @unchecked Sendable {
    weak var bridge: HostBridge?
    let generation: UInt64
    let captureAdmission: HostCaptureAdmission
    let onEvent: (@Sendable (HostCallbackEvent) -> Void)?

    init(bridge: HostBridge, generation: UInt64, captureAdmission: HostCaptureAdmission) {
        self.bridge = bridge
        self.generation = generation
        self.captureAdmission = captureAdmission
        self.onEvent = nil
    }

    /// Directory-runtime contexts deliver prepared events through the sharing
    /// closure, which performs its own generation gate and main-actor hop.
    init(onEvent: @escaping @Sendable (HostCallbackEvent) -> Void) {
        self.bridge = nil
        self.generation = 0
        self.captureAdmission = HostCaptureAdmission(runtimeGeneration: 0)
        self.onEvent = onEvent
    }
}

private final class NativeHostDirectoryRuntime: HostDirectoryRuntime, @unchecked Sendable {
    private(set) var nativeHandle: OpaquePointer?
    private var callbackContext: UnsafeMutableRawPointer?

    init(identity: HostIdentity, relayURL: String, serverName: String,
         bridge: HostBridge? = nil, generation: UInt64 = 0, captureAdmission: HostCaptureAdmission? = nil,
         onEvent: (@Sendable (HostCallbackEvent) -> Void)? = nil) throws {
        let callback: HostCallbackContext
        if let bridge, let captureAdmission {
            // The native runtime shares the bridge's admission-gated scheduling.
            callback = HostCallbackContext(bridge: bridge, generation: generation, captureAdmission: captureAdmission)
        } else {
            callback = HostCallbackContext(onEvent: onEvent ?? { _ in })
        }
        let context = Unmanaged.passRetained(callback).toOpaque()
        var handle: OpaquePointer?
        let result = relayURL.withCString { relayURL in
            serverName.withCString { serverName in
                var config = DesklinkHostConfig()
                config.relay_url = relayURL
                config.server_name = serverName
                withUnsafeMutableBytes(of: &config.host_device_id) { $0.copyBytes(from: identity.deviceID) }
                withUnsafeMutableBytes(of: &config.host_secret_key) { $0.copyBytes(from: identity.secretKey) }
                config.log_level = 1
                return desklink_host_create(&config, desklinkHostEventCallback, context, &handle)
            }
        }
        guard result == DESKLINK_OK, let handle else {
            Unmanaged<HostCallbackContext>.fromOpaque(context).release()
            throw HostDirectoryError.runtime(result.rawValue)
        }
        nativeHandle = handle
        callbackContext = context
    }

    func register(_ request: HostDirectoryStartRequest) -> HostDirectoryRegistration {
        guard let handle = nativeHandle else {
            return HostDirectoryRegistration(resultCode: DESKLINK_INVALID_STATE.rawValue,
                expiresAt: .distantPast, encodedInvite: Data())
        }
        let capacity = Int(DESKLINK_PAIRING_INVITE_BYTES)
        var bytes = [UInt8](repeating: 0, count: capacity)
        var length = 0
        var expiry: UInt64 = 0
        var runtimeDeviceNumber: UInt64 = 0
        guard desklink_host_public_device_id(handle, &runtimeDeviceNumber) == DESKLINK_OK,
              runtimeDeviceNumber == request.deviceNumber else {
            return HostDirectoryRegistration(resultCode: DESKLINK_INVALID_ARGUMENT.rawValue,
                expiresAt: .distantPast, encodedInvite: Data())
        }
        let result = request.password.bytes.withUnsafeBytes { password in
            bytes.withUnsafeMutableBytes { bytes in
                desklink_host_start_directory(handle, request.mode == .fixed ? 2 : 1,
                    password.bindMemory(to: UInt8.self).baseAddress, request.password.bytes.count,
                    bytes.bindMemory(to: UInt8.self).baseAddress, capacity, &length, &expiry)
            }
        }
        return HostDirectoryRegistration(resultCode: result.rawValue,
            expiresAt: Date(timeIntervalSince1970: TimeInterval(expiry)),
            encodedInvite: length == capacity ? Data(bytes) : Data())
    }

    func stopAndDestroy() {
        let handle = nativeHandle
        nativeHandle = nil
        if let handle {
            _ = desklink_host_release_all(handle)
            _ = desklink_host_stop(handle)
            desklink_host_destroy(handle)
        }
        if let callbackContext {
            self.callbackContext = nil
            Unmanaged<HostCallbackContext>.fromOpaque(callbackContext).release()
        }
    }

    deinit { stopAndDestroy() }
}

private enum HostDirectoryError: Error {
    case runtime(UInt32)
}

private struct PreparedHostCredentials: Sendable {
    let identity: HostIdentity
    let fixedPassword: HostAccessPassword?
    let fixedPasswordLoadFailed: Bool
}

@MainActor
final class HostBridge: ObservableObject {
    @Published private(set) var state: HostState = .idle
    @Published private(set) var permissions: MacPermissionSnapshot
    @Published private(set) var pairingInvite: HostPairingInvite?
    @Published private(set) var isCreatingInvite = false
    @Published private(set) var pendingApproval: HostApproval?
    @Published private(set) var metrics = HostMetrics()
    @Published private(set) var trustedControllers: [TrustedController] = []
    @Published private(set) var lastError: String?
    @Published private(set) var ignoredHostEventCount: UInt64 = 0
    @Published private(set) var publicDeviceID: String?
    @Published private(set) var accessMode: HostAccessMode = .temporary
    @Published private(set) var sharingStatus: HostSharingStatus = .off
    @Published private(set) var temporaryPassword: String?
    @Published private(set) var temporaryPasswordExpiresAt: Date?
    @Published private(set) var hasFixedPassword = false
    @Published private(set) var isUpdatingCredentials = false
    @Published private(set) var credentialsError: String?

    var canStartCapture: Bool {
        permissions.screenRecording == .granted && state == .connected
    }

    var canInjectInput: Bool {
        permissions.canCaptureAndControl && state == .connected
    }

    var canApprove: Bool {
        permissions.canCaptureAndControl && state == .waitingForApproval && stopTask == nil && pendingApproval != nil
    }

    private let permissionProvider: MacPermissions
    private let relayURL: String
    private let serverName: String
    private let identityLoader: @Sendable () throws -> HostIdentity
    private let fixedAccessStore: any HostFixedAccessStoring
    private let runtimeFactory: HostDirectoryRuntimeFactory?
    private let passwordGenerator: @Sendable () throws -> HostAccessPassword
    private let trustedControllerStore: any HostControllerTrustStoring
    private let captureFactory: HostCaptureFactory
    private var captureAdmission: HostCaptureAdmission?
    private var connectionReceipt: HostCaptureConnectionReceipt?
    private var activeCapture: HostCaptureAttempt?
    private var captureCleanupTask: Task<Void, Never>?
    private var captureWarningOwner: HostCaptureOwner?
    private var runtime: (any HostDirectoryRuntime)?
    private var handle: OpaquePointer? { runtime?.nativeHandle }
    private var inputInjector = MacInputInjector()
    private var captureStarting = false
    private var captureRunning = false
    private var captureGeneration: UInt64 = 0
    private var activeCaptureStreamID: UInt64 = 0
    private var nextStreamID: UInt64 = 0
    private var stopTask: Task<Void, Never>?
    private var registrationTask: Task<HostDirectoryRegistration, Never>?
    private var credentialsLoadTask: Task<PreparedHostCredentials, Error>?
    private var credentialsPrepared = false
    private var credentialsRevision: UInt64 = 0
    private var credentialsLoadGeneration: UInt64 = 0
    private var storedIdentity: HostIdentity?
    private var fixedPassword: HostAccessPassword?
    private var sharingGeneration: UInt64 = 0
    private var sharingRequested = false
    private var registrationConfirmed = false
    private var expiryTask: Task<Void, Never>?
    private var permissionRefreshTask: Task<Void, Never>?
    private var callbackGeneration: UInt64 = 0
    private var activeControllerDeviceID: [UInt8]?
#if DEBUG
    private(set) var releaseAllCallCountForTesting = 0
    var callbackGenerationForTesting: UInt64 { callbackGeneration }

    func observeConnectionForTesting(_ phase: HostCaptureConnectionPhase, mediaGeneration: UInt64,
                                     deliver: Bool = true) -> HostCaptureConnectionReceipt? {
        let raw = testingRawState(phase)
        guard let receipt = observeNativeStateForTesting(raw, mediaGeneration: mediaGeneration) else { return nil }
        if deliver { deliverNativeStateForTesting(raw, receipt: receipt) }
        return receipt
    }

    func deliverConnectionForTesting(_ receipt: HostCaptureConnectionReceipt) {
        deliverNativeStateForTesting(testingRawState(receipt.phase), receipt: receipt)
    }

    private func testingRawState(_ phase: HostCaptureConnectionPhase) -> UInt32 {
        switch phase {
        case .connected: DESKLINK_HOST_CONNECTED.rawValue
        case .unavailable: DESKLINK_HOST_CONNECTING.rawValue
        case .terminal: DESKLINK_HOST_CLOSED.rawValue
        }
    }

    var recordCaptureEventsForTesting = false
    var captureSubmitResultForTesting: UInt32 = DESKLINK_OK.rawValue
    /// Swift-boundary attempts, including those the simulated Rust gate rejects.
    private(set) var admittedVideoEventsForTesting: [OwnedHostVideoEvent] = []
    private var syntheticCaptureForTesting = false
    var activeCaptureOwnerForTesting: HostCaptureOwner? { activeCapture?.owner }
    var captureRunningForTesting: Bool { captureRunning }
    private var inputBackendForTesting: (any CGEventBackend)?
    var recordInputDeliveriesForTesting = false
    private(set) var injectedInputsForTesting: [HostInputDelivery] = []

    func installInputBackendForTesting(_ backend: any CGEventBackend) {
        inputBackendForTesting = backend
        inputInjector = makeInputInjector(displayFrame: .zero)
    }

    /// Uses the same native scheduling path as the real C callback. The return
    /// value observes preparation only; injection still waits for MainActor.
    @discardableResult
    func enqueueNativeInputForTesting(mediaGeneration: UInt64, streamID: UInt64,
                                      inputEpoch: UInt64, pressed: Bool = true) -> HostInputDelivery? {
        var event = DesklinkHostEvent()
        event.kind = DESKLINK_HOST_EVENT_INPUT
        event.state = DESKLINK_HOST_CONNECTED
        event.media_generation = mediaGeneration
        event.input.kind = DESKLINK_INPUT_MOUSE_BUTTON
        event.input.button = 1
        event.input.pressed = pressed ? 1 : 0
        event.input.stream_id = streamID
        event.input.input_epoch = inputEpoch
        return scheduleHostCallbackEvent(callbackContextForTesting(), event)
    }

    func enqueueNativeStateForTesting(_ phase: HostCaptureConnectionPhase, mediaGeneration: UInt64) {
        var event = DesklinkHostEvent()
        event.kind = DESKLINK_HOST_EVENT_STATE
        event.state = DesklinkHostState(rawValue: testingRawState(phase))
        event.media_generation = mediaGeneration
        _ = scheduleHostCallbackEvent(callbackContextForTesting(), event)
    }

    func enqueueNativeReleaseAllForTesting() {
        var event = DesklinkHostEvent()
        event.kind = DESKLINK_HOST_EVENT_RELEASE_ALL
        _ = scheduleHostCallbackEvent(callbackContextForTesting(), event)
    }

    private func callbackContextForTesting() -> HostCallbackContext {
        let admission = captureAdmission ?? HostCaptureAdmission(runtimeGeneration: callbackGeneration)
        captureAdmission = admission
        return HostCallbackContext(bridge: self, generation: callbackGeneration, captureAdmission: admission)
    }
#endif

    init(
        relayURL: String = DeskLinkRuntimeConfiguration.macOSDefaults.relayURL,
        serverName: String = DeskLinkRuntimeConfiguration.macOSDefaults.relayServerName,
        permissions: MacPermissions = MacPermissions(),
        identityStore: HostIdentityStore = HostIdentityStore(),
        trustedControllerStore: any HostControllerTrustStoring = TrustedControllerStore(),
        loadStoredTrust: Bool = true,
        identityLoader: (@Sendable () throws -> HostIdentity)? = nil,
        fixedAccessStore: any HostFixedAccessStoring = HostFixedAccessStore(),
        runtimeFactory: HostDirectoryRuntimeFactory? = nil,
        passwordGenerator: @escaping @Sendable () throws -> HostAccessPassword = { try HostAccessPassword.generate() },
        captureFactory: @escaping HostCaptureFactory = { owner, callbacks in NativeHostCaptureSession(owner: owner, callbacks: callbacks) }
    ) {
        permissionProvider = permissions
        self.identityLoader = identityLoader ?? { try identityStore.loadOrCreate() }
        self.fixedAccessStore = fixedAccessStore
        self.passwordGenerator = passwordGenerator
        self.runtimeFactory = runtimeFactory
        self.relayURL = relayURL
        self.serverName = serverName
        self.trustedControllerStore = trustedControllerStore
        self.captureFactory = captureFactory
        self.permissions = permissions.snapshot()
        if loadStoredTrust { reloadTrustedControllers() }
    }

    static func testing(permissionSnapshot: MacPermissionSnapshot, state: HostState = .idle,
                        captureFactory: @escaping HostCaptureFactory = { _, _ in InertHostCaptureSession() }) -> HostBridge {
        let bridge = HostBridge(permissions: MacPermissions(provider: StaticMacPermissionProvider(
            screenRecordingGranted: permissionSnapshot.screenRecording == .granted,
            accessibilityGranted: permissionSnapshot.accessibility == .granted
        )), loadStoredTrust: false, captureFactory: captureFactory)
        bridge.permissions = permissionSnapshot
        bridge.state = state
#if DEBUG
        bridge.syntheticCaptureForTesting = true
#endif
        return bridge
    }

    /// Identity and fixed-password status can be prepared without enabling capture.
    func prepareAccessCredentials() async {
        guard !credentialsPrepared else { return }
        if credentialsLoadTask == nil {
            credentialsLoadGeneration &+= 1
            let loader = identityLoader
            let store = fixedAccessStore
            credentialsLoadTask = Task.detached(priority: .userInitiated) {
                let identity = try loader()
                do {
                    return PreparedHostCredentials(identity: identity, fixedPassword: try store.load(),
                        fixedPasswordLoadFailed: false)
                } catch {
                    return PreparedHostCredentials(identity: identity, fixedPassword: nil,
                        fixedPasswordLoadFailed: true)
                }
            }
        }
        guard let task = credentialsLoadTask else { return }
        let loadGeneration = credentialsLoadGeneration
        let revision = credentialsRevision
        do {
            let loaded = try await task.value
            publicDeviceID = try HostPublicDeviceID.formatted(for: loaded.identity)
            storedIdentity = loaded.identity
            if revision == credentialsRevision {
                fixedPassword = loaded.fixedPassword
                hasFixedPassword = loaded.fixedPassword != nil
                credentialsPrepared = !loaded.fixedPasswordLoadFailed
                credentialsError = loaded.fixedPasswordLoadFailed
                    ? "无法读取固定密码状态，请检查钥匙串授权或重新设置。" : nil
            }
        } catch {
            credentialsError = "无法读取本机安全凭据，请检查钥匙串授权后重试。"
        }
        if credentialsLoadGeneration == loadGeneration { credentialsLoadTask = nil }
    }

    func start() { startSharing() }

    func startSharing() {
        guard !sharingRequested else { return }
        sharingRequested = true
        sharingGeneration &+= 1
        let generation = sharingGeneration
        Task { @MainActor [weak self] in await self?.replaceSharing(generation: generation) }
    }

    func setAccessMode(_ mode: HostAccessMode) {
        guard accessMode != mode else { return }
        accessMode = mode
        if sharingRequested { restartSharing() }
    }

    func refreshTemporaryPassword() {
        guard accessMode == .temporary else { return }
        if sharingRequested { restartSharing() } else { startSharing() }
    }

    @discardableResult
    func setFixedPassword(_ userInput: String) async -> Bool {
        guard !isUpdatingCredentials else { return false }
        let password: HostAccessPassword
        do { password = try HostAccessPassword(userInput) }
        catch { credentialsError = error.localizedDescription; return false }
        isUpdatingCredentials = true
        defer { isUpdatingCredentials = false }
        await prepareAccessCredentials()
        let store = fixedAccessStore
        do {
            try await Task.detached(priority: .userInitiated) { try store.save(password) }.value
            credentialsRevision &+= 1
            fixedPassword = password
            hasFixedPassword = true
            credentialsPrepared = storedIdentity != nil
            credentialsError = nil
            if sharingRequested && accessMode == .fixed { restartSharing() }
            return true
        } catch {
            credentialsError = "固定密码未保存，请检查钥匙串授权后重试。"
            return false
        }
    }

    @discardableResult
    func removeFixedPassword() async -> Bool {
        guard !isUpdatingCredentials else { return false }
        isUpdatingCredentials = true
        defer { isUpdatingCredentials = false }
        await prepareAccessCredentials()
        let store = fixedAccessStore
        do {
            try await Task.detached(priority: .userInitiated) { try store.clear() }.value
            credentialsRevision &+= 1
            fixedPassword = nil
            hasFixedPassword = false
            credentialsPrepared = storedIdentity != nil
            credentialsError = nil
            if accessMode == .fixed {
                accessMode = .temporary
                if sharingRequested { restartSharing() }
            }
            return true
        } catch {
            credentialsError = "固定密码未移除，请检查钥匙串授权后重试。"
            return false
        }
    }

    private func restartSharing() {
        sharingGeneration &+= 1
        let generation = sharingGeneration
        Task { @MainActor [weak self] in await self?.replaceSharing(generation: generation) }
    }

    private func replaceSharing(generation: UInt64) async {
        await stopRuntimeAndWait()
        guard sharingGeneration == generation, sharingRequested else { return }
        guard permissions.canCaptureAndControl else {
            failSharing("请先允许屏幕录制和辅助功能，再开启本机共享。")
            return
        }
        await prepareAccessCredentials()
        guard sharingGeneration == generation, sharingRequested else { return }
        guard let identity = storedIdentity else {
            failSharing(credentialsError ?? "无法读取本机安全凭据。")
            return
        }
        let password: HostAccessPassword
        do {
            if accessMode == .fixed {
                guard let fixedPassword else { failSharing("请先设置固定密码。"); return }
                password = fixedPassword
            } else {
                password = try passwordGenerator()
            }
            callbackGeneration &+= 1
            let callbackID = callbackGeneration
            let admission = HostCaptureAdmission(runtimeGeneration: callbackID)
            captureAdmission = admission
            connectionReceipt = nil
            let preparedRuntime: any HostDirectoryRuntime
            if let runtimeFactory {
                preparedRuntime = try runtimeFactory(identity) { [weak self] event in
                    Task { @MainActor [weak self] in self?.consume(event, generation: callbackID) }
                }
            } else {
                // The native runtime shares the bridge's callback context, so
                // its events keep the admission-gated scheduling path.
                preparedRuntime = try NativeHostDirectoryRuntime(identity: identity, relayURL: relayURL,
                    serverName: serverName, bridge: self, generation: callbackID, captureAdmission: admission)
            }
            runtime = preparedRuntime
            let mode = accessMode
            let request = HostDirectoryStartRequest(deviceNumber: try HostPublicDeviceID.number(for: identity), password: password, mode: mode)
            state = .connecting
            sharingStatus = .registering
            isCreatingInvite = true
            lastError = nil
            let task = Task.detached(priority: .userInitiated) { preparedRuntime.register(request) }
            registrationTask = task
            let outcome = await task.value
            guard sharingGeneration == generation, callbackGeneration == callbackID,
                  sharingRequested else { return }
            registrationTask = nil
            isCreatingInvite = false
            guard outcome.resultCode == DESKLINK_OK.rawValue,
                  outcome.encodedInvite.count == Int(DESKLINK_PAIRING_INVITE_BYTES),
                  mode == .fixed || outcome.expiresAt > Date() else {
                failSharing("本机共享注册失败，请检查网络后重试。")
                return
            }
            registrationConfirmed = true
            temporaryPassword = mode == .temporary ? password.revealed : nil
            temporaryPasswordExpiresAt = mode == .temporary ? outcome.expiresAt : nil
            pairingInvite = mode == .temporary
                ? HostPairingInvite(expiresAt: outcome.expiresAt, encoded: outcome.encodedInvite) : nil
            if state != .connected { sharingStatus = .ready }
            if mode == .temporary { scheduleRegistrationExpiry(at: outcome.expiresAt, generation: generation) }
        } catch {
            failSharing("无法准备本机共享，请重试。")
        }
    }

    private func failSharing(_ message: String) {
        lastError = message
        sharingStatus = .failed(message)
        sharingRequested = false
        if runtime != nil { stop() }
        else { state = .failed(message) }
    }

    private func scheduleRegistrationExpiry(at expiry: Date, generation: UInt64) {
        expiryTask?.cancel()
        expiryTask = Task { @MainActor [weak self] in
            let delay = max(0, expiry.timeIntervalSinceNow)
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled, let self, sharingGeneration == generation else { return }
            // An established session can finish normally, but the invitation must
            // never remain visible after its actual relay expiry.
            temporaryPassword = nil
            temporaryPasswordExpiresAt = nil
            pairingInvite = nil
            if state != .connected { stop() }
        }
    }

    func refreshPermissions() {
        applyPermissions(permissionProvider.snapshot())
    }

    func requestScreenRecording() {
        applyPermissions(permissionProvider.requestScreenRecording())
        schedulePermissionRefresh()
    }

    func requestAccessibility() {
        applyPermissions(permissionProvider.requestAccessibility())
        schedulePermissionRefresh()
    }

    func createInvite() {
        if accessMode != .temporary {
            setAccessMode(.temporary)
            if !sharingRequested { startSharing() }
        } else if sharingRequested {
            if pairingInvite == nil { refreshTemporaryPassword() }
        } else {
            startSharing()
        }
    }

    func approve() {
        guard var approval = pendingApproval, canApprove, let runtime else { return }
        approval.isApproved = true
        let result = runtime.approve(controllerDeviceID: approval.controllerDeviceID,
            controllerVerifyKey: approval.controllerVerifyKey)
        guard result == DESKLINK_OK.rawValue else {
            publishResultError("The controller could not be approved.", result: DesklinkResult(rawValue: result))
            return
        }
        pendingApproval = nil
        activeControllerDeviceID = approval.controllerDeviceID
        do {
            try trustedControllerStore.trust(TrustedController(
                deviceID: approval.controllerDeviceID,
                verifyKey: approval.controllerVerifyKey,
                approvedAtUnixSeconds: UInt64(Date().timeIntervalSince1970),
                displayName: approval.fingerprint
            ))
            reloadTrustedControllers()
        } catch let error as TrustedControllerStoreError where error == .duplicateRecord {
            reloadTrustedControllers()
        } catch {
            publishError("The controller was approved, but its trust record could not be saved.")
        }
    }

    func reject() {
        guard let handle = handle else { return }
        let result = desklink_host_reject(handle)
        guard result == DESKLINK_OK else {
            publishResultError("The controller could not be rejected.", result: result)
            return
        }
        pendingApproval = nil
    }

    func revoke(controller: TrustedController) {
        let revokingActiveController = activeControllerDeviceID == controller.deviceID
        if revokingActiveController {
            pendingApproval = nil
            stop()
        }
        let pendingStop = stopTask
        Task { @MainActor [weak self] in
            guard let self else { return }
            if let pendingStop { await pendingStop.value }
            do {
                _ = try trustedControllerStore.revoke(deviceID: controller.deviceID)
                reloadTrustedControllers()
            } catch {
                publishError("The trusted controller could not be revoked.")
            }
        }
    }

    func stop() {
        sharingRequested = false
        sharingGeneration &+= 1
        guard stopTask == nil else { return }
        beginStop()
        stopTask = Task { @MainActor [weak self] in await self?.performStop() }
    }

    func shutdown() {
        stop()
    }

    func shutdownAndWait() async {
        sharingRequested = false
        sharingGeneration &+= 1
        await stopRuntimeAndWait()
    }

    private func stopRuntimeAndWait() async {
        if let stopTask {
            await stopTask.value
            return
        }
        beginStop()
        let task = Task<Void, Never> { @MainActor [weak self] in await self?.performStop() }
        stopTask = task
        await task.value
    }

    private func beginStop() {
        captureAdmission?.close()
        retireActiveCapture()
        connectionReceipt = nil
        callbackGeneration &+= 1
        captureGeneration &+= 1
        activeCaptureStreamID = 0
        activeControllerDeviceID = nil
        pendingApproval = nil
        releaseAllLocalInput()
        state = .stopping
        sharingStatus = .stopping
        registrationConfirmed = false
        temporaryPassword = nil
        temporaryPasswordExpiresAt = nil
        pairingInvite = nil
        expiryTask?.cancel()
        expiryTask = nil
    }

    private func performStop() async {
        if let registrationTask {
            _ = await registrationTask.value
            self.registrationTask = nil
        }
        isCreatingInvite = false
        if let captureCleanupTask { await captureCleanupTask.value }
        let retiringRuntime = runtime
        runtime = nil
        if let retiringRuntime {
            await Task.detached(priority: .userInitiated) { retiringRuntime.stopAndDestroy() }.value
        }
        captureAdmission = nil
        releaseAllLocalInput()
        pairingInvite = nil
        pendingApproval = nil
        state = .closed
        sharingStatus = lastError.map { .failed($0) } ?? .off
        stopTask = nil
    }

    fileprivate func consume(_ event: HostCallbackEvent, generation: UInt64) {
        guard acceptsCallback(generation: generation) else { return }
        switch event.kind {
        case .state:
            consumeState(event.state, receipt: event.connectionReceipt)
        case .error:
            publishError(event.message ?? "The host runtime reported an error.")
        case .approvalRequested:
            retireActiveCapture()
            pendingApproval = HostApproval(
                id: UUID(),
                fingerprint: event.fingerprint ?? "Unverified controller",
                controllerDeviceID: event.controllerDeviceID,
                controllerVerifyKey: event.controllerVerifyKey
            )
            state = .waitingForApproval
            approveTrustedControllerIfEligible()
        case .input:
            guard let input = event.inputDelivery else { return }
            inject(input)
        case .keyframeRequested:
            if let capture = activeCapture, capture.admission.allows(capture.owner) { capture.session.requestKeyframe() }
        case .releaseAll:
            releaseAllLocalInput()
        case .metrics:
            if let metrics = event.metrics { self.metrics = metrics }
        case .transfer:
            lastError = "当前 Mac 共享尚不支持文件和剪贴板传输。"
        case .unknown:
            ignoredHostEventCount &+= 1
        }
    }

    private func approveTrustedControllerIfEligible() {
        guard canApprove, let approval = pendingApproval, let runtime,
              approval.controllerDeviceID.count == TrustedController.deviceIDLength,
              approval.controllerVerifyKey.count == TrustedController.verifyKeyLength,
              trustedControllers.contains(where: {
                  $0.deviceID == approval.controllerDeviceID && $0.verifyKey == approval.controllerVerifyKey
              }) else { return }
        let result = runtime.approve(controllerDeviceID: approval.controllerDeviceID,
            controllerVerifyKey: approval.controllerVerifyKey)
        guard result == DESKLINK_OK.rawValue else {
            lastError = "无法恢复已批准设备的连接，请在此 Mac 上重新确认。"
            return
        }
        activeControllerDeviceID = approval.controllerDeviceID
        pendingApproval = nil
        lastError = nil
    }

#if DEBUG
    /// Models only native metadata and task ordering; never creates a Rust host.
    func observeNativeStateForTesting(_ rawState: UInt32, mediaGeneration: UInt64) -> HostCaptureConnectionReceipt? {
        if captureAdmission == nil { captureAdmission = HostCaptureAdmission(runtimeGeneration: callbackGeneration) }
        return captureAdmission?.observe(captureConnectionPhase(Int(rawState)), mediaGeneration: mediaGeneration)
    }

    func deliverNativeStateForTesting(_ rawState: UInt32, receipt: HostCaptureConnectionReceipt) {
        consumeState(Int(rawState), receipt: receipt)
    }

    func waitForCaptureStartForTesting() async {
        let starting = activeCapture?.startTask
        await starting?.value
    }

    func waitForCaptureCleanupForTesting() async { await captureCleanupTask?.value }

    func flushCaptureCallbacksForTesting() async {
        await withCheckedContinuation { continuation in
            DispatchQueue.main.async { continuation.resume() }
        }
    }

    /// Exercises event classification only; no native host or input injection.
    func acceptEventKindForTesting(_ rawValue: UInt32, poisonUnrelatedFields: Bool = false) {
        var value = DesklinkHostEvent()
        value.kind = DesklinkHostEventKind(rawValue: rawValue)
        value.state = DESKLINK_HOST_CONNECTED
        if poisonUnrelatedFields {
            value.data_len = Int.max
            value.fingerprint_len = Int.max
            value.metrics.sent_video_configs = UInt64.max
            value.metrics.sent_video_packets = UInt64.max
            value.metrics.received_input_events = UInt64.max
            value.metrics.keyframe_requests = UInt64.max
        }
        consume(hostCallbackEvent(value), generation: callbackGeneration)
    }
#endif

    private func consumeState(_ rawState: Int, receipt: HostCaptureConnectionReceipt?) {
        if let receipt {
            guard let admission = captureAdmission,
                  receipt.runtimeGeneration == callbackGeneration, admission.matches(receipt) else { return }
            connectionReceipt = receipt
            if receipt.phase != .connected { retireActiveCapture() }
        }
        // Directory-runtime and synthetic events carry no native receipt; their
        // callback generation was already gated before the main-actor hop.
        switch rawState {
        case Int(DESKLINK_HOST_CONNECTING.rawValue):
            state = .connecting
            if sharingRequested { sharingStatus = .registering }
            releaseAllLocalInput()
        case Int(DESKLINK_HOST_WAITING_FOR_APPROVAL.rawValue):
            state = .waitingForApproval
            if registrationConfirmed { sharingStatus = .ready }
        case Int(DESKLINK_HOST_NEGOTIATING_CAPABILITIES.rawValue):
            state = .negotiating
            if sharingRequested { sharingStatus = .registering }
            releaseAllLocalInput()
        case Int(DESKLINK_HOST_CONNECTED.rawValue):
            // Connecting may have been superseded while its UI delivery waited.
            // The native gate already retired that connection's capture.
            if let current = activeCapture, captureAdmission?.allows(current.owner) != true { retireActiveCapture() }
            state = .connected
            sharingStatus = .connected
            pendingApproval = nil
            if captureWarningOwner != nil {
                captureWarningOwner = nil
                lastError = nil
            }
            startCaptureIfPermitted()
        case Int(DESKLINK_HOST_STOPPING.rawValue): state = .stopping
        case Int(DESKLINK_HOST_CLOSED.rawValue):
            state = .closed
            if stopTask == nil { stop() }
        default: break
        }
    }

    private func startCaptureIfPermitted() {
        guard canStartCapture, activeCapture == nil, !captureStarting, !captureRunning,
              let admission = captureAdmission, let receipt = connectionReceipt,
              receipt.phase == .connected, admission.matches(receipt) else { return }
        guard nextStreamID < UInt64.max else {
            publishError("The capture stream sequence is exhausted. Restart host mode.")
            return
        }
        // Burn the identifier when reserving, including failed/cancelled starts.
        nextStreamID += 1
        guard let owner = admission.register(receipt, streamID: nextStreamID) else { return }
        let callbacks = HostCaptureCallbacks(
            video: { [weak self, owner, admission] event in
                DispatchQueue.main.async { [weak self, owner, admission] in
                    self?.sendVideo(event, owner: owner, admission: admission)
                }
            },
            encodingError: { [weak self, owner, admission] error in
                DispatchQueue.main.async { [weak self, owner, admission] in
                    self?.encodingFailed(error, owner: owner, admission: admission)
                }
            },
            captureStopped: { [weak self, owner, admission] message in
                DispatchQueue.main.async { [weak self, owner, admission] in
                    self?.captureStoppedUnexpectedly(owner: owner, admission: admission, message: message)
                }
            }
        )
        let attempt = HostCaptureAttempt(owner: owner, admission: admission, session: captureFactory(owner, callbacks))
        activeCapture = attempt
        captureStarting = true
        let previousCleanup = captureCleanupTask
        attempt.startTask = Task { @MainActor [weak self, attempt, previousCleanup] in
            if let previousCleanup { await previousCleanup.value }
            guard let self else { return }
            guard !Task.isCancelled, isCurrentCapture(attempt.owner, admission: attempt.admission) else {
                retireCaptureIfOwned(attempt.owner)
                return
            }
            do {
                let geometry = try await attempt.session.start()
                guard !Task.isCancelled, isCurrentCapture(attempt.owner, admission: attempt.admission) else {
                    retireCaptureIfOwned(attempt.owner)
                    return
                }
                inputInjector = makeInputInjector(displayFrame: geometry.displayFrame)
                captureRunning = true
                captureStarting = false
                if captureWarningOwner != nil {
                    captureWarningOwner = nil
                    lastError = nil
                }
            } catch {
                guard isCurrentCapture(attempt.owner, admission: attempt.admission) else {
                    retireCaptureIfOwned(attempt.owner)
                    return
                }
                // Retirement cancels this task, so preserve its incoming
                // cancellation state before cancelling our own resource owner.
                let wasCancelled = error is CancellationError || Task.isCancelled
                retireActiveCapture()
                if wasCancelled { return }
                captureWarningOwner = attempt.owner
                lastError = "Screen capture could not start. Check Screen Recording permission and refresh permissions to retry."
            }
        }
    }

    private func isCurrentCapture(_ owner: HostCaptureOwner, admission: HostCaptureAdmission) -> Bool {
        state == .connected && owner.runtimeGeneration == callbackGeneration
            && captureAdmission === admission && activeCapture?.owner == owner && admission.allows(owner)
    }

    private func retireCaptureIfOwned(_ owner: HostCaptureOwner) {
        if activeCapture?.owner == owner { retireActiveCapture() }
    }

    private func retireActiveCapture() {
        guard let attempt = activeCapture else { return }
        activeCapture = nil
        captureStarting = false
        captureRunning = false
        attempt.admission.retire(attempt.owner)
        attempt.startTask?.cancel()
        releaseAllLocalInput()
        let previousCleanup = captureCleanupTask
        let starting = attempt.startTask
        captureCleanupTask = Task { @MainActor [attempt, previousCleanup, starting] in
            if let previousCleanup { await previousCleanup.value }
            if let starting { await starting.value }
            await attempt.session.stop()
        }
    }

    private func captureStoppedUnexpectedly(owner: HostCaptureOwner, admission: HostCaptureAdmission, message: String) {
        guard isCurrentCapture(owner, admission: admission) else { return }
        retireActiveCapture()
        captureWarningOwner = owner
        lastError = "Screen capture stopped unexpectedly. Refresh permissions or restart sharing to retry."
    }

    private func encodingFailed(_ error: MacH264EncoderError, owner: HostCaptureOwner, admission: HostCaptureAdmission) {
        guard isCurrentCapture(owner, admission: admission) else { return }
        retireActiveCapture()
        captureWarningOwner = owner
        lastError = Self.redact("Video encoding could not continue: \(error). Refresh permissions or restart sharing to retry.")
    }

    private func applyPermissions(_ snapshot: MacPermissionSnapshot) {
        let previous = permissions
        permissions = snapshot
        if previous.screenRecording == .granted, snapshot.screenRecording != .granted {
            retireActiveCapture()
            if state == .connected { stop() }
        }
        if previous.accessibility == .granted, snapshot.accessibility != .granted {
            releaseAllLocalInput()
            if state == .connected { stop() }
        }
        if previous.canCaptureAndControl && !snapshot.canCaptureAndControl,
           sharingRequested || runtime != nil {
            stop()
        }
        startCaptureIfPermitted()
    }

    private func schedulePermissionRefresh() {
        permissionRefreshTask?.cancel()
        permissionRefreshTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 500_000_000)
            guard !Task.isCancelled else { return }
            self?.refreshPermissions()
        }
    }

    private func sendVideo(_ event: EncodedVideoEvent, owner: HostCaptureOwner, admission: HostCaptureAdmission) {
        guard isCurrentCapture(owner, admission: admission) else { return }
        let eventStream: UInt64
        switch event {
        case let .configuration(streamID, _, _, _, _): eventStream = streamID
        case let .accessUnit(streamID, _, _, _, _): eventStream = streamID
        }
        guard eventStream == owner.streamID else { return }
        let result: DesklinkResult? = admission.withAdmission(owner) {
#if DEBUG
            if recordCaptureEventsForTesting { admittedVideoEventsForTesting.append(OwnedHostVideoEvent(owner: owner, event: event)) }
#endif
            guard let handle = runtime?.nativeHandle else {
#if DEBUG
                if syntheticCaptureForTesting { return DesklinkResult(rawValue: captureSubmitResultForTesting) }
#endif
                return DESKLINK_INVALID_STATE
            }
            switch event {
            case let .configuration(streamID, version, width, height, bytes):
                return bytes.withUnsafeBytes {
                    desklink_host_send_video_config_for_generation(handle, owner.mediaGeneration, streamID, version, width, height,
                        $0.bindMemory(to: UInt8.self).baseAddress, bytes.count)
                }
            case let .accessUnit(streamID, frameID, configVersion, _, bytes):
                return bytes.withUnsafeBytes {
                    desklink_host_send_video_access_unit_for_generation(handle, owner.mediaGeneration, streamID, frameID, configVersion,
                        $0.bindMemory(to: UInt8.self).baseAddress, bytes.count)
                }
            }
        }
        guard let result, result != DESKLINK_STALE_GENERATION,
              isCurrentCapture(owner, admission: admission) else { return }
        if result != DESKLINK_OK {
            retireActiveCapture()
            captureWarningOwner = owner
            lastError = "Video delivery could not continue (code \(result.rawValue)). Refresh permissions or restart sharing to retry."
        }
    }

    private func releaseAllLocalInput() {
#if DEBUG
        releaseAllCallCountForTesting += 1
#endif
        _ = inputInjector.releaseAll()
    }

    private func makeInputInjector(displayFrame: CGRect) -> MacInputInjector {
#if DEBUG
        if let inputBackendForTesting { return MacInputInjector(backend: inputBackendForTesting, displayFrame: displayFrame) }
#endif
        return MacInputInjector(displayFrame: displayFrame)
    }

    private func inject(_ input: HostInputDelivery) {
        guard canInjectInput, captureRunning, captureAdmission === input.admission,
              activeCapture?.owner == input.captureOwner,
              input.captureOwner.runtimeGeneration == callbackGeneration else { return }
        let outcome: Result<Void, Error>? = input.admission.withInputAdmission(input) {
            Result {
                try inputInjector.inject(input.command)
#if DEBUG
                if recordInputDeliveriesForTesting { injectedInputsForTesting.append(input) }
#endif
            }
        }
        if case .failure? = outcome, isCurrentCapture(input.captureOwner, admission: input.admission) {
            publishError("Remote input could not be applied.")
        }
    }

    private func reloadTrustedControllers() {
        do {
            trustedControllers = try trustedControllerStore.list()
        } catch {
            trustedControllers = []
        }
    }

    private func publishResultError(_ message: String, result: DesklinkResult) {
        publishError("\(message) (code \(result.rawValue)).")
    }

    private func publishError(_ message: String) {
        let safe = Self.redact(message)
        lastError = safe
        state = .failed(safe)
        sharingStatus = .failed(safe)
        if runtime != nil, stopTask == nil { stop() }
        else { sharingRequested = false }
    }

    private static func redact(_ message: String) -> String {
        let lowercased = message.lowercased()
        if lowercased.contains("auth") || lowercased.contains("secret") || lowercased.contains("key") || lowercased.contains("token") {
            return "A secure host connection error occurred."
        }
        return message
    }

#if DEBUG
    func acceptEventForTesting(kind: UInt32, state: UInt32 = 0, generation: UInt64? = nil,
        controllerDeviceID: [UInt8] = [], controllerVerifyKey: [UInt8] = []) {
        consume(HostCallbackEvent(
            kind: hostCallbackKind(DesklinkHostEventKind(rawValue: kind)),
            state: Int(state), message: nil, fingerprint: nil,
            controllerDeviceID: controllerDeviceID, controllerVerifyKey: controllerVerifyKey,
            inputDelivery: nil, metrics: nil, connectionReceipt: nil
        ), generation: generation ?? callbackGeneration)
    }
#endif
}

enum HostCallbackKind: Sendable {
    case state
    case error
    case approvalRequested
    case input
    case keyframeRequested
    case releaseAll
    case metrics
    case transfer
    case unknown
}

struct HostCallbackEvent: Sendable {
    let kind: HostCallbackKind
    let state: Int
    let message: String?
    let fingerprint: String?
    let controllerDeviceID: [UInt8]
    let controllerVerifyKey: [UInt8]
    let inputDelivery: HostInputDelivery?
    let metrics: HostMetrics?
    let connectionReceipt: HostCaptureConnectionReceipt?
}

private func desklinkHostEventCallback(_ context: UnsafeMutableRawPointer?, _ event: UnsafePointer<DesklinkHostEvent>?) {
    guard let context, let event else { return }
    let callbackContext = Unmanaged<HostCallbackContext>.fromOpaque(context).takeUnretainedValue()
    _ = scheduleHostCallbackEvent(callbackContext, event.pointee)
}

/// Both C callbacks and synthetic queue regressions enter this preparation path.
/// All original Rust fields are copied before scheduling any MainActor work.
private func scheduleHostCallbackEvent(_ callbackContext: HostCallbackContext, _ value: DesklinkHostEvent) -> HostInputDelivery? {
    guard let bridge = callbackContext.bridge else {
        // Runtimes injected by tests keep the sharing closure's routing; that
        // closure performs its own generation gate and main-actor hop.
        callbackContext.onEvent?(hostCallbackEvent(value))
        return nil
    }
    let generation = callbackContext.generation
    let kind = hostCallbackKind(value.kind)
    let receipt: HostCaptureConnectionReceipt?
    let inputDelivery: HostInputDelivery?
    switch kind {
    case .state:
        // Retire encoded deliveries before waiting for the MainActor. The
        // immutable Rust generation remains attached to the connection receipt.
        guard let observed = callbackContext.captureAdmission.observe(
            captureConnectionPhase(Int(value.state.rawValue)), mediaGeneration: value.media_generation
        ) else { return nil }
        receipt = observed
        inputDelivery = nil
    case .approvalRequested:
        guard let observed = callbackContext.captureAdmission.observe(.unavailable, mediaGeneration: value.media_generation) else { return nil }
        receipt = observed
        inputDelivery = nil
    case .input:
        guard let command = hostInputCommand(value.input),
              let input = callbackContext.captureAdmission.captureInput(
                mediaGeneration: value.media_generation,
                context: HostInputContext(streamID: value.input.stream_id, inputEpoch: value.input.input_epoch),
                command: command
              ) else { return nil }
        inputDelivery = input
        receipt = input.connectionReceipt
    default:
        receipt = nil
        inputDelivery = nil
    }
    let mapped = hostCallbackEvent(value, connectionReceipt: receipt, inputDelivery: inputDelivery)
    DispatchQueue.main.async {
        guard bridge.acceptsCallback(generation: generation) else { return }
        if let receipt, !callbackContext.captureAdmission.matches(receipt) { return }
        bridge.consume(mapped, generation: generation)
    }
    return inputDelivery
}

private func hostCallbackEvent(_ value: DesklinkHostEvent, connectionReceipt: HostCaptureConnectionReceipt? = nil,
                               inputDelivery: HostInputDelivery? = nil) -> HostCallbackEvent {
    let kind = hostCallbackKind(value.kind)
    let message: String?
    if case .error = kind {
        message = value.data.map { String(decoding: UnsafeBufferPointer(start: $0, count: value.data_len), as: UTF8.self) }
    } else {
        // Unknown/transfer bytes need not be copied or interpreted as errors.
        message = nil
    }
    let fingerprint: String?
    if case .approvalRequested = kind {
        fingerprint = value.fingerprint.map { String(decoding: UnsafeBufferPointer(start: $0, count: value.fingerprint_len), as: UTF8.self) }
    } else { fingerprint = nil }
    let metrics: HostMetrics?
    if case .metrics = kind {
        metrics = HostMetrics(sentVideoConfigurations: Int(clamping: value.metrics.sent_video_configs),
            sentVideoPackets: Int(clamping: value.metrics.sent_video_packets),
            receivedInputEvents: Int(clamping: value.metrics.received_input_events),
            keyframeRequests: Int(clamping: value.metrics.keyframe_requests))
    } else { metrics = nil }
    return HostCallbackEvent(
        kind: kind,
        state: Int(value.state.rawValue),
        message: message,
        fingerprint: fingerprint,
        controllerDeviceID: withUnsafeBytes(of: value.controller_device_id) { Array($0) },
        controllerVerifyKey: withUnsafeBytes(of: value.controller_verify_key) { Array($0) },
        inputDelivery: inputDelivery,
        metrics: metrics,
        connectionReceipt: connectionReceipt
    )
}

private func captureConnectionPhase(_ state: Int) -> HostCaptureConnectionPhase {
    switch state {
    case Int(DESKLINK_HOST_CONNECTED.rawValue): .connected
    case Int(DESKLINK_HOST_STOPPING.rawValue), Int(DESKLINK_HOST_CLOSED.rawValue): .terminal
    default: .unavailable
    }
}

private extension HostBridge {
    func acceptsCallback(generation: UInt64) -> Bool {
        callbackGeneration == generation
    }
}

private func hostCallbackKind(_ kind: DesklinkHostEventKind) -> HostCallbackKind {
    switch kind {
    case DESKLINK_HOST_EVENT_STATE: .state
    case DESKLINK_HOST_EVENT_ERROR: .error
    case DESKLINK_HOST_EVENT_APPROVAL_REQUESTED: .approvalRequested
    case DESKLINK_HOST_EVENT_INPUT: .input
    case DESKLINK_HOST_EVENT_KEYFRAME_REQUESTED: .keyframeRequested
    case DESKLINK_HOST_EVENT_RELEASE_ALL: .releaseAll
    case DESKLINK_HOST_EVENT_METRICS: .metrics
    case DESKLINK_HOST_EVENT_TRANSFER: .transfer
    default: .unknown
    }
}

private func hostInputCommand(_ input: DesklinkHostInput) -> RemoteInputCommand? {
    switch input.kind {
    case DESKLINK_INPUT_MOUSE_MOVE:
        return .move(normalizedX: input.x, normalizedY: input.y)
    case DESKLINK_INPUT_MOUSE_BUTTON:
        let button: MouseButton = switch input.button {
        case 1: .left
        case 2: .right
        default: .center
        }
        return .mouseButton(button, pressed: input.pressed != 0)
    case DESKLINK_INPUT_MOUSE_WHEEL:
        return .wheel(deltaX: input.wheel_x, deltaY: input.wheel_y)
    case DESKLINK_INPUT_KEY:
        let modifiers = Modifiers(rawValue: UInt32(input.modifiers))
        if input.character != 0, UnicodeScalar(input.character) != nil {
            return .character(input.character, pressed: input.pressed != 0, modifiers: modifiers)
        }
        guard let virtualKeyCode = MacKeyCodeMapper.appKitKeyCode(forProtocolCode: input.key_code) else { return nil }
        return .key(code: virtualKeyCode, pressed: input.pressed != 0, modifiers: modifiers)
    default:
        return nil
    }
}
