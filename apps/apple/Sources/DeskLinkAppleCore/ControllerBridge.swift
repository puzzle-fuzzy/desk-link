import Combine
import CoreVideo
import DeskLinkC
import Foundation

private final class ControllerCallbackContext: @unchecked Sendable {
    weak var bridge: ControllerBridge?
    let generation: UInt64
    let decodeScheduler: H264DecodeScheduler

    init(bridge: ControllerBridge, generation: UInt64, decodeScheduler: H264DecodeScheduler) {
        self.bridge = bridge
        self.generation = generation
        self.decodeScheduler = decodeScheduler
    }
}

private final class DeskLinkHandleOwner: @unchecked Sendable {
    var pointer: OpaquePointer?
    var callbackContext: UnsafeMutableRawPointer?

    func destroy() {
        let retired = RetiredControllerHandle(pointer: self.pointer, callbackContext: self.callbackContext)
        self.pointer = nil
        self.callbackContext = nil
        guard retired.pointer != nil || retired.callbackContext != nil else { return }
        DispatchQueue.global(qos: .userInitiated).async { retired.finish() }
    }

    deinit { destroy() }
}

/// The callback context stays retained until Rust has joined its worker. The
/// old generation is retired by the bridge before this cleanup is scheduled.
private struct RetiredControllerHandle: @unchecked Sendable {
    let pointer: OpaquePointer?
    let callbackContext: UnsafeMutableRawPointer?

    func finish() {
        if let pointer { desklink_destroy(pointer) }
        if let callbackContext {
            Unmanaged<ControllerCallbackContext>.fromOpaque(callbackContext).release()
        }
    }
}

private struct DecodedConfiguration: Sendable {
    let generation: UInt64
    let streamID: UInt64
    let version: UInt32
    let width: UInt16
    let height: UInt16
}

@MainActor
public final class ControllerBridge: ObservableObject {
    @Published public private(set) var state: ConnectionState = .idle
    @Published public private(set) var pairing: PairingInfo?
    @Published public private(set) var metrics = Metrics()
    @Published public private(set) var lastError: String?
    @Published public private(set) var latestAccessUnit: Data?
    @Published public private(set) var latestPixelBuffer: CVPixelBuffer?
    @Published public private(set) var videoSize: CGSize?
    @Published public private(set) var controllerVerifyKeyHex: String?
    @Published public private(set) var cursorOverlay: CursorOverlay?
    @Published public private(set) var remotePlatform: RemotePlatform = .unknown
    @Published public private(set) var capabilities: SessionCapabilities?
    /// Consumers must explicitly handle these protocol events; bounded latest values.
    @Published public private(set) var latestControlMessage: Data?
    @Published public private(set) var latestTransferMessage: Data?
    @Published public private(set) var latestAudioPacket: Data?

    public var userFacingError: String { lastError ?? "" }

    private let configuration: DeskLinkRuntimeConfiguration
    private let handleOwner = DeskLinkHandleOwner()
    private lazy var decodeScheduler = H264DecodeScheduler(
        onFrame: { [weak self] frame in self?.consumeDecodedFrame(frame) },
        onConfiguration: { [weak self] generation, streamID, version, width, height in
            self?.consumeDecodedConfiguration(generation: generation, streamID: streamID,
                                              version: version, width: width, height: height)
        },
        onKeyframeRequest: { [weak self] generation, streamID in
            self?.consumeDecodeKeyframeRequest(generation: generation, streamID: streamID)
        }
    )
    private let identityStore: ControllerIdentityStore
    private let savedHostStore: SavedHostStore
    private var controllerIdentity: ControllerIdentity?
    private var savedHostForResume: SavedHost?
    private var activeStreamID: UInt64 = 0
    private var activeConfigVersion: UInt32 = 0
    private var pendingDecodedConfiguration: DecodedConfiguration?
    private var pendingDecodedFrame: H264DecodedFrame?
    private var pendingDecodeKeyframeRequest: (generation: UInt64, streamID: UInt64)?
    private var highestStreamID: UInt64 = 0
    private var callbackGeneration: UInt64 = 0
    private var awaitingApprovedHostMaterial = false
    private var suspendedForBackground = false
    private var requestKeyframeAfterNextVideoConfig = false
#if DEBUG
    public private(set) var releaseAllCallCountForTesting = 0
    public var recordInputsForTesting = false
    public private(set) var sentInputsForTesting: [DesklinkInput] = []
    public var activeRuntimeForTesting: Bool { handleOwner.pointer != nil }
    public var activeStreamIDForTesting: UInt64 { activeStreamID }
#endif

    public init(
        configuration: DeskLinkRuntimeConfiguration,
        identityStore: ControllerIdentityStore = ControllerIdentityStore(),
        savedHostStore: SavedHostStore = SavedHostStore()
    ) {
        self.configuration = configuration
        self.identityStore = identityStore
        self.savedHostStore = savedHostStore
        do {
            _ = try loadIdentityIfNeeded()
        } catch {
            publishError(error)
        }
    }

    public static func testing(
        configuration: DeskLinkRuntimeConfiguration = .macOSDefaults,
        error: String? = nil,
        state: ConnectionState = .idle
    ) -> ControllerBridge {
        let keychain = InMemoryKeychainStore()
        let bridge = ControllerBridge(
            configuration: configuration,
            identityStore: ControllerIdentityStore(keychain: keychain),
            savedHostStore: SavedHostStore(keychain: keychain)
        )
        bridge.state = state
        if let error { bridge.publishErrorMessage(error) }
        return bridge
    }

    public func connect(invite: Data) {
        state = .connecting
        lastError = nil
        guard invite.count == Int(DESKLINK_PAIRING_INVITE_BYTES) else {
            publishErrorMessage("The pairing invitation is invalid.")
            return
        }
        savedHostForResume = nil
        suspendedForBackground = false
        requestKeyframeAfterNextVideoConfig = false
        createIfNeeded()
        guard let handle = handleOwner.pointer else { return }
        do {
            let identity = try loadIdentityIfNeeded()
            var config = DesklinkPairingInviteConnectionConfig()
            withUnsafeMutableBytes(of: &config.controller_device_id) { $0.copyBytes(from: identity.deviceID) }
            withUnsafeMutableBytes(of: &config.controller_secret_key) { $0.copyBytes(from: identity.secretKey) }
            let result = invite.withUnsafeBytes { inviteBytes in
                relayServerName.withCString { serverName in
                    config.server_name = serverName
                    config.invite = inviteBytes.bindMemory(to: UInt8.self).baseAddress
                    config.invite_len = invite.count
                    return desklink_connect_pairing_invite(handle, &config)
                }
            }
            guard result == DESKLINK_OK else {
                publishResultError("The pairing invitation could not be used.", result: result)
                return
            }
            awaitingApprovedHostMaterial = true
            state = .connecting
        } catch {
            publishError(error)
        }
    }

    public func connect(invite: [UInt8]) { connect(invite: Data(invite)) }

    /// Connects through the relay directory using the same device ID and
    /// temporary/fixed password that the Windows controller accepts.
    public func connect(deviceID: String, temporaryPassword: String) {
        let normalizedDeviceID = deviceID.filter(\.isNumber)
        let normalizedPassword = temporaryPassword
            .uppercased()
            .filter { "23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains($0) }

        guard normalizedDeviceID.count == 12,
              let publicDeviceID = UInt64(normalizedDeviceID),
              normalizedPassword.count == Int(DESKLINK_DIRECTORY_ACCESS_CODE_BYTES)
        else {
            publishErrorMessage("设备 ID 或访问密码格式无效。")
            return
        }

        state = .connecting
        lastError = nil
        savedHostForResume = nil
        suspendedForBackground = false
        requestKeyframeAfterNextVideoConfig = false
        createIfNeeded()
        guard let handle = handleOwner.pointer else { return }
        do {
            let identity = try loadIdentityIfNeeded()
            var config = DesklinkDirectoryConnectionConfig(
                server_name: nil,
                device_id: publicDeviceID,
                access_code: (0, 0, 0, 0, 0, 0, 0, 0),
                controller_device_id: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
                controller_secret_key: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
            )
            withUnsafeMutableBytes(of: &config.access_code) {
                $0.copyBytes(from: normalizedPassword.utf8)
            }
            withUnsafeMutableBytes(of: &config.controller_device_id) {
                $0.copyBytes(from: identity.deviceID)
            }
            withUnsafeMutableBytes(of: &config.controller_secret_key) {
                $0.copyBytes(from: identity.secretKey)
            }
            let result = relayServerName.withCString { serverName in
                config.server_name = serverName
                return desklink_connect_directory(handle, &config)
            }
            guard result == DESKLINK_OK else {
                publishResultError("无法使用设备 ID 和访问密码连接。", result: result)
                return
            }
            // The directory lookup resolves the same signed host material as
            // a QR invite, but it is only available inside the Rust worker.
            // Stage it after the authenticated stream reaches Connected.
            awaitingApprovedHostMaterial = true
            state = .connecting
        } catch {
            publishError(error)
        }
    }

    public func connect(savedHost: SavedHost) {
        state = .connecting
        lastError = nil
        guard savedHost.isValid else {
            publishErrorMessage("The saved host record is invalid.")
            return
        }
        savedHostForResume = savedHost
        suspendedForBackground = false
        createIfNeeded()
        guard let handle = handleOwner.pointer else { return }
        do {
            let identity = try loadIdentityIfNeeded()
            var config = DesklinkSecureConnectionConfig()
            withUnsafeMutableBytes(of: &config.session_id) { $0.copyBytes(from: savedHost.sessionID) }
            withUnsafeMutableBytes(of: &config.relay_authentication) { $0.copyBytes(from: savedHost.relayAuthentication) }
            withUnsafeMutableBytes(of: &config.controller_device_id) { $0.copyBytes(from: identity.deviceID) }
            withUnsafeMutableBytes(of: &config.controller_secret_key) { $0.copyBytes(from: identity.secretKey) }
            withUnsafeMutableBytes(of: &config.host_verify_key) { $0.copyBytes(from: savedHost.hostVerifyKey) }
            let result = savedHost.serverName.withCString { serverName in
                config.server_name = serverName
                return desklink_connect_secure(handle, &config)
            }
            guard result == DESKLINK_OK else {
                publishResultError("The saved host could not be reached.", result: result)
                return
            }
            awaitingApprovedHostMaterial = false
            state = .connecting
        } catch {
            publishError(error)
        }
    }

    public func requestKeyframe() {
        guard let handle = handleOwner.pointer else { return }
        let result = desklink_request_keyframe(handle)
        if result != DESKLINK_OK { publishResultError("The video stream needs to recover.", result: result) }
    }

    /// Returns queue admission only. Remote acknowledgements remain separate.
    @discardableResult
    public func sendSessionCommand(_ command: UInt32, value: UInt32) -> Bool {
        guard case .connected = state, let handle = handleOwner.pointer else { return false }
        let result = desklink_controller_command(handle, command, value)
        if result != DESKLINK_OK { lastError = "操作暂未发送，请稍后重试。" }
        return result == DESKLINK_OK
    }

    @discardableResult
    public func sendTransferMessage(_ encoded: Data) -> Bool {
        guard case .connected = state, let handle = handleOwner.pointer else { return false }
        let result = encoded.withUnsafeBytes { bytes in
            desklink_controller_send_transfer(handle, bytes.bindMemory(to: UInt8.self).baseAddress, encoded.count)
        }
        if result != DESKLINK_OK { lastError = "传输请求暂未发送，请稍后重试。" }
        return result == DESKLINK_OK
    }

    public func send(input command: RemoteInputCommand) {
        var input: DesklinkInput
        switch command {
        case let .move(x, y):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_MOVE, x: x, y: y, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0)
        case let .mouseButton(button, pressed):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_BUTTON, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: button.rawValue + 1, key_code: 0, character: 0, pressed: pressed ? 1 : 0, modifiers: 0)
        case let .wheel(x, y):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_WHEEL, x: 0, y: 0, wheel_x: x, wheel_y: y, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0)
        case let .key(code, pressed, modifiers):
            input = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: code, character: 0, pressed: pressed ? 1 : 0, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f))
        case let .character(scalar, pressed, modifiers):
            input = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: scalar, pressed: pressed ? 1 : 0, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f))
        case let .unicode(text, modifiers):
            var previousWasCR = false
            for scalar in text.unicodeScalars {
                if scalar.value == 10 && previousWasCR { previousWasCR = false; continue }
                previousWasCR = scalar.value == 13
                let specialKey: UInt32 = scalar.value == 10 || scalar.value == 13 ? 1 : (scalar.value == 9 ? 4 : 0)
                var characterInput = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: specialKey, character: specialKey == 0 ? scalar.value : 0, pressed: 1, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f))
                sendInput(&characterInput)
                characterInput.pressed = 0
                sendInput(&characterInput)
            }
            return
        }
        sendInput(&input)
    }

    public func releaseAll() {
#if DEBUG
        releaseAllCallCountForTesting += 1
#endif
        guard let handle = handleOwner.pointer else { return }
        _ = desklink_release_all(handle)
    }

    /// Stops the active native runtime while retaining the approved host
    /// material for a foreground reconnect. Callback generation is advanced
    /// before teardown so queued events from the retired runtime cannot update
    /// the new session's decoder or UI.
    public func suspendForBackground() {
        guard !suspendedForBackground else { return }
        suspendedForBackground = true
        callbackGeneration &+= 1
        releaseAll()
        handleOwner.destroy()
        resetDisplayedSessionState()
        requestKeyframeAfterNextVideoConfig = false
        if savedHostForResume != nil, state != .idle {
            state = .reconnecting
        }
    }

    /// Reconnects once after a background suspension. A saved host is kept in
    /// Keychain independently of this in-memory reference; the reference is
    /// only the session that should resume automatically.
    public func resumeFromBackground() {
        guard suspendedForBackground else { return }
        suspendedForBackground = false
        guard let savedHost = savedHostForResume else {
            if state == .reconnecting { state = .closed }
            return
        }
        requestKeyframeAfterNextVideoConfig = true
        connect(savedHost: savedHost)
        if case .connecting = state { state = .recovering }
    }

    public func disconnect() {
        suspendedForBackground = false
        requestKeyframeAfterNextVideoConfig = false
        callbackGeneration &+= 1
        releaseAll()
        handleOwner.destroy()
        resetDisplayedSessionState()
        awaitingApprovedHostMaterial = false
        state = .closed
    }

    /// Clears all controller-side connection material during account logout.
    /// The device identity remains local so a future login can pair as the
    /// same physical installation, but saved host credentials never cross
    /// account boundaries.
    public func clearRemoteLinksForLogout() {
        disconnect()
        savedHostForResume = nil
        awaitingApprovedHostMaterial = false
        do {
            try savedHostStore.removeAll()
        } catch {
            publishErrorMessage("无法清除本机已保存的远程设备，请稍后重试。")
        }
    }

    fileprivate func consume(
        eventKind: Int,
        data: Data,
        streamID: UInt64,
        frameID: UInt64,
        configVersion: UInt32,
        width: UInt16,
        height: UInt16,
        stateValue: Int,
        errorCode: UInt32 = 0,
        recoverable: Bool = false
    ) {
        switch eventKind {
        case 1:
            consumeState(streamID: streamID, stateValue: stateValue)
        case 2:
            let message = String(decoding: data, as: UTF8.self)
            if recoverable {
                consumeState(streamID: streamID, stateValue: stateValue)
                lastError = errorCode == 2 ? "正在恢复画面，请稍候。" : "连接已中断，正在自动重连。"
            } else {
                publishErrorMessage(message.isEmpty ? "The connection reported an error." : message)
            }
        case 8:
            guard streamID == activeStreamID, !data.isEmpty else { return }
            cursorOverlay = CursorOverlay(streamID: streamID, encodedUpdate: data)
        case 4: latestControlMessage = data
        case 11: latestTransferMessage = data
        case 12:
            guard streamID == activeStreamID else { return }
            latestAudioPacket = data
        case 13:
            guard let value = SessionCapabilities(encoded: data) else { return }
            capabilities = value
            remotePlatform = value.platform
        default:
            break
        }
    }

    private var relayServerName: String {
        configuration.relayServerName
    }

    private func createIfNeeded() {
        guard handleOwner.pointer == nil else { return }
        callbackGeneration &+= 1
        decodeScheduler.reset(generation: callbackGeneration)
        let callbackContext = ControllerCallbackContext(bridge: self, generation: callbackGeneration, decodeScheduler: decodeScheduler)
        let callbackPointer = Unmanaged.passRetained(callbackContext).toOpaque()
        var createdHandle: OpaquePointer?
        guard configuration.isValid else {
            Unmanaged<ControllerCallbackContext>.fromOpaque(callbackPointer).release()
            publishErrorMessage("The DeskLink runtime configuration is invalid.")
            return
        }
        let result = configuration.relayURL.withCString { relayPointer in
            var config = DesklinkConfig(relay_url: relayPointer, log_level: 1)
            return desklink_create_for_platform(
                &config,
                configuration.platform.cValue,
                desklinkEventCallback,
                callbackPointer,
                &createdHandle
            )
        }
        guard result == DESKLINK_OK, let createdHandle else {
            Unmanaged<ControllerCallbackContext>.fromOpaque(callbackPointer).release()
            publishResultError("DeskLink could not start its connection runtime.", result: result)
            return
        }
        handleOwner.pointer = createdHandle
        handleOwner.callbackContext = callbackPointer
    }

    private func consumeState(streamID: UInt64, stateValue: Int) {
        switch stateValue {
        case Int(DESKLINK_CONNECTED.rawValue):
            guard streamID > 0,
                  streamID == activeStreamID || streamID > highestStreamID
            else { return }
            if activeStreamID != streamID {
                // Configuration/frames may already be admitted by the FFI
                // callback. configure owns their stream barrier; do not clear it
                // from this delayed main-actor state notification.
                activeConfigVersion = 0
                activeStreamID = streamID
                metrics.lastFrameID = nil
                videoSize = nil
            }
            highestStreamID = max(highestStreamID, streamID)
            state = .connected(streamID: streamID)
            lastError = nil
            applyPendingDecodedConfiguration()
            stageApprovedHostMaterialIfNeeded()
        case Int(DESKLINK_CLOSED.rawValue):
            guard streamID == 0 || activeStreamID == 0 || streamID == activeStreamID else { return }
            decodeScheduler.reset(generation: callbackGeneration)
            activeConfigVersion = 0
            pendingDecodedConfiguration = nil
            pendingDecodedFrame = nil
            pendingDecodeKeyframeRequest = nil
            activeStreamID = 0
            latestPixelBuffer = nil
            videoSize = nil
            cursorOverlay = nil
            state = .closed
        case Int(DESKLINK_WAITING_FOR_APPROVAL.rawValue): state = .pairing
        case Int(DESKLINK_IDLE.rawValue): state = .idle
        case Int(DESKLINK_DEGRADED.rawValue): state = .frozen
        case Int(DESKLINK_RECOVERING_VIDEO.rawValue): state = .recovering
        case Int(DESKLINK_RECONNECTING.rawValue): state = .reconnecting
        case Int(DESKLINK_DISCONNECTING.rawValue): state = .reconnecting
        default: state = .connecting
        }
    }

    private func consumeDecodedConfiguration(generation: UInt64, streamID: UInt64,
                                             version: UInt32, width: UInt16, height: UInt16) {
        guard acceptsCallback(generation: generation),
              decodeScheduler.accepts(generation: generation, streamID: streamID, version: version) else { return }
        // Decoder delivery uses a MainActor task while Rust state uses main.async;
        // neither mechanism orders the other. Retain one config until Connected
        // arrives instead of losing a config delivered first.
        pendingDecodedConfiguration = DecodedConfiguration(generation: generation, streamID: streamID,
                                                           version: version, width: width, height: height)
        applyPendingDecodedConfiguration()
    }

    private func applyPendingDecodedConfiguration() {
        guard let pending = pendingDecodedConfiguration, pending.streamID == activeStreamID,
              acceptsCallback(generation: pending.generation),
              decodeScheduler.accepts(generation: pending.generation, streamID: pending.streamID, version: pending.version) else { return }
        pendingDecodedConfiguration = nil
        activeConfigVersion = pending.version
        videoSize = CGSize(width: Int(pending.width), height: Int(pending.height))
        if requestKeyframeAfterNextVideoConfig {
            requestKeyframeAfterNextVideoConfig = false
            requestDecodeKeyframe()
        }
        flushPendingDecodeKeyframeRequest()
        if let frame = pendingDecodedFrame {
            pendingDecodedFrame = nil
            consumeDecodedFrame(frame)
        }
    }

    private func consumeDecodedFrame(_ frame: H264DecodedFrame) {
        guard acceptsCallback(generation: frame.generation),
              decodeScheduler.accepts(generation: frame.generation, streamID: frame.streamID, version: frame.version) else { return }
        guard activeStreamID == frame.streamID, activeConfigVersion == frame.version else {
            pendingDecodedFrame = frame
            return
        }
        guard frame.frameID > (metrics.lastFrameID ?? 0) else { return }
        latestPixelBuffer = frame.pixelBuffer
        let snapshot = decodeScheduler.snapshot()
        metrics.receivedFrames = Int(clamping: snapshot.decodedFrames)
        metrics.droppedFrames = Int(clamping: snapshot.droppedFrames)
        metrics.lastFrameID = frame.frameID
    }

    private func consumeDecodeKeyframeRequest(generation: UInt64, streamID: UInt64) {
        guard acceptsCallback(generation: generation) else { return }
        pendingDecodeKeyframeRequest = (generation, streamID)
        flushPendingDecodeKeyframeRequest()
    }

    private func flushPendingDecodeKeyframeRequest() {
        guard let pending = pendingDecodeKeyframeRequest, activeStreamID == pending.streamID,
              acceptsCallback(generation: pending.generation), activeConfigVersion > 0,
              decodeScheduler.accepts(generation: pending.generation, streamID: pending.streamID, version: activeConfigVersion) else { return }
        pendingDecodeKeyframeRequest = nil
        requestDecodeKeyframe()
    }

    private func requestDecodeKeyframe() {
        guard let handle = handleOwner.pointer else { return }
        if desklink_request_keyframe(handle) != DESKLINK_OK {
            // Queue congestion is a recovery condition; retiring the runtime
            // here would cancel the transport that is still recovering.
            lastError = "画面恢复请求暂未发送，正在等待连接恢复。"
        }
    }

    private func stageApprovedHostMaterialIfNeeded() {
        guard awaitingApprovedHostMaterial, let handle = handleOwner.pointer else { return }
        var material = DesklinkSavedHostMaterial()
        guard desklink_controller_copy_saved_host_material(handle, &material) == DESKLINK_OK else { return }
        let serverName = withUnsafeBytes(of: material.server_name) {
            String(decoding: $0.prefix { $0 != 0 }, as: UTF8.self)
        }
        let host = SavedHost(
            id: UUID(),
            serverName: serverName,
            sessionID: withUnsafeBytes(of: material.session_id) { Array($0) },
            relayAuthentication: withUnsafeBytes(of: material.relay_authentication) { Array($0) },
            hostVerifyKey: withUnsafeBytes(of: material.host_verify_key) { Array($0) }
        )
        do {
            try savedHostStore.save(host)
            savedHostForResume = host
            awaitingApprovedHostMaterial = false
        } catch {
            publishErrorMessage("The approved host could not be saved securely.")
        }
    }

    private func sendInput(_ input: inout DesklinkInput) {
#if DEBUG
        if recordInputsForTesting { sentInputsForTesting.append(input) }
#endif
        guard case .connected = state, let handle = handleOwner.pointer else { return }
        let result = desklink_send_input(handle, &input)
        guard result != DESKLINK_INVALID_ARGUMENT else { return }
        if result != DESKLINK_OK {
            publishErrorMessage("Input delivery failed.")
        }
    }

    private func loadIdentityIfNeeded() throws -> ControllerIdentity {
        if let controllerIdentity { return controllerIdentity }
        let identity = try identityStore.loadOrCreate()
        controllerIdentity = identity
        controllerVerifyKeyHex = identity.verifyKey.map { String(format: "%02x", $0) }.joined()
        return identity
    }

    private func publishResultError(_ message: String, result: DesklinkResult) {
        publishErrorMessage("\(message) (code \(result.rawValue)).")
    }

    private func publishError(_ error: Error) { publishErrorMessage(error.localizedDescription) }

    private func resetDisplayedSessionState() {
        capabilities = nil
        remotePlatform = .unknown
        latestControlMessage = nil
        latestTransferMessage = nil
        latestAudioPacket = nil
        decodeScheduler.reset(generation: callbackGeneration)
        activeConfigVersion = 0
        pendingDecodedConfiguration = nil
        pendingDecodedFrame = nil
        pendingDecodeKeyframeRequest = nil
        activeStreamID = 0
        highestStreamID = 0
        metrics.lastFrameID = nil
        latestAccessUnit = nil
        latestPixelBuffer = nil
        videoSize = nil
        cursorOverlay = nil
    }

    private func publishErrorMessage(_ message: String) {
        let safe = Self.userFacingMessage(message)
        if handleOwner.pointer != nil {
            disconnect()
        }
        lastError = safe
        state = .failed(safe)
    }

    private static func userFacingMessage(_ message: String) -> String {
        let lowercase = message.lowercased()
        if message.range(of: "[\\u{4e00}-\\u{9fff}]", options: .regularExpression) != nil {
            return message
        }
        if lowercase.contains("runtime configuration is invalid") {
            return "DeskLink 连接配置无效，请检查中继地址和服务器名称。"
        }
        if lowercase.contains("pairing invitation is invalid") {
            return "连接码无效，请重新扫描二维码或粘贴完整连接码。"
        }
        if lowercase.contains("pairing invitation could not be used") {
            return "连接码无法使用，请重新生成连接码后重试。"
        }
        if lowercase.contains("device is offline")
            || lowercase.contains("temporary password")
            || lowercase.contains("access code")
            || lowercase.contains("device returned an invalid")
        {
            return "找不到在线设备或访问密码不正确，请确认设备在线并检查密码后重试。"
        }
        if lowercase.contains("could not start its connection runtime") {
            return "连接运行时启动失败，请稍后重试。"
        }
        if lowercase.contains("auth") || lowercase.contains("secret") || lowercase.contains("key") || lowercase.contains("token") {
            return "安全连接校验失败，请重新生成连接码后重试。"
        }
        if lowercase.contains("relay") || lowercase.contains("transport") || lowercase.contains("timed out") {
            return "中继服务暂时不可用，请检查网络或中继配置后重试。"
        }
        if lowercase.contains("connection reported an error") {
            return "连接运行时报告错误，请稍后重试。"
        }
        return "连接失败，请检查网络和中继配置后重试。"
    }

#if DEBUG
    /// Exercises UI state ordering only; the invalid header cannot configure a
    /// native decoder and is not evidence of video or hardware acceptance.
    public func stageDecodedMetadataForTesting(streamID: UInt64, version: UInt32) {
        decodeScheduler.configure(sequenceHeader: Data([1]), streamID: streamID,
                                  width: 1920, height: 1080, version: version, generation: callbackGeneration)
        consumeDecodedConfiguration(generation: callbackGeneration, streamID: streamID,
                                    version: version, width: 1920, height: 1080)
    }

    public func acceptCapabilitiesForTesting(platform: RemotePlatform) {
        remotePlatform = platform
    }

    public func acceptErrorForTesting(stateValue: Int, recoverable: Bool) {
        consume(eventKind: 2, data: Data("transport unavailable".utf8), streamID: 0,
                frameID: 0, configVersion: 0, width: 0, height: 0,
                stateValue: stateValue, errorCode: recoverable ? 1 : 3, recoverable: recoverable)
    }

    public func acceptStateForTesting(streamID: UInt64) {
        consumeState(streamID: streamID, stateValue: Int(DESKLINK_CONNECTED.rawValue))
    }
#endif
}

private func desklinkEventCallback(_ context: UnsafeMutableRawPointer?, _ event: UnsafePointer<DesklinkEvent>?) {
    guard let context, let event else { return }
    let callbackContext = Unmanaged<ControllerCallbackContext>.fromOpaque(context).takeUnretainedValue()
    guard let bridge = callbackContext.bridge else { return }
    let value = event.pointee
    let generation = callbackContext.generation
    // Admit encoded media on the FFI callback thread before any main-queue
    // scheduling. Data is copied once while the borrowed FFI buffer is alive.
    if value.kind == DESKLINK_EVENT_VIDEO_CONFIG {
        guard value.data_len > 0, value.data_len <= H264DecodeScheduler.maximumSequenceHeaderBytes,
              let pointer = value.data else { return }
        callbackContext.decodeScheduler.configure(sequenceHeader: Data(bytes: pointer, count: value.data_len),
            streamID: value.stream_id, width: value.width, height: value.height,
            version: value.config_version, generation: generation)
        return
    }
    if value.kind == DESKLINK_EVENT_H264_ACCESS_UNIT {
        let accessUnit: Data
        if value.data_len > 0, value.data_len <= H264DecodeScheduler.maximumEncodedInputBytes, let pointer = value.data {
            accessUnit = Data(bytes: pointer, count: value.data_len)
        } else {
            // Invalidate the dependent GOP without copying an oversized packet.
            accessUnit = Data()
        }
        callbackContext.decodeScheduler.receive(accessUnit: accessUnit, streamID: value.stream_id,
            frameID: value.frame_id, version: value.config_version, generation: generation)
        return
    }
    let data = value.data.map { Data(bytes: $0, count: value.data_len) } ?? Data()
    let eventKind = Int(value.kind.rawValue)
    let streamID = value.stream_id
    let frameID = value.frame_id
    let configVersion = value.config_version
    let width = value.width
    let height = value.height
    let stateValue = Int(value.state.rawValue)
    let errorCode = value.error_code
    let recoverable = value.recoverable != 0
    DispatchQueue.main.async {
        guard bridge.acceptsCallback(generation: generation) else { return }
        bridge.consume(
            eventKind: eventKind, data: data, streamID: streamID, frameID: frameID,
            configVersion: configVersion, width: width, height: height, stateValue: stateValue,
            errorCode: errorCode, recoverable: recoverable
        )
    }
}

private extension ControllerBridge {
    func acceptsCallback(generation: UInt64) -> Bool {
        callbackGeneration == generation
    }
}
