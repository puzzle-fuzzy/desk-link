import AppKit
import CoreGraphics
import DeskLinkC
import Foundation

private final class HostCallbackContext: @unchecked Sendable {
    weak var bridge: HostBridge?

    init(bridge: HostBridge) {
        self.bridge = bridge
    }
}

private final class HostHandleOwner: @unchecked Sendable {
    var pointer: OpaquePointer?
    var callbackContext: UnsafeMutableRawPointer?

    func destroy() {
        guard let pointer else { return }
        self.pointer = nil
        desklink_host_destroy(pointer)
        if let callbackContext {
            self.callbackContext = nil
            Unmanaged<HostCallbackContext>.fromOpaque(callbackContext).release()
        }
    }

    deinit { destroy() }
}

@MainActor
final class HostBridge: ObservableObject {
    @Published private(set) var state: HostState = .idle
    @Published private(set) var permissions: MacPermissionSnapshot
    @Published private(set) var pairingInvite: HostPairingInvite?
    @Published private(set) var pendingApproval: HostApproval?
    @Published private(set) var metrics = HostMetrics()
    @Published private(set) var trustedControllers: [TrustedController] = []
    @Published private(set) var lastError: String?

    var canStartCapture: Bool {
        permissions.screenRecording == .granted && state == .connected
    }

    var canInjectInput: Bool {
        permissions.accessibility == .granted && state == .connected
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
    private let captureSource = ScreenCaptureSource()
    private let encoder = MacH264Encoder()
    private var inputInjector = MacInputInjector()
    private var captureStarting = false
    private var captureRunning = false
    private var stopTask: Task<Void, Never>?

    init(
        relayURL: String = ProcessInfo.processInfo.environment["DESKLINK_RELAY_URL"] ?? "quic://127.0.0.1:4433",
        serverName: String = ProcessInfo.processInfo.environment["DESKLINK_RELAY_SERVER_NAME"] ?? "localhost",
        permissions: MacPermissions = MacPermissions(),
        identityStore: HostIdentityStore = HostIdentityStore(),
        trustedControllerStore: TrustedControllerStore = TrustedControllerStore()
    ) {
        self.relayURL = relayURL
        self.serverName = serverName
        permissionProvider = permissions
        self.identityStore = identityStore
        self.trustedControllerStore = trustedControllerStore
        self.permissions = permissions.snapshot()
        encoder.onEvent = { [weak self] event in
            Task { @MainActor in self?.sendVideo(event) }
        }
        encoder.onError = { [weak self] error in
            Task { @MainActor in self?.publishError("Video encoding could not continue: \(error).") }
        }
        reloadTrustedControllers()
    }

    static func testing(permissionSnapshot: MacPermissionSnapshot) -> HostBridge {
        let bridge = HostBridge(permissions: MacPermissions(provider: StaticMacPermissionProvider(
            screenRecordingGranted: permissionSnapshot.screenRecording == .granted,
            accessibilityGranted: permissionSnapshot.accessibility == .granted
        )))
        bridge.permissions = permissionSnapshot
        bridge.state = .idle
        return bridge
    }

    func start() {
        guard handleOwner.pointer == nil else { return }
        do {
            let identity = try identityStore.loadOrCreate()
            let context = HostCallbackContext(bridge: self)
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
    }

    func requestAccessibility() {
        applyPermissions(permissionProvider.requestAccessibility())
    }

    func createInvite() {
        start()
        guard let handle = handleOwner.pointer else { return }
        var bytes = [UInt8](repeating: 0, count: Int(DESKLINK_PAIRING_INVITE_BYTES))
        let capacity = bytes.count
        var length = 0
        var expiry: UInt64 = 0
        let result = bytes.withUnsafeMutableBytes {
            desklink_host_start_pairing(handle, $0.bindMemory(to: UInt8.self).baseAddress, capacity, &length, &expiry)
        }
        guard result == DESKLINK_OK, length == capacity else {
            publishResultError("A secure invitation could not be created.", result: result)
            return
        }
        pairingInvite = HostPairingInvite(
            expiresAt: Date(timeIntervalSince1970: TimeInterval(expiry)),
            encoded: Data(bytes)
        )
        state = .connecting
        lastError = nil
    }

    func copyInviteToPasteboard() {
        guard let pairingInvite else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(pairingInvite.encoded.base64EncodedString(), forType: .string)
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
        do {
            _ = try trustedControllerStore.revoke(deviceID: controller.deviceID)
            reloadTrustedControllers()
        } catch {
            publishError("The trusted controller could not be revoked.")
        }
    }

    func stop() {
        guard stopTask == nil else { return }
        stopTask = Task { @MainActor [weak self] in await self?.performStop() }
    }

    func shutdown() {
        stop()
    }

    func shutdownAndWait() async {
        if let stopTask {
            await stopTask.value
        } else {
            await performStop()
        }
    }

    private func performStop() async {
        state = .stopping
        releaseAllLocalInput()
        await captureSource.stop()
        captureStarting = false
        captureRunning = false
        encoder.stop()
        if let handle = handleOwner.pointer {
            _ = desklink_host_release_all(handle)
            _ = desklink_host_stop(handle)
        }
        handleOwner.destroy()
        pairingInvite = nil
        pendingApproval = nil
        state = .closed
        stopTask = nil
    }

    fileprivate func consume(_ event: HostCallbackEvent) {
        switch event.kind {
        case .state:
            consumeState(event.state)
        case .error:
            publishError(event.message ?? "The host runtime reported an error.")
        case .approvalRequested:
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
            encoder.requestKeyframe()
        case .releaseAll:
            releaseAllLocalInput()
        case .metrics:
            if let metrics = event.metrics { self.metrics = metrics }
        }
    }

    private func consumeState(_ rawState: Int) {
        switch rawState {
        case Int(DESKLINK_HOST_CONNECTING.rawValue): state = .connecting
        case Int(DESKLINK_HOST_WAITING_FOR_APPROVAL.rawValue): state = .waitingForApproval
        case Int(DESKLINK_HOST_NEGOTIATING_CAPABILITIES.rawValue): state = .negotiating
        case Int(DESKLINK_HOST_CONNECTED.rawValue):
            state = .connected
            startCaptureIfPermitted()
        case Int(DESKLINK_HOST_STOPPING.rawValue): state = .stopping
        case Int(DESKLINK_HOST_CLOSED.rawValue):
            releaseAllLocalInput()
            state = .closed
            if stopTask == nil { stop() }
        default: state = .idle
        }
    }

    private func startCaptureIfPermitted() {
        guard canStartCapture, !captureStarting, !captureRunning else { return }
        captureStarting = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try await captureSource.start(
                    displayID: CGMainDisplayID(),
                    streamID: 1,
                    onFrame: { [weak encoder] buffer, frameID in
                        encoder?.encode(pixelBuffer: buffer, frameID: frameID)
                    },
                    onStop: { [weak self] _ in
                        Task { @MainActor in self?.captureStoppedUnexpectedly() }
                    }
                )
                guard state == .connected else {
                    await captureSource.stop()
                    captureStarting = false
                    return
                }
                let frame = captureSource.capturedDisplayFrame
                try encoder.start(width: Int(frame.width), height: Int(frame.height), streamID: 1)
                inputInjector = MacInputInjector(displayFrame: frame)
                captureRunning = true
                captureStarting = false
            } catch {
                captureStarting = false
                captureRunning = false
                publishError("Screen capture could not start. Check Screen Recording permission.")
            }
        }
    }

    private func captureStoppedUnexpectedly() {
        guard captureRunning || captureStarting else { return }
        captureRunning = false
        captureStarting = false
        encoder.stop()
        if state == .connected { publishError("Screen capture stopped unexpectedly.") }
    }

    private func applyPermissions(_ snapshot: MacPermissionSnapshot) {
        let previous = permissions
        permissions = snapshot
        if previous.screenRecording == .granted, snapshot.screenRecording != .granted,
           captureRunning || captureStarting
        {
            captureRunning = false
            captureStarting = false
            Task { @MainActor [weak self] in
                guard let self else { return }
                await captureSource.stop()
                encoder.stop()
                releaseAllLocalInput()
            }
        }
        if previous.accessibility == .granted, snapshot.accessibility != .granted {
            releaseAllLocalInput()
        }
        startCaptureIfPermitted()
    }

    private func sendVideo(_ event: EncodedVideoEvent) {
        guard state == .connected, let handle = handleOwner.pointer else { return }
        let result: DesklinkResult
        switch event {
        case let .configuration(streamID, version, width, height, bytes):
            result = bytes.withUnsafeBytes {
                desklink_host_send_video_config(handle, streamID, version, width, height, $0.bindMemory(to: UInt8.self).baseAddress, bytes.count)
            }
        case let .accessUnit(streamID, frameID, configVersion, _, bytes):
            result = bytes.withUnsafeBytes {
                desklink_host_send_video_access_unit(handle, streamID, frameID, configVersion, $0.bindMemory(to: UInt8.self).baseAddress, bytes.count)
            }
        }
        if result != DESKLINK_OK { publishResultError("Video delivery could not continue.", result: result) }
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

private enum HostCallbackKind {
    case state
    case error
    case approvalRequested
    case input
    case keyframeRequested
    case releaseAll
    case metrics
}

private struct HostCallbackEvent {
    let kind: HostCallbackKind
    let state: Int
    let message: String?
    let fingerprint: String?
    let controllerDeviceID: [UInt8]
    let controllerVerifyKey: [UInt8]
    let input: MacInputCommand?
    let metrics: HostMetrics?
}

private func desklinkHostEventCallback(_ context: UnsafeMutableRawPointer?, _ event: UnsafePointer<DesklinkHostEvent>?) {
    guard let context, let event else { return }
    let callbackContext = Unmanaged<HostCallbackContext>.fromOpaque(context).takeUnretainedValue()
    let value = event.pointee
    let mapped = HostCallbackEvent(
        kind: hostCallbackKind(value.kind),
        state: Int(value.state.rawValue),
        message: value.data.map { String(decoding: UnsafeBufferPointer(start: $0, count: value.data_len), as: UTF8.self) },
        fingerprint: value.fingerprint.map { String(decoding: UnsafeBufferPointer(start: $0, count: value.fingerprint_len), as: UTF8.self) },
        controllerDeviceID: withUnsafeBytes(of: value.controller_device_id) { Array($0) },
        controllerVerifyKey: withUnsafeBytes(of: value.controller_verify_key) { Array($0) },
        input: hostInputCommand(value.input),
        metrics: HostMetrics(
            sentVideoConfigurations: Int(value.metrics.sent_video_configs),
            sentVideoPackets: Int(value.metrics.sent_video_packets),
            receivedInputEvents: Int(value.metrics.received_input_events),
            keyframeRequests: Int(value.metrics.keyframe_requests)
        )
    )
    DispatchQueue.main.async {
        callbackContext.bridge?.consume(mapped)
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
    default: .error
    }
}

private func hostInputCommand(_ input: DesklinkHostInput) -> MacInputCommand? {
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
        if input.character != 0,
           let scalar = UnicodeScalar(input.character) {
            return .unicode(String(Character(scalar)), modifiers: modifiers)
        }
        return .key(code: input.key_code, pressed: input.pressed != 0, modifiers: modifiers)
    default:
        return nil
    }
}
