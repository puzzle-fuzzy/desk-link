import Combine
import CoreVideo
import DeskLinkC
import Foundation

/// Connection stages use the stable C ABI values so UI progress reflects the
/// worker's actual phase, including phases before a video stream exists.
public enum ControllerConnectionStage: Int, Sendable {
    case idle = 0, creatingSession = 1, connectingRelay = 2, secureHandshake = 3
    case waitingApproval = 4, negotiating = 5, startingVideo = 6, connected = 7
    case degraded = 8, recoveringVideo = 9, reconnecting = 10, disconnecting = 11, closed = 12
}

private final class ControllerCallbackContext: @unchecked Sendable {
    weak var bridge: ControllerBridge?
    let generation: UInt64
    let decodeScheduler: H264DecodeScheduler
    private let audioLock = NSLock()
    private var audioFrames: [(data: Data, stream: UInt64, sequence: UInt64, received: UInt64)] = []
    private var audioDrainScheduled = false

    init(bridge: ControllerBridge, generation: UInt64, decodeScheduler: H264DecodeScheduler) {
        self.bridge = bridge
        self.generation = generation
        self.decodeScheduler = decodeScheduler
    }

    func enqueuePCM(_ data: Data, streamID: UInt64, sequence: UInt64) {
        let schedule: Bool = audioLock.withLock {
            if audioFrames.count == 6 { audioFrames.removeFirst() }
            audioFrames.append((data, streamID, sequence, DispatchTime.now().uptimeNanoseconds))
            guard !audioDrainScheduled else { return false }
            audioDrainScheduled = true
            return true
        }
        guard schedule else { return }
        DispatchQueue.main.async { [self] in
            let frames = audioLock.withLock {
                let result = audioFrames
                audioFrames.removeAll(keepingCapacity: true)
                audioDrainScheduled = false
                return result
            }
            let now = DispatchTime.now().uptimeNanoseconds
            for frame in frames where now >= frame.received && now - frame.received < 100_000_000 {
                bridge?.consume(eventKind: Int(DESKLINK_EVENT_AUDIO_PCM.rawValue), data: frame.data,
                    streamID: frame.stream, frameID: frame.sequence, configVersion: 0, width: 48_000, height: 1,
                    stateValue: Int(DESKLINK_CONNECTED.rawValue), generation: generation)
            }
        }
    }

    func discardQueuedPCM() { audioLock.withLock { audioFrames.removeAll(keepingCapacity: true) } }
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
    let inputEpoch: UInt64
    let version: UInt32
    let width: UInt16
    let height: UInt16
}

@MainActor
public final class ControllerBridge: ObservableObject {
    @Published public private(set) var state: ConnectionState = .idle
    @Published public private(set) var connectionStage: ControllerConnectionStage = .idle
    @Published public private(set) var connectionAttemptStartedAt: Date?
    @Published public private(set) var pairing: PairingInfo?
    @Published public private(set) var metrics = Metrics()
    @Published public private(set) var lastError: String?
    @Published public private(set) var latestAccessUnit: Data?
    @Published public private(set) var latestPixelBuffer: CVPixelBuffer?
    @Published public private(set) var videoFrame: PresentedVideoFrame?
    @Published public private(set) var presentedInputContext: CapturedInputContext?
    @Published public private(set) var videoSize: CGSize?
    @Published public private(set) var controllerVerifyKeyHex: String?
    @Published public private(set) var cursorOverlay: CursorOverlay?
    @Published public private(set) var remotePlatform: RemotePlatform = .unknown
    @Published public private(set) var capabilities: SessionCapabilities?
    /// Consumers must explicitly handle these protocol events; bounded latest values.
    @Published public private(set) var latestControlMessage: Data?
    @Published public private(set) var latestTransferMessage: Data?
    @Published public private(set) var latestAudioPacket: Data?
    @Published public private(set) var remoteDisplays: [SessionDisplay] = []
    @Published public private(set) var activeDisplayID: UInt32?
    @Published public private(set) var confirmedQuality: SessionVideoQuality?
    @Published public private(set) var inputRestriction: SessionInputRestrictionReason?
    @Published public private(set) var audioAvailable = false
    @Published public private(set) var audioEnabled = false
    @Published public private(set) var qualityStateRevision: UInt64 = 0
    @Published public private(set) var displayStateRevision: UInt64 = 0
    @Published public private(set) var displays: [RemoteDisplay] = []
    @Published public private(set) var selectedDisplayID: UInt32?
    @Published public private(set) var pendingDisplayID: UInt32?
    @Published public private(set) var displayError: String?
    @Published public private(set) var qualityPreference: VideoQualityPreference = .automatic
    @Published public private(set) var appliedQualityPreset: VideoQualityPreset?
    @Published public private(set) var pendingQuality: VideoQualityPreference?
    @Published public private(set) var qualityError: String?
    @Published public private(set) var isAwaitingDisplayFrame = false
    @Published public private(set) var inputContextID: UInt64 = 0
    @Published public private(set) var isClipboardBusy = false
    @Published public private(set) var clipboardStatus: String?
    @Published public private(set) var clipboardError: String?
    public let media = ControllerMediaBridge()

    /// Invoked only for the matching, user-requested receive operation. The
    /// platform UI owns pasteboard access; shared Apple code never writes it.
    public var onRemoteClipboardText: ((String) -> Void)?

    public var canSendRemoteInput: Bool {
        guard case .connected(let streamID) = state else { return false }
        return streamID == activeStreamID && streamID > 0 && !isAwaitingDisplayFrame && pendingClipboard?.paste != true
    }

    public var userFacingError: String { lastError ?? "" }

    private let configuration: DeskLinkRuntimeConfiguration
    private let handleOwner = DeskLinkHandleOwner()
    /// Presentation-order decoder used by display-switch UX; frame admission
    /// for input contexts flows through `decodeScheduler` instead.
    private let decoder = H264Decoder()
    private lazy var decodeScheduler = H264DecodeScheduler(
        onFrame: { [weak self] frame in self?.consumeDecodedFrame(frame) },
        onConfiguration: { [weak self] generation, streamID, inputEpoch, version, width, height in
            self?.consumeDecodedConfiguration(generation: generation, streamID: streamID, inputEpoch: inputEpoch,
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
    private var connectingDeviceID: String?
    private var activeStreamID: UInt64 = 0
    private var activeConfigVersion: UInt32 = 0
    private var activeInputEpoch: UInt64 = 0
    private var presentationState = VideoPresentationState()
    private var pendingDecodedConfiguration: DecodedConfiguration?
    private var pendingDecodedFrame: H264DecodedFrame?
    private var pendingDecodeKeyframeRequest: (generation: UInt64, streamID: UInt64)?
    private var highestStreamID: UInt64 = 0
    private var awaitingReconnectHandshake = false
    private var callbackGeneration: UInt64 = 0
    private var awaitingApprovedHostMaterial = false
    private var suspendedForBackground = false
    private var requestKeyframeAfterNextVideoConfig = false
    private var displayTimeoutTask: Task<Void, Never>?
    private var qualityTimeoutTask: Task<Void, Never>?
    private var clipboardTimeoutTask: Task<Void, Never>?
    private var displayDeadline: Date?
    private var qualityDeadline: Date?
    private var displaySwitchTarget: UInt32?
    private var displaySwitchConfirmed = false
    private var minimumDisplayConfigVersion: UInt32?
    private var displaySwitchHasNewConfiguration = false
    private var displaySwitchHasPresentedFrame = false
    private var highestVideoConfigVersion: UInt32 = 0
    private struct VideoConfiguration {
        let sequenceHeader: Data
        let width: UInt16
        let height: UInt16
        let version: UInt32
    }
    private var lastVideoConfiguration: VideoConfiguration?
    private var displayConfigurationCache: [UInt32: VideoConfiguration] = [:]
    private var displayFallbackConfiguration: VideoConfiguration?
    private var acceptedFallbackConfigVersion: UInt32?
    private var nextClipboardRequestID: UInt64 = 0
    private struct PendingClipboard {
        let requestID: UInt64
        let streamID: UInt64
        let generation: UInt64
        let operation: String
        let paste: Bool
        let deadline: Date
    }
    private var pendingClipboard: PendingClipboard?
    private static let featureTimeout: TimeInterval = 20
    public static let clipboardTextMaximumBytes = 49_152
    public static let committedTextMaximumBytes = 4_096
#if DEBUG
    public private(set) var releaseAllCallCountForTesting = 0
    public var recordInputsForTesting = false
    public private(set) var sentInputsForTesting: [DesklinkInput] = []
    public private(set) var releasedContextsForTesting: [CapturedInputContext] = []
    private var presentedInputContextFixtureForTesting: CapturedInputContext?
    public var activeConfigVersionForTesting: UInt32 { activeConfigVersion }
    public var activeRuntimeForTesting: Bool { handleOwner.pointer != nil }
    public var activeStreamIDForTesting: UInt64 { activeStreamID }
    public var callbackGenerationForTesting: UInt64 { callbackGeneration }
    public var videoConfigVersionForTesting: UInt32 { decoder.configVersion }
    public var minimumDisplayConfigVersionForTesting: UInt32? { minimumDisplayConfigVersion }
    public var featureRequestForTesting: ((ControllerFeatureRequest) -> Bool)?
    public var inputForTesting: ((DesklinkInput, UInt64) -> Void)?
#endif

    public init(
        configuration: DeskLinkRuntimeConfiguration,
        identityStore: ControllerIdentityStore = ControllerIdentityStore(),
        savedHostStore: SavedHostStore = SavedHostStore()
    ) {
        self.configuration = configuration
        self.identityStore = identityStore
        self.savedHostStore = savedHostStore
        media.performAction = { [weak self] in self?.performMediaAction($0) ?? false }
        decoder.onFrame = { [weak self] pixelBuffer in
            guard let self else { return }
            if self.isAwaitingDisplayFrame && self.displaySwitchHasNewConfiguration {
                self.displaySwitchHasPresentedFrame = true
            }
            self.finishDisplaySwitchIfReady()
        }

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
        if case .connected = state { bridge.connectionStage = .connected }
        if let error { bridge.publishErrorMessage(error) }
        return bridge
    }

    /// Capture once at the original input event. A configuration may have been
    /// admitted on the FFI thread before its main-actor invalidation arrives.
    public func captureInputContext() -> CapturedInputContext? {
#if DEBUG
        if let fixture = presentedInputContextFixtureForTesting { return fixture }
#endif
        guard let context = presentedInputContext, acceptsInputContext(context),
              decodeScheduler.acceptsInputSurface(generation: context.runtimeGeneration, streamID: context.streamID,
                                                  inputEpoch: context.inputEpoch) else { return nil }
        return context
    }

    /// Old captured releases may still enter the current runtime. Never relabel
    /// them with a new epoch/version; the host validates their original context.
    public func acceptsInputContext(_ context: CapturedInputContext) -> Bool {
        guard case .connected = state, handleOwner.pointer != nil else { return false }
        return context.runtimeGeneration == callbackGeneration && context.streamID > 0 && context.inputEpoch > 0
    }

    /// Called only by the Metal drawable's presented handler, never by decode.
    @discardableResult
    public func confirmVideoFramePresentation(_ token: VideoFramePresentationToken) -> Bool {
        guard acceptsInputContext(token.context), let issued = videoFrame,
              issued.token.context == token.context,
              decodeScheduler.accepts(generation: token.context.runtimeGeneration, streamID: token.context.streamID,
                                      version: token.context.configVersion, inputEpoch: token.context.inputEpoch),
              presentationState.confirm(token, current: issued.token.context, newestIssuedFrameID: issued.token.frameID) else { return false }
        videoSize = token.videoSize
        presentedInputContext = token.context
        return true
    }

    private func clearPresentedInputContext() {
        presentationState.reset()
        presentedInputContext = nil
#if DEBUG
        presentedInputContextFixtureForTesting = nil
#endif
    }

    public func connect(invite: Data) {
        guard invite.count == Int(DESKLINK_PAIRING_INVITE_BYTES) else {
            publishErrorMessage("The pairing invitation is invalid.")
            return
        }
        prepareConnection()
        connectingDeviceID = nil
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
        // Access passwords accept any characters; surrounding whitespace is
        // trimmed but the typed bytes are otherwise used verbatim because both
        // peers derive the directory code from the exact same password.
        let passwordBytes = Array(
            temporaryPassword.trimmingCharacters(in: .whitespacesAndNewlines).utf8)

        guard normalizedDeviceID.count == 12,
              let publicDeviceID = UInt64(normalizedDeviceID),
              (8...64).contains(passwordBytes.count)
        else {
            publishErrorMessage("设备 ID 无效，或访问密码长度需在 8 到 64 个字符之间。")
            return
        }

        prepareConnection()
        connectingDeviceID = normalizedDeviceID
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
                access_code: nil,
                access_code_len: 0,
                controller_device_id: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
                controller_secret_key: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
            )
            let password = passwordBytes
            password.withUnsafeBufferPointer { passwordBuffer in
                config.access_code = passwordBuffer.baseAddress
                config.access_code_len = password.count
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
        guard savedHost.canReconnect else {
            publishErrorMessage("已保存的授权已过期或缺少有效期限，请重新配对。")
            return
        }
        prepareConnection()
        connectingDeviceID = savedHost.deviceID
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
            config.expires_at_unix_s = savedHost.expiresAtUnixS ?? 0
            config.authorization_type = savedHost.authorizationType ?? 0
            config.directory_device_id = savedHost.directoryDeviceID ?? 0
            if let accessCode = savedHost.directoryAccessCode {
                withUnsafeMutableBytes(of: &config.directory_access_code) { $0.copyBytes(from: accessCode) }
            }
            let result = savedHost.serverName.withCString { serverName in
                config.server_name = serverName
                return desklink_connect_secure(handle, &config)
            }
            guard result == DESKLINK_OK else {
                publishResultError("The saved host could not be reached.", result: result)
                return
            }
            awaitingApprovedHostMaterial = true
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
        // Prefer the captured presentation context; fall back to the
        // stream-scoped lane when no surface has been presented yet.
        if let context = captureInputContext() {
            send(input: command, context: context)
            return
        }
        guard canSendRemoteInput else { return }
        switch command {
        case let .move(x, y):
            var input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_MOVE, x: x, y: y, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0, stream_id: activeStreamID, input_epoch: 0)
            sendInputForStream(&input)
        case let .mouseButton(button, pressed):
            var input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_BUTTON, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: button.rawValue + 1, key_code: 0, character: 0, pressed: pressed ? 1 : 0, modifiers: 0, stream_id: activeStreamID, input_epoch: 0)
            sendInputForStream(&input)
        case let .wheel(x, y):
            var input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_WHEEL, x: 0, y: 0, wheel_x: x, wheel_y: y, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0, stream_id: activeStreamID, input_epoch: 0)
            sendInputForStream(&input)
        case let .key(code, pressed, modifiers):
            var input = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: code, character: 0, pressed: pressed ? 1 : 0, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f), stream_id: activeStreamID, input_epoch: 0)
            sendInputForStream(&input)
        case let .character(scalar, pressed, modifiers):
            var input = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: scalar, pressed: pressed ? 1 : 0, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f), stream_id: activeStreamID, input_epoch: 0)
            sendInputForStream(&input)
        case let .unicode(text, modifiers):
            if modifiers.isEmpty {
                _ = sendText(text)
                return
            }
            for scalar in text.unicodeScalars {
                var characterInput = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: scalar.value, pressed: 1, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f), stream_id: activeStreamID, input_epoch: 0)
                sendInputForStream(&characterInput)
                characterInput.pressed = 0
                sendInputForStream(&characterInput)
            }
        }
    }

    public func send(captured input: CapturedRemoteInput) {
        send(input: input.command, context: input.context)
    }

    public func send(input command: RemoteInputCommand, context: CapturedInputContext) {
        guard context.streamID > 0, context.inputEpoch > 0 else { return }
        var input: DesklinkInput
        switch command {
        case let .move(x, y):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_MOVE, x: x, y: y, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0, stream_id: context.streamID, input_epoch: context.inputEpoch)
        case let .mouseButton(button, pressed):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_BUTTON, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: button.rawValue + 1, key_code: 0, character: 0, pressed: pressed ? 1 : 0, modifiers: 0, stream_id: context.streamID, input_epoch: context.inputEpoch)
        case let .wheel(x, y):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_WHEEL, x: 0, y: 0, wheel_x: x, wheel_y: y, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0, stream_id: context.streamID, input_epoch: context.inputEpoch)
        case let .key(code, pressed, modifiers):
            input = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: code, character: 0, pressed: pressed ? 1 : 0, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f), stream_id: context.streamID, input_epoch: context.inputEpoch)
        case let .character(scalar, pressed, modifiers):
            input = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: scalar, pressed: pressed ? 1 : 0, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f), stream_id: context.streamID, input_epoch: context.inputEpoch)
        case let .unicode(text, modifiers):
            var previousWasCR = false
            for scalar in text.unicodeScalars {
                if scalar.value == 10 && previousWasCR { previousWasCR = false; continue }
                previousWasCR = scalar.value == 13
                let specialKey: UInt32 = scalar.value == 10 || scalar.value == 13 ? 1 : (scalar.value == 9 ? 4 : 0)
                var characterInput = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: specialKey, character: specialKey == 0 ? scalar.value : 0, pressed: 1, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue & 0x0f), stream_id: context.streamID, input_epoch: context.inputEpoch)
                sendInput(&characterInput, context: context)
                characterInput.pressed = 0
                sendInput(&characterInput, context: context)
            }
            return
        }
        sendInput(&input, context: context)
    }

    public func releaseAll() {
#if DEBUG
        releaseAllCallCountForTesting += 1
#endif
        guard let handle = handleOwner.pointer else { return }
        _ = desklink_release_all(handle)
    }

    public func releaseAll(context: CapturedInputContext) {
#if DEBUG
        releaseAllCallCountForTesting += 1
        if recordInputsForTesting { releasedContextsForTesting.append(context) }
#endif
        guard context.runtimeGeneration == callbackGeneration,
              context.streamID > 0, context.inputEpoch > 0,
              let handle = handleOwner.pointer else { return }
        _ = desklink_release_all_for_context(handle, context.streamID, context.inputEpoch)
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
            connectionStage = .reconnecting
        }
    }

    /// Reconnects once after a background suspension. A saved host is kept in
    /// Keychain independently of this in-memory reference; the reference is
    /// only the session that should resume automatically.
    public func resumeFromBackground() {
        guard suspendedForBackground else { return }
        suspendedForBackground = false
        guard let savedHost = savedHostForResume else {
            if state == .reconnecting {
                state = .closed
                connectionStage = .closed
            }
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
        connectionStage = .closed
    }

    private func prepareConnection() {
        // A new connection owns a new callback context, even when the previous
        // worker has already closed. Queued callbacks cannot cross attempts.
        if handleOwner.pointer != nil { disconnect() }
        resetDisplayedSessionState()
        awaitingApprovedHostMaterial = false
        connectionAttemptStartedAt = Date()
        connectionStage = .creatingSession
        state = .connecting
        lastError = nil
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
        recoverable: Bool = false,
        generation: UInt64
    ) {
        guard acceptsCallback(generation: generation) else { return }
        switch eventKind {
        case Int(DESKLINK_EVENT_STATE.rawValue):
            consumeState(streamID: streamID, stateValue: stateValue)
        case Int(DESKLINK_EVENT_ERROR.rawValue):
            guard streamID == 0 || streamID == activeStreamID else { return }
            let message = String(decoding: data, as: UTF8.self)
            if recoverable {
                // Recoverable transport errors keep the runtime and the current
                // presentation surface. Input retirement belongs to the worker's
                // explicit reconnect boundary (STATE Reconnecting).
                state = .reconnecting
                connectionStage = .reconnecting
                lastError = errorCode == 2 ? "正在恢复画面，请稍候。" : "连接已中断，正在自动重连。"
            } else {
                publishErrorMessage(message.isEmpty ? "The connection reported an error." : message)
            }
        case Int(DESKLINK_EVENT_CURSOR.rawValue):
            guard streamID == activeStreamID, !data.isEmpty else { return }
            cursorOverlay = CursorOverlay(streamID: streamID, encodedUpdate: data)
        case Int(DESKLINK_EVENT_CONTROL.rawValue):
            latestControlMessage = data
            consumeControl(data)
        case Int(DESKLINK_EVENT_TRANSFER.rawValue): latestTransferMessage = data
        case Int(DESKLINK_EVENT_AUDIO.rawValue):
            guard streamID == activeStreamID else { return }
            latestAudioPacket = data
        case Int(DESKLINK_EVENT_CAPABILITIES.rawValue):
            guard let value = SessionCapabilities(encoded: data) else { return }
            resetControlState()
            capabilities = value
            remotePlatform = value.platform
        case Int(DESKLINK_EVENT_SESSION.rawValue):
            consumeSessionFeature(data: data, streamID: streamID)
            media.consumeSession(data, streamID: streamID)
        case Int(DESKLINK_EVENT_CLIPBOARD.rawValue):
            consumeClipboardFeature(data: data, streamID: streamID)
        case Int(DESKLINK_EVENT_AUDIO_PCM.rawValue):
            media.consumePCM(data, streamID: streamID, sequence: frameID, sampleRate: width, channels: height)
        case Int(DESKLINK_EVENT_FILE.rawValue):
            media.consumeFile(data, streamID: streamID)
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
        clearPresentedInputContext()
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
        guard let stage = ControllerConnectionStage(rawValue: stateValue) else { return }
        if stage != .connected {
            guard streamID == 0 || streamID == activeStreamID else { return }
        }
        switch stateValue {
        case Int(DESKLINK_CONNECTED.rawValue):
            let current = decodeScheduler.currentInputContext(generation: callbackGeneration)
            if awaitingReconnectHandshake, current.map({ $0.streamID == streamID }) == true {
                // A freshly admitted decode surface proves the successor
                // connection even before its handshake notification arrives.
                awaitingReconnectHandshake = false
            }
            guard !awaitingReconnectHandshake, streamID > 0,
                  current.map({ $0.streamID == streamID })
                    ?? (streamID == activeStreamID || streamID > highestStreamID)
            else { return }
            if activeStreamID != streamID {
                // Configuration/frames may already be admitted by the FFI
                // callback. configure owns their stream barrier; do not clear it
                // from this delayed main-actor state notification.
                activeConfigVersion = 0
                activeInputEpoch = 0
                clearPresentedInputContext()
                resetSessionFeatures()
                decoder.reset()
                activeStreamID = streamID
                metrics.lastFrameID = nil
                videoSize = nil
            }
            highestStreamID = max(highestStreamID, streamID)
            state = .connected(streamID: streamID)
            media.attach(streamID: streamID)
            lastError = nil
            applyPendingDecodedConfiguration()
            stageApprovedHostMaterialIfNeeded()
            updateSavedHostConnectionDate()
            if case .failed = state { return }
        case Int(DESKLINK_CLOSED.rawValue):
            guard streamID == 0 || activeStreamID == 0 || streamID == activeStreamID else { return }
            decodeScheduler.reset(generation: callbackGeneration)
            activeConfigVersion = 0
            activeInputEpoch = 0
            clearPresentedInputContext()
            videoFrame = nil
            pendingDecodedConfiguration = nil
            pendingDecodedFrame = nil
            pendingDecodeKeyframeRequest = nil
            retireActiveStream()
            state = .closed
        case Int(DESKLINK_WAITING_FOR_APPROVAL.rawValue):
            clearPresentedInputContext()
            if state != .reconnecting { state = .pairing }
        case Int(DESKLINK_IDLE.rawValue):
            clearPresentedInputContext()
            state = .idle
        case Int(DESKLINK_DEGRADED.rawValue):
            clearPresentedInputContext()
            state = .frozen
        case Int(DESKLINK_RECOVERING_VIDEO.rawValue):
            clearPresentedInputContext()
            state = .recovering
        case Int(DESKLINK_SECURE_HANDSHAKE.rawValue):
            clearPresentedInputContext()
            if streamID == 0, awaitingReconnectHandshake {
                // The worker has retired run_connected before emitting this
                // stage for its next connection. All worker callbacks enqueue
                // onto the main queue in order, so this is a real new epoch.
                // A restarted host may reuse a lower or identical stream ID.
                highestStreamID = 0
                awaitingReconnectHandshake = false
            }
            if state != .reconnecting { state = .connecting }
        case Int(DESKLINK_NEGOTIATING_CAPABILITIES.rawValue), Int(DESKLINK_STARTING_VIDEO.rawValue):
            // Keep the existing session view and its viewport/fullscreen state
            // while reconnecting; connectionStage still reports real progress.
            if state != .reconnecting { state = .connecting }
        case Int(DESKLINK_RECONNECTING.rawValue), Int(DESKLINK_DISCONNECTING.rawValue):
            // The FFI callback already retired the old decode stream. A newer
            // configuration/frame may reach this actor before this notification;
            // never reset that future work from a delayed main-queue event.
            if let context = presentedInputContext,
               decodeScheduler.acceptsInputSurface(generation: context.runtimeGeneration,
                                                   streamID: context.streamID, inputEpoch: context.inputEpoch) {
                // The presented surface survived the same-generation retry;
                // keep the live session exactly as the runtime delivered it.
                return
            }
            inputRestriction = nil
            if savedHostForResume != nil { awaitingApprovedHostMaterial = true }
            if let context = presentedInputContext { clearPresentedInputContext() }
            if let pending = pendingDecodedConfiguration,
               !decodeScheduler.accepts(generation: pending.generation, streamID: pending.streamID,
                                        version: pending.version, inputEpoch: pending.inputEpoch) {
                pendingDecodedConfiguration = nil
            }
            if let pending = pendingDecodedFrame,
               !decodeScheduler.accepts(generation: pending.generation, streamID: pending.streamID,
                                        version: pending.version, inputEpoch: pending.inputEpoch) {
                pendingDecodedFrame = nil
            }
            pendingDecodeKeyframeRequest = nil
            retireActiveStream()
            activeConfigVersion = 0
            activeInputEpoch = 0
            state = .reconnecting
        default:
            clearPresentedInputContext()
            state = .connecting
        }
        connectionStage = stage
    }

    private func retireActiveStream() {
        if activeStreamID != 0 { releaseAll() }
        resetSessionFeatures()
        decoder.reset()
        activeStreamID = 0
        metrics.lastFrameID = nil
        latestAccessUnit = nil
        latestPixelBuffer = nil
        videoSize = nil
        cursorOverlay = nil
        awaitingReconnectHandshake = true
        // Keep the high-water mark and reject all Connected events until the
        // native worker reaches a new handshake (or the runtime is retired).
    }

    private func consumeControl(_ data: Data) {
        guard let control = SessionControlState(encoded: data) else { return }
        switch control {
        case let .displays(displays, activeID):
            remoteDisplays = displays
            activeDisplayID = activeID
            displayStateRevision &+= 1
        case let .audio(available, enabled):
            audioAvailable = available
            audioEnabled = enabled
        case let .videoQuality(quality):
            confirmedQuality = quality
            qualityStateRevision &+= 1
        case let .inputRestriction(reason):
            // Input permission can recover independently of the media stream.
            inputRestriction = reason
        }
    }

    private func resetControlState() {
        remoteDisplays = []
        activeDisplayID = nil
        confirmedQuality = nil
        inputRestriction = nil
        audioAvailable = false
        audioEnabled = false
        qualityStateRevision &+= 1
        displayStateRevision &+= 1
    }

    private func consumeDecodedConfiguration(generation: UInt64, streamID: UInt64, inputEpoch: UInt64,
                                             version: UInt32, width: UInt16, height: UInt16) {
        guard acceptsCallback(generation: generation),
              decodeScheduler.accepts(generation: generation, streamID: streamID, version: version, inputEpoch: inputEpoch) else { return }
        if let presented = presentedInputContext,
           presented.runtimeGeneration != generation || presented.streamID != streamID || presented.inputEpoch != inputEpoch {
            clearPresentedInputContext()
        }
        // Decoder delivery uses a MainActor task while Rust state uses main.async;
        // neither mechanism orders the other. Retain one config until Connected
        // arrives instead of losing a config delivered first.
        pendingDecodedConfiguration = DecodedConfiguration(generation: generation, streamID: streamID, inputEpoch: inputEpoch,
                                                           version: version, width: width, height: height)
        applyPendingDecodedConfiguration()
    }

    private func applyPendingDecodedConfiguration() {
        guard let pending = pendingDecodedConfiguration, pending.streamID == activeStreamID,
              acceptsCallback(generation: pending.generation),
              decodeScheduler.accepts(generation: pending.generation, streamID: pending.streamID, version: pending.version, inputEpoch: pending.inputEpoch) else { return }
        if let minimum = minimumDisplayConfigVersion,
           pending.version < minimum,
           !(displaySwitchConfirmed && pending.version == acceptedFallbackConfigVersion) {
            // Belongs to the abandoned screen; keep the confirmed surface.
            pendingDecodedConfiguration = nil
            metrics.droppedFrames &+= 1
            return
        }
        pendingDecodedConfiguration = nil
        if activeInputEpoch != pending.inputEpoch { metrics.lastFrameID = nil }
        activeConfigVersion = pending.version
        activeInputEpoch = pending.inputEpoch
        highestVideoConfigVersion = max(highestVideoConfigVersion, pending.version)
        if !isAwaitingDisplayFrame, let selectedDisplayID {
            displayConfigurationCache[selectedDisplayID] = VideoConfiguration(
                sequenceHeader: Data(), width: pending.width, height: pending.height, version: pending.version)
            if let minimumDisplayConfigVersion, pending.version >= minimumDisplayConfigVersion {
                self.minimumDisplayConfigVersion = nil
                acceptedFallbackConfigVersion = nil
            }
        }
        if isAwaitingDisplayFrame { displaySwitchHasNewConfiguration = true }
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
              decodeScheduler.accepts(generation: frame.generation, streamID: frame.streamID, version: frame.version, inputEpoch: frame.inputEpoch) else { return }
        guard activeStreamID == frame.streamID, activeConfigVersion == frame.version, activeInputEpoch == frame.inputEpoch else {
            pendingDecodedFrame = frame
            return
        }
        guard frame.frameID > (metrics.lastFrameID ?? 0) else { return }
        let context = CapturedInputContext(streamID: frame.streamID, inputEpoch: frame.inputEpoch,
                                           configVersion: frame.version, runtimeGeneration: frame.generation)
        let token = VideoFramePresentationToken(context: context, frameID: frame.frameID, width: frame.width, height: frame.height)
        latestPixelBuffer = frame.pixelBuffer
        videoSize = CGSize(width: Int(frame.width), height: Int(frame.height))
        videoFrame = PresentedVideoFrame(pixelBuffer: frame.pixelBuffer, token: token)
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
              decodeScheduler.accepts(generation: pending.generation, streamID: pending.streamID, version: activeConfigVersion, inputEpoch: activeInputEpoch) else { return }
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
            hostVerifyKey: withUnsafeBytes(of: material.host_verify_key) { Array($0) },
            expiresAtUnixS: material.expires_at_unix_s,
            authorizationType: material.authorization_type,
            directoryDeviceID: material.directory_device_id == 0 ? nil : material.directory_device_id,
            directoryAccessCode: material.directory_device_id == 0 ? nil : withUnsafeBytes(of: material.directory_access_code) { Array($0) },
            lastConnectedAt: Date()
        )
        do {
            let hostKey = withUnsafeBytes(of: material.host_verify_key) { Array($0) }
            let previous = try savedHostStore.loadAll().first {
                $0.serverName == serverName && $0.hostVerifyKey == hostKey
            }
            let host = SavedHost(
                id: previous?.id ?? UUID(),
                serverName: serverName,
                sessionID: withUnsafeBytes(of: material.session_id) { Array($0) },
                relayAuthentication: withUnsafeBytes(of: material.relay_authentication) { Array($0) },
                hostVerifyKey: hostKey,
                deviceID: connectingDeviceID ?? previous?.deviceID,
                displayName: previous?.displayName,
                lastConnectedAt: Date()
            )
            try savedHostStore.save(host)
            savedHostForResume = host
            awaitingApprovedHostMaterial = false
        } catch {
            publishErrorMessage("The approved host could not be saved securely.")
        }
    }

    /// Stream-scoped sender for flows without a captured presentation context;
    /// Rust derives the validated geometry from the session gate.
    private func sendInputForStream(_ input: inout DesklinkInput) {
#if DEBUG
        if let inputForTesting {
            inputForTesting(input, activeStreamID)
            return
        }
        if recordInputsForTesting {
            sentInputsForTesting.append(input)
            return
        }
#endif
        guard let handle = handleOwner.pointer else { return }
        let result = desklink_send_input_for_stream(handle, activeStreamID, &input)
        guard result != DESKLINK_INVALID_ARGUMENT else { return }
        if result != DESKLINK_OK {
            lastError = "暂时无法发送输入，请稍后重试。"
        }
    }

    private func updateSavedHostConnectionDate() {
        guard let previous = savedHostForResume,
              (previous.lastConnectedAt ?? .distantPast) < (connectionAttemptStartedAt ?? .distantPast)
        else { return }
        let host = SavedHost(
            id: previous.id, serverName: previous.serverName, sessionID: previous.sessionID,
            relayAuthentication: previous.relayAuthentication, hostVerifyKey: previous.hostVerifyKey,
            deviceID: previous.deviceID, displayName: previous.displayName, lastConnectedAt: Date()
        )
        do {
            try savedHostStore.save(host)
            savedHostForResume = host
        } catch {
            lastError = "已连接，但无法更新最近设备的连接时间。"
        }
    }

    private func sendInput(_ input: inout DesklinkInput, context: CapturedInputContext) {
#if DEBUG
        if let inputForTesting {
            inputForTesting(input, context.streamID)
            return
        }
        if recordInputsForTesting { sentInputsForTesting.append(input) }
#endif
        guard acceptsInputContext(context), let handle = handleOwner.pointer else { return }
        let result = desklink_send_input(handle, &input)
        guard result != DESKLINK_INVALID_ARGUMENT else { return }
        if result != DESKLINK_OK {
            lastError = "暂时无法发送输入，请稍后重试。"
        }
    }

    private func performMediaAction(_ action: ControllerMediaAction) -> Bool {
        switch state { case .connected, .recovering, .frozen: break; default: return false }
        guard let handle = handleOwner.pointer else { return false }
        switch action {
        case let .setAudio(stream, enabled):
            guard stream == activeStreamID else { return false }
            return desklink_set_audio_enabled(handle, stream, enabled ? 1 : 0) == DESKLINK_OK
        case let .sendFile(stream, id, path):
            guard stream == activeStreamID, id.count == 16 else { return false }
            return id.withUnsafeBufferPointer { idBytes in
                Data(path.utf8).withUnsafeBytes { bytes in
                    desklink_send_file(handle, stream, idBytes.baseAddress,
                        bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count) == DESKLINK_OK
                }
            }
        case let .requestFile(stream, requestID, directory):
            guard stream == activeStreamID else { return false }
            return Data(directory.utf8).withUnsafeBytes { bytes in
                desklink_request_remote_file(handle, stream, requestID,
                    bytes.bindMemory(to: UInt8.self).baseAddress, bytes.count) == DESKLINK_OK
            }
        case let .cancelFile(stream):
            guard stream == activeStreamID else { return false }
            return desklink_cancel_file_transfer(handle, stream) == DESKLINK_OK
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
        resetControlState()
        capabilities = nil
        remotePlatform = .unknown
        latestControlMessage = nil
        latestTransferMessage = nil
        latestAudioPacket = nil
        decodeScheduler.reset(generation: callbackGeneration)
        activeConfigVersion = 0
        activeInputEpoch = 0
        clearPresentedInputContext()
        videoFrame = nil
        pendingDecodedConfiguration = nil
        pendingDecodedFrame = nil
        pendingDecodeKeyframeRequest = nil
        resetSessionFeatures()
        decoder.reset()
        activeStreamID = 0
        highestStreamID = 0
        awaitingReconnectHandshake = false
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
        connectionStage = .closed
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
    public func acceptControlForTesting(_ data: Data) { consumeControl(data) }

    /// Explicit metadata fixture for input unit tests only. This does not create
    /// a native runtime or establish that any drawable was actually presented.
    public func acceptPresentedInputContextForTesting(_ context: CapturedInputContext) {
        guard handleOwner.pointer == nil else { return }
        presentedInputContextFixtureForTesting = context
        presentedInputContext = context
    }

    /// Exercises UI state ordering only; the invalid header cannot configure a
    /// native decoder and is not evidence of video or hardware acceptance.
    public func stageDecodedMetadataForTesting(streamID: UInt64, version: UInt32, inputEpoch: UInt64 = 1) {
        decodeScheduler.configure(sequenceHeader: Data([1]), streamID: streamID, inputEpoch: inputEpoch,
                                  width: 1920, height: 1080, version: version, generation: callbackGeneration)
        consumeDecodedConfiguration(generation: callbackGeneration, streamID: streamID, inputEpoch: inputEpoch,
                                    version: version, width: 1920, height: 1080)
    }

    public func retireDecodeStreamForTesting() { decodeScheduler.retireStream(generation: callbackGeneration) }

    public func acceptReconnectingForTesting() {
        consumeState(streamID: 0, stateValue: Int(DESKLINK_RECONNECTING.rawValue))
    }

    public func acceptCapabilitiesForTesting(platform: RemotePlatform) {
        remotePlatform = platform
    }

    /// Drives the real terminal-error path: raw message → redaction →
    /// user-facing mapping → `.failed` state.
    public func publishErrorForTesting(_ message: String) {
        publishErrorMessage(message)
    }

    public func acceptErrorForTesting(stateValue: Int, recoverable: Bool) {
        consume(eventKind: 2, data: Data("transport unavailable".utf8), streamID: 0,
                frameID: 0, configVersion: 0, width: 0, height: 0,
                stateValue: stateValue, errorCode: recoverable ? 1 : 3, recoverable: recoverable,
                generation: callbackGeneration)
    }

    public func presentFrameForTesting(_ pixelBuffer: CVPixelBuffer, streamID: UInt64, cursor: CursorOverlay? = nil) {
        activeStreamID = streamID
        highestStreamID = max(highestStreamID, streamID)
        state = .connected(streamID: streamID)
        media.attach(streamID: streamID)
        connectionStage = .connected
        latestPixelBuffer = pixelBuffer
        videoSize = CGSize(width: CVPixelBufferGetWidth(pixelBuffer), height: CVPixelBufferGetHeight(pixelBuffer))
        cursorOverlay = cursor
        if isAwaitingDisplayFrame { displaySwitchHasNewConfiguration = true }
        if isAwaitingDisplayFrame { displaySwitchHasPresentedFrame = true }
        finishDisplaySwitchIfReady()
    }

    public func acceptStateForTesting(streamID: UInt64) {
        consumeState(streamID: streamID, stateValue: Int(DESKLINK_CONNECTED.rawValue))
    }

    public func acceptEventForTesting(
        eventKind: Int, stateValue: Int, streamID: UInt64 = 0,
        message: String = "", generation: UInt64? = nil
    ) {
        // Mirror the FFI error metadata contract: reconnect/recovery states
        // carry recoverable=1 instead of a fatal terminal error.
        let recoverable = eventKind == Int(DESKLINK_EVENT_ERROR.rawValue) && [
            DESKLINK_RECONNECTING, DESKLINK_STARTING_VIDEO,
            DESKLINK_RECOVERING_VIDEO, DESKLINK_DEGRADED,
        ].contains { stateValue == Int($0.rawValue) }
        consume(
            eventKind: eventKind, data: Data(message.utf8), streamID: streamID, frameID: 0,
            configVersion: 0, width: 0, height: 0, stateValue: stateValue,
            errorCode: recoverable ? 1 : 3, recoverable: recoverable,
            generation: generation ?? callbackGeneration
        )
    }

    public func expireFeatureRequestsForTesting(now: Date = .distantFuture) {
        expireFeatureRequests(now: now)
    }

    public func confirmDisplayFrameForTesting() {
        displaySwitchHasNewConfiguration = true
        displaySwitchHasPresentedFrame = true
        finishDisplaySwitchIfReady()
    }

    public func acceptVideoConfigurationForTesting(_ data: Data, version: UInt32, width: UInt16 = 1920, height: UInt16 = 1080) {
        if let minimum = minimumDisplayConfigVersion,
           version < minimum, !(displaySwitchConfirmed && version == acceptedFallbackConfigVersion) {
            metrics.droppedFrames &+= 1
            return
        }
        _ = decoder.configure(sequenceHeader: data, width: width, height: height, version: version)
        videoSize = CGSize(width: Int(width), height: Int(height))
        highestVideoConfigVersion = max(highestVideoConfigVersion, version)
        lastVideoConfiguration = VideoConfiguration(sequenceHeader: data, width: width, height: height, version: version)
        if !isAwaitingDisplayFrame, let selectedDisplayID {
            displayConfigurationCache[selectedDisplayID] = lastVideoConfiguration
            if let minimumDisplayConfigVersion, version >= minimumDisplayConfigVersion {
                self.minimumDisplayConfigVersion = nil
                acceptedFallbackConfigVersion = nil
            }
        }
        if isAwaitingDisplayFrame { displaySwitchHasNewConfiguration = true }
    }

    public func acceptPCMForTesting(_ data: Data, streamID: UInt64, sequence: UInt64) {
        consume(eventKind: Int(DESKLINK_EVENT_AUDIO_PCM.rawValue), data: data, streamID: streamID,
                frameID: sequence, configVersion: 0, width: 48_000, height: 1,
                stateValue: Int(DESKLINK_CONNECTED.rawValue), generation: callbackGeneration)
    }
#endif
}

@MainActor
public extension ControllerBridge {
    /// Submit a request while keeping the actual selected display unchanged
    /// until the peer confirms it. Coordinates remain disabled until a frame
    /// from a newer encoder configuration has actually been presented.
    @discardableResult
    func selectDisplay(_ displayID: UInt32) -> Bool {
        guard case .connected = state, activeStreamID > 0,
              pendingDisplayID == nil, pendingQuality == nil, displays.contains(where: { $0.id == displayID }),
              displayID != selectedDisplayID || isAwaitingDisplayFrame
        else { return false }
        let previousSwitch = (awaiting: isAwaitingDisplayFrame, target: displaySwitchTarget,
            confirmed: displaySwitchConfirmed, minimumVersion: minimumDisplayConfigVersion,
            newConfiguration: displaySwitchHasNewConfiguration, presentedFrame: displaySwitchHasPresentedFrame,
            fallback: displayFallbackConfiguration, acceptedFallbackVersion: acceptedFallbackConfigVersion)
        let retryingSameTarget = previousSwitch.awaiting && previousSwitch.target == displayID
        releaseAll()
        cancelClipboard(message: "屏幕切换已取消剪贴板操作。")
        inputContextID &+= 1
        pendingDisplayID = displayID
        displaySwitchTarget = displayID
        displaySwitchConfirmed = false
        let previousVersion = max(decoder.configVersion, highestVideoConfigVersion)
        minimumDisplayConfigVersion = retryingSameTarget ? previousSwitch.minimumVersion
            : (previousVersion == .max ? .max : previousVersion + 1)
        displaySwitchHasNewConfiguration = retryingSameTarget && previousSwitch.newConfiguration
        displaySwitchHasPresentedFrame = false
        displayFallbackConfiguration = displayConfigurationCache[displayID]
        acceptedFallbackConfigVersion = retryingSameTarget ? previousSwitch.acceptedFallbackVersion : nil
        isAwaitingDisplayFrame = true
        displayError = nil
        let accepted: Bool
#if DEBUG
        if let featureRequestForTesting {
            accepted = featureRequestForTesting(.selectDisplay(streamID: activeStreamID, displayID: displayID))
        } else {
            accepted = handleOwner.pointer.map { desklink_select_display($0, activeStreamID, displayID) == DESKLINK_OK } ?? false
        }
#else
        accepted = handleOwner.pointer.map { desklink_select_display($0, activeStreamID, displayID) == DESKLINK_OK } ?? false
#endif
        guard accepted else {
            pendingDisplayID = nil
            displayError = "无法发送屏幕切换请求，请重试。"
            displaySwitchTarget = previousSwitch.target
            displaySwitchConfirmed = previousSwitch.confirmed
            minimumDisplayConfigVersion = previousSwitch.minimumVersion
            displaySwitchHasNewConfiguration = previousSwitch.newConfiguration
            displaySwitchHasPresentedFrame = previousSwitch.presentedFrame
            displayFallbackConfiguration = previousSwitch.fallback
            acceptedFallbackConfigVersion = previousSwitch.acceptedFallbackVersion
            isAwaitingDisplayFrame = previousSwitch.awaiting
            return false
        }
        // The synchronous enqueue cannot deliver callbacks on this actor
        // before this reset. Retire asynchronous old frames before yielding.
        if !retryingSameTarget { decoder.reset() }
        latestPixelBuffer = nil
        latestAccessUnit = nil
        cursorOverlay = nil
        if !retryingSameTarget { metrics.lastFrameID = nil }
        displayDeadline = Date().addingTimeInterval(Self.featureTimeout)
        displayTimeoutTask?.cancel()
        displayTimeoutTask = featureTimeoutTask()
        return true
    }

    @discardableResult
    func chooseQuality(_ preference: VideoQualityPreference) -> Bool {
        guard case .connected = state, activeStreamID > 0, pendingQuality == nil,
              !isAwaitingDisplayFrame, pendingDisplayID == nil,
              preference != qualityPreference || appliedQualityPreset == nil || qualityError != nil
        else { return false }
        let accepted: Bool
#if DEBUG
        if let featureRequestForTesting {
            accepted = featureRequestForTesting(.chooseQuality(streamID: activeStreamID, preference: preference))
        } else {
            accepted = handleOwner.pointer.map { desklink_set_video_quality($0, activeStreamID, preference.cValue) == DESKLINK_OK } ?? false
        }
#else
        accepted = handleOwner.pointer.map { desklink_set_video_quality($0, activeStreamID, preference.cValue) == DESKLINK_OK } ?? false
#endif
        guard accepted else { qualityError = "无法发送画质设置，请重试。"; return false }
        pendingQuality = preference
        qualityError = nil
        qualityDeadline = Date().addingTimeInterval(Self.featureTimeout)
        qualityTimeoutTask?.cancel()
        qualityTimeoutTask = featureTimeoutTask()
        return true
    }

    /// One stream-bound, ordered batch preserves a committed IME composition.
    /// Marked text belongs to the platform input method and never reaches here.
    @discardableResult
    func sendText(_ text: String) -> Bool {
        guard canSendRemoteInput, !text.isEmpty, !text.utf8.contains(0),
              text.utf8.count <= Self.committedTextMaximumBytes else { return false }
#if DEBUG
        if let featureRequestForTesting {
            return featureRequestForTesting(.sendText(streamID: activeStreamID, text: text))
        }
#endif
        guard let handle = handleOwner.pointer else { return false }
        return Data(text.utf8).withUnsafeBytes {
            desklink_send_text(handle, activeStreamID, $0.bindMemory(to: UInt8.self).baseAddress, $0.count) == DESKLINK_OK
        }
    }

    @discardableResult
    func sendClipboardText(_ text: String, paste: Bool = false) -> Bool {
        guard let request = beginClipboard(operation: "send", paste: paste) else { return false }
        guard text.utf8.count <= Self.clipboardTextMaximumBytes else {
            cancelClipboard(message: "剪贴板文本过长，最多支持 48 KiB UTF-8 文本。")
            return false
        }
        if paste { releaseAll() }
        let accepted: Bool
#if DEBUG
        if let featureRequestForTesting {
            accepted = featureRequestForTesting(.sendClipboard(streamID: request.streamID, requestID: request.requestID, text: text))
        } else {
            accepted = sendClipboardData(text, request: request)
        }
#else
        accepted = sendClipboardData(text, request: request)
#endif
        guard accepted else { cancelClipboard(message: "无法发送剪贴板文本，请重试。"); return false }
        clipboardStatus = paste ? "正在发送文本，远端确认后粘贴…" : "正在发送剪贴板文本…"
        return true
    }

    @discardableResult
    func requestRemoteClipboard() -> Bool {
        guard let request = beginClipboard(operation: "request", paste: false) else { return false }
        let accepted: Bool
#if DEBUG
        if let featureRequestForTesting {
            accepted = featureRequestForTesting(.requestClipboard(streamID: request.streamID, requestID: request.requestID))
        } else {
            accepted = handleOwner.pointer.map { desklink_request_clipboard_text($0, request.streamID, request.requestID) == DESKLINK_OK } ?? false
        }
#else
        accepted = handleOwner.pointer.map { desklink_request_clipboard_text($0, request.streamID, request.requestID) == DESKLINK_OK } ?? false
#endif
        guard accepted else { cancelClipboard(message: "无法请求远端剪贴板，请重试。"); return false }
        clipboardStatus = "正在接收远端剪贴板文本…"
        return true
    }

    /// A real focus loss must revoke the deferred shortcut. The peer may
    /// finish its clipboard write, but its late acknowledgement can no longer
    /// cause input in a window the user has left. Ordinary explicit clipboard
    /// send/receive operations continue independently of window focus.
    func cancelPendingPaste() {
        guard pendingClipboard?.paste == true else { return }
        cancelClipboard(message: "窗口失去焦点，已取消待执行的远端粘贴。")
    }
}

private extension ControllerBridge {
    private func beginClipboard(operation: String, paste: Bool) -> PendingClipboard? {
        guard canSendRemoteInput, pendingClipboard == nil, nextClipboardRequestID < .max else { return nil }
        nextClipboardRequestID += 1
        let pending = PendingClipboard(requestID: nextClipboardRequestID, streamID: activeStreamID,
            generation: callbackGeneration, operation: operation, paste: paste,
            deadline: Date().addingTimeInterval(Self.featureTimeout))
        pendingClipboard = pending
        isClipboardBusy = true
        clipboardError = nil
        clipboardTimeoutTask?.cancel()
        clipboardTimeoutTask = featureTimeoutTask()
        return pending
    }

    private func sendClipboardData(_ text: String, request: PendingClipboard) -> Bool {
        guard let handle = handleOwner.pointer else { return false }
        return Data(text.utf8).withUnsafeBytes {
            desklink_send_clipboard_text(handle, request.streamID, request.requestID,
                $0.bindMemory(to: UInt8.self).baseAddress, $0.count) == DESKLINK_OK
        }
    }

    func consumeSessionFeature(data: Data, streamID: UInt64) {
        guard streamID > 0, streamID == activeStreamID,
              data.count <= 307_200, let event = try? JSONDecoder().decode(SessionFeatureEvent.self, from: data)
        else { return }
        switch event.kind {
        case "display_list":
            guard let displays = event.displays, displays.count <= 64,
                  Set(displays.map(\.id)).count == displays.count,
                  displays.allSatisfy({ $0.width > 0 && $0.height > 0 && $0.width <= 65_535 && $0.height <= 65_535 }),
                  let selected = event.selectedDisplayID, displays.contains(where: { $0.id == selected })
            else { return }
            self.displays = displays
            selectedDisplayID = selected
            if !isAwaitingDisplayFrame, let lastVideoConfiguration {
                displayConfigurationCache[selected] = lastVideoConfiguration
            }
            if let target = displaySwitchTarget {
                if selected == target {
                    if !displaySwitchConfirmed {
                        // Configuration and control use different lanes. A
                        // cached new configuration is useful, but a frame
                        // presented before the host's selection ack cannot
                        // reopen coordinates. Wait for a presentation after it.
                        displaySwitchHasPresentedFrame = false
                    }
                    displaySwitchConfirmed = true
                    if !displaySwitchHasNewConfiguration,
                       let fallback = displayFallbackConfiguration,
                       decoder.configure(sequenceHeader: fallback.sequenceHeader, width: fallback.width,
                                         height: fallback.height, version: fallback.version) {
                        // Selecting a screen the host already uses may be a
                        // no-op and produce no new encoder version. Reuse only
                        // a configuration previously confirmed for this exact
                        // screen; reject intermediate versions from another
                        // pending screen switch.
                        acceptedFallbackConfigVersion = fallback.version
                        lastVideoConfiguration = fallback
                        videoSize = CGSize(width: Int(fallback.width), height: Int(fallback.height))
                        displaySwitchHasNewConfiguration = true
                    }
                    finishDisplaySwitchIfReady()
                }
            }
        case "video_quality_state":
            guard let preference = event.qualityPreference, let preset = event.appliedPreset else { return }
            qualityPreference = preference
            appliedQualityPreset = preset
            if pendingQuality == preference {
                pendingQuality = nil
                qualityDeadline = nil
                qualityTimeoutTask?.cancel()
                qualityTimeoutTask = nil
                qualityError = nil
            }
        case "operation_error":
            if event.operation == "select_display", pendingDisplayID != nil {
                pendingDisplayID = nil
                displayDeadline = nil
                displayTimeoutTask?.cancel()
                displayTimeoutTask = nil
                displayError = "远端未能切换屏幕，请重试。"
                // Recovery still requires a fresh frame. Old coordinates are
                // unsafe after a request reaches the peer, including failures.
            } else if event.operation == "set_video_quality", pendingQuality != nil {
                pendingQuality = nil
                qualityDeadline = nil
                qualityTimeoutTask?.cancel()
                qualityTimeoutTask = nil
                qualityError = "远端未能应用画质设置，请重试。"
            }
        default: break
        }
    }

    func consumeClipboardFeature(data: Data, streamID: UInt64) {
        guard data.count <= 307_200,
              let request = pendingClipboard,
              request.generation == callbackGeneration, request.streamID == activeStreamID,
              streamID == request.streamID, Date() < request.deadline,
              let event = try? JSONDecoder().decode(ClipboardFeatureEvent.self, from: data),
              event.requestID == request.requestID, event.operation == request.operation
        else { return }
        guard event.status == "completed", event.result == "completed" else {
            guard event.status == "failed" else { return }
            cancelClipboard(message: clipboardFailureMessage(event.result))
            return
        }
        if request.operation == "request" {
            guard let text = event.text, text.utf8.count <= Self.clipboardTextMaximumBytes else {
                cancelClipboard(message: "远端剪贴板文本无效或过长。")
                return
            }
            clearPendingClipboard()
            clipboardStatus = "已接收远端剪贴板文本。"
            onRemoteClipboardText?(text)
        } else {
            clearPendingClipboard()
            clipboardStatus = "剪贴板文本已发送。"
            if request.paste, canSendRemoteInput {
                // The acknowledgement guarantees the remote clipboard write
                // completed before the shortcut is put on the input lane.
                releaseAll()
                // The stream-scoped FFI derives the validated geometry itself.
                var controlDown = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0,
                    button: 0, key_code: DeskLinkKeyCode.control, character: 0, pressed: 1, modifiers: 0,
                    stream_id: activeStreamID, input_epoch: 0)
                var vDown = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0,
                    button: 0, key_code: 0, character: 118, pressed: 1, modifiers: UInt8(Modifiers.control.rawValue),
                    stream_id: activeStreamID, input_epoch: 0)
                var vUp = vDown; vUp.pressed = 0
                var controlUp = controlDown; controlUp.pressed = 0
                sendInputForStream(&controlDown)
                sendInputForStream(&vDown)
                sendInputForStream(&vUp)
                sendInputForStream(&controlUp)
                // release_all has reserved queue capacity. Even a partially
                // accepted shortcut cannot leave Ctrl or V held remotely.
                releaseAll()
                clipboardStatus = "文本已发送并已请求远端粘贴。"
            }
        }
    }

    func finishDisplaySwitchIfReady() {
        guard isAwaitingDisplayFrame, displaySwitchConfirmed,
              displaySwitchHasNewConfiguration, displaySwitchHasPresentedFrame else { return }
        isAwaitingDisplayFrame = false
        pendingDisplayID = nil
        displaySwitchTarget = nil
        if let selectedDisplayID, let lastVideoConfiguration {
            displayConfigurationCache[selectedDisplayID] = lastVideoConfiguration
        }
        displayFallbackConfiguration = nil
        // A no-op return may reuse an older, known screen configuration.
        // Continue rejecting intermediate versions from the abandoned screen
        // even after input reopens, until a genuinely newer config arrives.
        if acceptedFallbackConfigVersion == nil
            || minimumDisplayConfigVersion.map({ decoder.configVersion >= $0 }) == true {
            acceptedFallbackConfigVersion = nil
            minimumDisplayConfigVersion = nil
        }
        displayDeadline = nil
        displayTimeoutTask?.cancel()
        displayTimeoutTask = nil
        displayError = nil
        inputContextID &+= 1
    }

    func featureTimeoutTask() -> Task<Void, Never> {
        // Sleep uses a monotonic clock. Expire against the original deadline
        // even if the system wall clock moves backwards while awaiting a peer.
        let deadline = Date().addingTimeInterval(Self.featureTimeout)
        return Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.featureTimeout * 1_000_000_000))
            guard !Task.isCancelled else { return }
            self?.expireFeatureRequests(now: deadline)
        }
    }

    func expireFeatureRequests(now: Date) {
        if let deadline = displayDeadline, now >= deadline {
            displayDeadline = nil
            displayTimeoutTask?.cancel(); displayTimeoutTask = nil
            pendingDisplayID = nil
            displayError = "屏幕切换未在 20 秒内确认，请重试。"
            // The menu becomes retryable; input remains gated until the
            // selected screen and its new frame are known to agree.
        }
        if let deadline = qualityDeadline, now >= deadline {
            qualityDeadline = nil
            qualityTimeoutTask?.cancel(); qualityTimeoutTask = nil
            pendingQuality = nil
            qualityError = "画质设置未在 20 秒内确认，请重试。"
        }
        if let pending = pendingClipboard, now >= pending.deadline {
            cancelClipboard(message: "剪贴板操作超时，请重试。")
        }
    }

    func resetSessionFeatures() {
        media.reset()
        displayTimeoutTask?.cancel(); displayTimeoutTask = nil
        qualityTimeoutTask?.cancel(); qualityTimeoutTask = nil
        displayDeadline = nil; qualityDeadline = nil
        pendingDisplayID = nil; selectedDisplayID = nil; displays = []
        displayError = nil; displaySwitchTarget = nil; displaySwitchConfirmed = false
        minimumDisplayConfigVersion = nil
        displaySwitchHasNewConfiguration = false; displaySwitchHasPresentedFrame = false
        highestVideoConfigVersion = 0
        lastVideoConfiguration = nil
        displayConfigurationCache = [:]
        displayFallbackConfiguration = nil
        acceptedFallbackConfigVersion = nil
        isAwaitingDisplayFrame = false
        qualityPreference = .automatic; appliedQualityPreset = nil
        pendingQuality = nil; qualityError = nil
        clearPendingClipboard()
        clipboardStatus = nil; clipboardError = nil
        inputContextID &+= 1
    }

    func clearPendingClipboard() {
        clipboardTimeoutTask?.cancel(); clipboardTimeoutTask = nil
        pendingClipboard = nil
        isClipboardBusy = false
    }

    func cancelClipboard(message: String) {
        guard pendingClipboard != nil else { return }
        clearPendingClipboard()
        clipboardStatus = nil
        clipboardError = message
    }

    func clipboardFailureMessage(_ result: String) -> String {
        switch result {
        case "permission_denied": "远端未授权剪贴板操作。"
        case "unsupported": "远端暂不支持文本剪贴板。"
        case "timeout": "剪贴板操作超时，请重试。"
        case "busy": "远端剪贴板正忙，请稍后重试。"
        case "stale_session": "连接已变化，请重试剪贴板操作。"
        default: "远端未能完成剪贴板操作，请重试。"
        }
    }
}

private func desklinkEventCallback(_ context: UnsafeMutableRawPointer?, _ event: UnsafePointer<DesklinkEvent>?) {
    guard let context, let event else { return }
    let callbackContext = Unmanaged<ControllerCallbackContext>.fromOpaque(context).takeUnretainedValue()
    guard let bridge = callbackContext.bridge else { return }
    let value = event.pointee
    // Legacy raw media/transfer events are kept by the public C ABI. Apple
    // consumes the bounded decoded PCM and file metadata events instead; do
    // not copy bulk file chunks or enqueue ignored tasks on the main actor.
    if value.kind == DESKLINK_EVENT_AUDIO || value.kind == DESKLINK_EVENT_TRANSFER { return }
    if value.kind == DESKLINK_EVENT_AUDIO_PCM {
        guard value.data_len == 960, value.width == 48_000, value.height == 1, let pointer = value.data else { return }
        callbackContext.enqueuePCM(Data(bytes: pointer, count: 960), streamID: value.stream_id, sequence: value.frame_id)
        return
    }
    // State/control callbacks delimit audio ownership. Do not let PCM waiting
    // for the main actor cross a disabled/reconnected acknowledgement.
    if value.kind == DESKLINK_EVENT_STATE || value.kind == DESKLINK_EVENT_SESSION {
        callbackContext.discardQueuedPCM()
    }
    let generation = callbackContext.generation
    if value.kind == DESKLINK_EVENT_STATE, value.state == DESKLINK_RECONNECTING {
        callbackContext.decodeScheduler.retireStream(generation: generation)
    }
    // Admit encoded media on the FFI callback thread before any main-queue
    // scheduling. Data is copied once while the borrowed FFI buffer is alive.
    if value.kind == DESKLINK_EVENT_VIDEO_CONFIG {
        guard value.data_len > 0, value.data_len <= H264DecodeScheduler.maximumSequenceHeaderBytes,
              let pointer = value.data else { return }
        callbackContext.decodeScheduler.configure(sequenceHeader: Data(bytes: pointer, count: value.data_len),
            streamID: value.stream_id, inputEpoch: value.input_epoch, width: value.width, height: value.height,
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
        bridge.consume(
            eventKind: eventKind, data: data, streamID: streamID, frameID: frameID,
            configVersion: configVersion, width: width, height: height, stateValue: stateValue,
            errorCode: errorCode, recoverable: recoverable, generation: generation
        )
    }
}

private extension ControllerBridge {
    func acceptsCallback(generation: UInt64) -> Bool {
        callbackGeneration == generation
    }
}
