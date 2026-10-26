import AppKit
import CoreGraphics
import DeskLinkAppleCore
import DeskLinkC
import Foundation

private final class HostCallbackContext: @unchecked Sendable {
    weak var bridge: HostBridge?
    let generation: UInt64
    let captureAdmission: HostCaptureAdmission

    init(bridge: HostBridge, generation: UInt64, captureAdmission: HostCaptureAdmission) {
        self.bridge = bridge
        self.generation = generation
        self.captureAdmission = captureAdmission
    }
}

private final class HostHandleOwner: @unchecked Sendable {
    var pointer: OpaquePointer?
    var callbackContext: UnsafeMutableRawPointer?

    func destroy() {
        let pointer = self.pointer
        self.pointer = nil
        if let pointer { desklink_host_destroy(pointer) }
        if let callbackContext {
            self.callbackContext = nil
            Unmanaged<HostCallbackContext>.fromOpaque(callbackContext).release()
        }
    }

    deinit { destroy() }
}

private struct PairingStartResult: Sendable {
    let resultCode: UInt32
    let length: Int
    let expiry: UInt64
    let bytes: Data
}

private struct HostHandleReference: @unchecked Sendable {
    let pointer: OpaquePointer
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

    var canStartCapture: Bool {
        permissions.screenRecording == .granted && state == .connected
    }

    var canInjectInput: Bool {
        permissions.canCaptureAndControl && state == .connected
    }

    var canApprove: Bool {
        permissions.canCaptureAndControl && pendingApproval != nil
    }

    private let relayURL: String
    private let serverName: String
    private let permissionProvider: MacPermissions
    private let identityStore: HostIdentityStore
    private let trustedControllerStore: TrustedControllerStore
    private let handleOwner = HostHandleOwner()
    private let captureFactory: HostCaptureFactory
    private var captureAdmission: HostCaptureAdmission?
    private var connectionReceipt: HostCaptureConnectionReceipt?
    private var activeCapture: HostCaptureAttempt?
    private var captureCleanupTask: Task<Void, Never>?
    private var captureWarningOwner: HostCaptureOwner?
    private var inputInjector = MacInputInjector()
    private var captureStarting = false
    private var captureRunning = false
    private var nextStreamID: UInt64 = 0
    private var stopTask: Task<Void, Never>?
    private var pairingTask: Task<PairingStartResult, Never>?
    private var permissionRefreshTask: Task<Void, Never>?
    private var callbackGeneration: UInt64 = 0
    private var activeControllerDeviceID: [UInt8]?
#if DEBUG
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
#endif

    init(
        relayURL: String = DeskLinkRuntimeConfiguration.macOSDefaults.relayURL,
        serverName: String = DeskLinkRuntimeConfiguration.macOSDefaults.relayServerName,
        permissions: MacPermissions = MacPermissions(),
        identityStore: HostIdentityStore = HostIdentityStore(),
        trustedControllerStore: TrustedControllerStore = TrustedControllerStore(),
        captureFactory: @escaping HostCaptureFactory = { owner, callbacks in NativeHostCaptureSession(owner: owner, callbacks: callbacks) },
        loadTrustedControllers: Bool = true
    ) {
        self.relayURL = relayURL
        self.serverName = serverName
        permissionProvider = permissions
        self.identityStore = identityStore
        self.trustedControllerStore = trustedControllerStore
        self.captureFactory = captureFactory
        self.permissions = permissions.snapshot()
        if loadTrustedControllers { reloadTrustedControllers() }
    }

    static func testing(permissionSnapshot: MacPermissionSnapshot,
                        captureFactory: @escaping HostCaptureFactory = { _, _ in InertHostCaptureSession() }) -> HostBridge {
        let bridge = HostBridge(permissions: MacPermissions(provider: StaticMacPermissionProvider(
            screenRecordingGranted: permissionSnapshot.screenRecording == .granted,
            accessibilityGranted: permissionSnapshot.accessibility == .granted
        )), captureFactory: captureFactory, loadTrustedControllers: false)
        bridge.permissions = permissionSnapshot
        bridge.state = .idle
#if DEBUG
        bridge.syntheticCaptureForTesting = true
#endif
        return bridge
    }

    func start() {
        guard handleOwner.pointer == nil else { return }
        do {
            let identity = try identityStore.loadOrCreate()
            callbackGeneration &+= 1
            let admission = HostCaptureAdmission(runtimeGeneration: callbackGeneration)
            captureAdmission = admission
            connectionReceipt = nil
            let context = HostCallbackContext(bridge: self, generation: callbackGeneration, captureAdmission: admission)
            let contextPointer = Unmanaged.passRetained(context).toOpaque()
            var handle: OpaquePointer?
            let result = relayURL.withCString { relayURL in
                serverName.withCString { serverName in
                    var config = DesklinkHostConfig()
                    config.relay_url = relayURL
                    config.server_name = serverName
                    withUnsafeMutableBytes(of: &config.host_device_id) { $0.copyBytes(from: identity.deviceID) }
                    withUnsafeMutableBytes(of: &config.host_secret_key) { $0.copyBytes(from: identity.secretKey) }
                    config.log_level = 1
                    return desklink_host_create(&config, desklinkHostEventCallback, contextPointer, &handle)
                }
            }
            guard result == DESKLINK_OK, let handle else {
                Unmanaged<HostCallbackContext>.fromOpaque(contextPointer).release()
                publishResultError("DeskLink could not prepare host mode.", result: result)
                return
            }
            handleOwner.pointer = handle
            handleOwner.callbackContext = contextPointer
            state = .idle
            lastError = nil
        } catch {
            publishError(error.localizedDescription)
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
        guard permissions.canCaptureAndControl, !isCreatingInvite else {
            if !permissions.canCaptureAndControl {
                publishError("Grant Screen Recording and Accessibility before creating an invitation.")
            }
            return
        }
        start()
        guard let handle = handleOwner.pointer else { return }
        let capacity = Int(DESKLINK_PAIRING_INVITE_BYTES)
        let handleReference = HostHandleReference(pointer: handle)
        let generation = callbackGeneration
        isCreatingInvite = true
        pairingInvite = nil
        state = .connecting
        lastError = nil
        pairingTask = Task.detached(priority: .userInitiated) {
            var bytes = [UInt8](repeating: 0, count: capacity)
            var length = 0
            var expiry: UInt64 = 0
            let result = bytes.withUnsafeMutableBytes {
                desklink_host_start_pairing(
                    handleReference.pointer,
                    $0.bindMemory(to: UInt8.self).baseAddress,
                    capacity,
                    &length,
                    &expiry
                )
            }
            return PairingStartResult(
                resultCode: result.rawValue,
                length: length,
                expiry: expiry,
                bytes: Data(bytes)
            )
        }
        Task { @MainActor [weak self] in
            guard let self, let pairingTask = self.pairingTask else { return }
            let outcome = await pairingTask.value
            self.pairingTask = nil
            self.isCreatingInvite = false
            guard self.callbackGeneration == generation, self.handleOwner.pointer == handle else { return }
            guard outcome.resultCode == DESKLINK_OK.rawValue, outcome.length == capacity else {
                self.publishResultError(
                    "The relay could not start the host invitation.",
                    result: DesklinkResult(rawValue: outcome.resultCode)
                )
                return
            }
            self.pairingInvite = HostPairingInvite(
                expiresAt: Date(timeIntervalSince1970: TimeInterval(outcome.expiry)),
                encoded: outcome.bytes
            )
            self.state = .connecting
            self.lastError = nil
        }
    }

    func approve() {
        guard var approval = pendingApproval, canApprove, let handle = handleOwner.pointer else { return }
        approval.isApproved = true
        let result = approval.controllerDeviceID.withUnsafeBytes { deviceID in
            approval.controllerVerifyKey.withUnsafeBytes { verifyKey in
                desklink_host_approve(handle, deviceID.bindMemory(to: UInt8.self).baseAddress, verifyKey.bindMemory(to: UInt8.self).baseAddress)
            }
        }
        guard result == DESKLINK_OK else {
            publishResultError("The controller could not be approved.", result: result)
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
        guard let handle = handleOwner.pointer else { return }
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
        guard stopTask == nil else { return }
        beginStop()
        stopTask = Task { @MainActor [weak self] in await self?.performStop() }
    }

    func shutdown() {
        stop()
    }

    func shutdownAndWait() async {
        if let stopTask {
            await stopTask.value
        } else {
            beginStop()
            await performStop()
        }
    }

    private func beginStop() {
        captureAdmission?.close()
        retireActiveCapture()
        connectionReceipt = nil
        callbackGeneration &+= 1
        activeControllerDeviceID = nil
        state = .stopping
    }

    private func performStop() async {
        if let pairingTask {
            _ = await pairingTask.value
            self.pairingTask = nil
        }
        isCreatingInvite = false
        if let captureCleanupTask { await captureCleanupTask.value }
        if let handle = handleOwner.pointer {
            _ = desklink_host_release_all(handle)
            _ = desklink_host_stop(handle)
        }
        handleOwner.destroy()
        captureAdmission = nil
        releaseAllLocalInput()
        pairingInvite = nil
        pendingApproval = nil
        state = .closed
        stopTask = nil
    }

    fileprivate func consume(_ event: HostCallbackEvent) {
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
        case .input:
            guard canInjectInput, let input = event.input else { return }
            do {
                try inputInjector.inject(input)
            } catch {
                publishError("Remote input could not be applied.")
            }
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
        consume(hostCallbackEvent(value))
    }
#endif

    private func consumeState(_ rawState: Int, receipt: HostCaptureConnectionReceipt?) {
        guard let receipt, let admission = captureAdmission,
              receipt.runtimeGeneration == callbackGeneration, admission.matches(receipt) else { return }
        connectionReceipt = receipt
        if receipt.phase != .connected { retireActiveCapture() }
        switch rawState {
        case Int(DESKLINK_HOST_CONNECTING.rawValue): state = .connecting
        case Int(DESKLINK_HOST_WAITING_FOR_APPROVAL.rawValue): state = .waitingForApproval
        case Int(DESKLINK_HOST_NEGOTIATING_CAPABILITIES.rawValue): state = .negotiating
        case Int(DESKLINK_HOST_CONNECTED.rawValue):
            // Connecting may have been superseded while its UI delivery waited.
            // The native gate already retired that connection's capture.
            if let current = activeCapture, !admission.allows(current.owner) { retireActiveCapture() }
            state = .connected
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
        default: state = .idle
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
                inputInjector = MacInputInjector(displayFrame: geometry.displayFrame)
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
            guard let handle = handleOwner.pointer else {
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
        _ = inputInjector.releaseAll()
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
        if handleOwner.pointer != nil, stopTask == nil { stop() }
    }

    private static func redact(_ message: String) -> String {
        let lowercased = message.lowercased()
        if lowercased.contains("auth") || lowercased.contains("secret") || lowercased.contains("key") || lowercased.contains("token") {
            return "A secure host connection error occurred."
        }
        return message
    }
}

private enum HostCallbackKind: Sendable {
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

private struct HostCallbackEvent: Sendable {
    let kind: HostCallbackKind
    let state: Int
    let message: String?
    let fingerprint: String?
    let controllerDeviceID: [UInt8]
    let controllerVerifyKey: [UInt8]
    let input: RemoteInputCommand?
    let metrics: HostMetrics?
    let connectionReceipt: HostCaptureConnectionReceipt?
}

private func desklinkHostEventCallback(_ context: UnsafeMutableRawPointer?, _ event: UnsafePointer<DesklinkHostEvent>?) {
    guard let context, let event else { return }
    let callbackContext = Unmanaged<HostCallbackContext>.fromOpaque(context).takeUnretainedValue()
    guard let bridge = callbackContext.bridge else { return }
    let generation = callbackContext.generation
    let value = event.pointee
    let kind = hostCallbackKind(value.kind)
    let receipt: HostCaptureConnectionReceipt?
    switch kind {
    case .state:
        // Retire encoded deliveries before waiting for the MainActor. The
        // immutable Rust generation remains attached to the connection receipt.
        guard let observed = callbackContext.captureAdmission.observe(
            captureConnectionPhase(Int(value.state.rawValue)), mediaGeneration: value.media_generation
        ) else { return }
        receipt = observed
    case .approvalRequested:
        guard let observed = callbackContext.captureAdmission.observe(.unavailable, mediaGeneration: value.media_generation) else { return }
        receipt = observed
    default: receipt = nil
    }
    let mapped = hostCallbackEvent(value, connectionReceipt: receipt)
    DispatchQueue.main.async {
        guard bridge.acceptsCallback(generation: generation) else { return }
        if let receipt, !callbackContext.captureAdmission.matches(receipt) { return }
        bridge.consume(mapped)
    }
}

private func hostCallbackEvent(_ value: DesklinkHostEvent, connectionReceipt: HostCaptureConnectionReceipt? = nil) -> HostCallbackEvent {
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
    let input: RemoteInputCommand?
    if case .input = kind { input = hostInputCommand(value.input) } else { input = nil }
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
        input: input,
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
