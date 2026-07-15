import Combine
import CoreVideo
import DeskLinkC
import Foundation

private final class ControllerCallbackContext: @unchecked Sendable {
    weak var bridge: ControllerBridge?
    let generation: UInt64

    init(bridge: ControllerBridge, generation: UInt64) {
        self.bridge = bridge
        self.generation = generation
    }
}

private final class DeskLinkHandleOwner: @unchecked Sendable {
    var pointer: OpaquePointer?
    var callbackContext: UnsafeMutableRawPointer?

    func destroy() {
        let pointer = self.pointer
        self.pointer = nil
        if let pointer { desklink_destroy(pointer) }
        if let callbackContext {
            self.callbackContext = nil
            Unmanaged<ControllerCallbackContext>.fromOpaque(callbackContext).release()
        }
    }

    deinit { destroy() }
}

struct CursorOverlay: Equatable {
    let streamID: UInt64
    let encodedUpdate: Data
}

@MainActor
final class ControllerBridge: ObservableObject {
    @Published private(set) var state: ConnectionState = .idle
    @Published private(set) var pairing: PairingInfo?
    @Published private(set) var metrics = Metrics()
    @Published private(set) var lastError: String?
    @Published private(set) var latestAccessUnit: Data?
    @Published private(set) var latestPixelBuffer: CVPixelBuffer?
    @Published private(set) var controllerVerifyKeyHex: String?
    @Published private(set) var cursorOverlay: CursorOverlay?

    var userFacingError: String { lastError ?? "" }

    private let relayURL: String
    private let handleOwner = DeskLinkHandleOwner()
    private let decoder = H264Decoder()
    private let identityStore: ControllerIdentityStore
    private let savedHostStore: SavedHostStore
    private var controllerIdentity: ControllerIdentity?
    private var activeStreamID: UInt64 = 0
    private var highestStreamID: UInt64 = 0
    private var callbackGeneration: UInt64 = 0
    private var awaitingApprovedHostMaterial = false

    init(
        relayURL: String = ProcessInfo.processInfo.environment["DESKLINK_RELAY_URL"] ?? "quic://127.0.0.1:4433",
        identityStore: ControllerIdentityStore = ControllerIdentityStore(),
        savedHostStore: SavedHostStore = SavedHostStore()
    ) {
        self.relayURL = relayURL
        self.identityStore = identityStore
        self.savedHostStore = savedHostStore
        decoder.onFrame = { [weak self] pixelBuffer in
            self?.latestPixelBuffer = pixelBuffer
        }
        do {
            _ = try loadIdentityIfNeeded()
        } catch {
            publishError(error)
        }
    }

    static func testing(
        error: String? = nil,
        state: ConnectionState = .idle
    ) -> ControllerBridge {
        let bridge = ControllerBridge()
        bridge.state = state
        if let error { bridge.publishErrorMessage(error) }
        return bridge
    }

    func connect(invite: Data) {
        guard invite.count == Int(DESKLINK_PAIRING_INVITE_BYTES) else {
            publishErrorMessage("The pairing invitation is invalid.")
            return
        }
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
            lastError = nil
            state = .connecting
        } catch {
            publishError(error)
        }
    }

    func connect(invite: [UInt8]) { connect(invite: Data(invite)) }

    func connect(savedHost: SavedHost) {
        guard savedHost.isValid else {
            publishErrorMessage("The saved host record is invalid.")
            return
        }
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
            lastError = nil
            state = .connecting
        } catch {
            publishError(error)
        }
    }

    func requestKeyframe() {
        guard let handle = handleOwner.pointer else { return }
        let result = desklink_request_keyframe(handle)
        if result != DESKLINK_OK { publishResultError("The video stream needs to recover.", result: result) }
    }

    func send(input command: MacInputCommand) {
        var input: DesklinkInput
        switch command {
        case let .move(x, y):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_MOVE, x: x, y: y, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0)
        case let .mouseButton(button, pressed):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_BUTTON, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: button.rawValue + 1, key_code: 0, character: 0, pressed: pressed ? 1 : 0, modifiers: 0)
        case let .wheel(x, y):
            input = DesklinkInput(kind: DESKLINK_INPUT_MOUSE_WHEEL, x: 0, y: 0, wheel_x: x, wheel_y: y, button: 0, key_code: 0, character: 0, pressed: 0, modifiers: 0)
        case let .key(code, pressed, modifiers):
            input = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: code, character: 0, pressed: pressed ? 1 : 0, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue))
        case let .unicode(text, modifiers):
            for scalar in text.unicodeScalars {
                var characterInput = DesklinkInput(kind: DESKLINK_INPUT_KEY, x: 0, y: 0, wheel_x: 0, wheel_y: 0, button: 0, key_code: 0, character: scalar.value, pressed: 1, modifiers: UInt8(truncatingIfNeeded: modifiers.rawValue))
                sendInput(&characterInput)
            }
            return
        }
        sendInput(&input)
    }

    func releaseAll() {
        guard let handle = handleOwner.pointer else { return }
        _ = desklink_release_all(handle)
    }

    func disconnect() {
        callbackGeneration &+= 1
        releaseAll()
        if let handle = handleOwner.pointer { _ = desklink_reject(handle) }
        handleOwner.destroy()
        decoder.reset()
        activeStreamID = 0
        highestStreamID = 0
        awaitingApprovedHostMaterial = false
        latestAccessUnit = nil
        latestPixelBuffer = nil
        cursorOverlay = nil
        state = .closed
    }

    // These shims keep the Task 1-4 views source-compatible while they are migrated.
    @available(*, deprecated, message: "Use connect(invite:) or connect(savedHost:).")
    func connectSecureFromEnvironment() {
        do {
            if ProcessInfo.processInfo.environment["DESKLINK_PAIRING_INVITE"] != nil {
                connect(invite: try PairingInviteConnectionSettings().invite)
                return
            }
            let settings = try SecureConnectionSettings()
            connect(savedHost: settings.savedHost)
        } catch {
            publishError(error)
        }
    }

    @available(*, deprecated, message: "Use connect(invite:).")
    func connect(code: String) {
        publishErrorMessage("Pairing codes are no longer supported. Use a signed invitation.")
    }

    @available(*, deprecated, message: "Use SessionInputView.")
    func sendMouseMove(x: Float, y: Float) { send(input: .move(normalizedX: x, normalizedY: y)) }
    @available(*, deprecated, message: "Use SessionInputView.")
    func sendMouseButton(_ button: UInt32, pressed: Bool) { send(input: .mouseButton(MouseButton(rawValue: button) ?? .center, pressed: pressed)) }
    @available(*, deprecated, message: "Use SessionInputView.")
    func sendMouseWheel(deltaX: Int32, deltaY: Int32) { send(input: .wheel(deltaX: deltaX, deltaY: deltaY)) }
    @available(*, deprecated, message: "Use connect(invite:).")
    func startPairing() { publishErrorMessage("This controller connects with a signed invitation.") }

    fileprivate func consume(
        eventKind: Int,
        data: Data,
        streamID: UInt64,
        frameID: UInt64,
        configVersion: UInt32,
        width: UInt16,
        height: UInt16,
        stateValue: Int
    ) {
        switch eventKind {
        case 1:
            consumeState(streamID: streamID, stateValue: stateValue)
        case 2:
            publishErrorMessage("The connection reported an error.")
        case 6:
            consumeVideoConfig(data: data, streamID: streamID, version: configVersion, width: width, height: height)
        case 7:
            consumeFrame(data: data, streamID: streamID, frameID: frameID, version: configVersion)
        case 8:
            guard streamID == activeStreamID, !data.isEmpty else { return }
            cursorOverlay = CursorOverlay(streamID: streamID, encodedUpdate: data)
        default:
            break
        }
    }

    private var relayServerName: String {
        ProcessInfo.processInfo.environment["DESKLINK_RELAY_SERVER_NAME"] ?? "localhost"
    }

    private func createIfNeeded() {
        guard handleOwner.pointer == nil else { return }
        callbackGeneration &+= 1
        let callbackContext = ControllerCallbackContext(bridge: self, generation: callbackGeneration)
        let callbackPointer = Unmanaged.passRetained(callbackContext).toOpaque()
        var createdHandle: OpaquePointer?
        let result = relayURL.withCString { relayPointer in
            var config = DesklinkConfig(relay_url: relayPointer, log_level: 1)
            return desklink_create(&config, desklinkEventCallback, callbackPointer, &createdHandle)
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
                decoder.reset()
                activeStreamID = streamID
                metrics.lastFrameID = nil
            }
            highestStreamID = max(highestStreamID, streamID)
            state = .connected(streamID: streamID)
            stageApprovedHostMaterialIfNeeded()
        case Int(DESKLINK_CLOSED.rawValue):
            guard streamID == 0 || activeStreamID == 0 || streamID == activeStreamID else { return }
            decoder.reset()
            activeStreamID = 0
            latestPixelBuffer = nil
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

    private func consumeVideoConfig(data: Data, streamID: UInt64, version: UInt32, width: UInt16, height: UInt16) {
        guard case let .connected(connectedStreamID) = state,
              connectedStreamID == streamID,
              streamID > 0,
              version > 0,
              width > 0,
              height > 0,
              !data.isEmpty
        else {
            dropFrame(); return
        }
        guard streamID == activeStreamID, version >= decoder.configVersion,
              decoder.configure(sequenceHeader: data, width: width, height: height, version: version)
        else { dropFrame(); return }
    }

    private func consumeFrame(data: Data, streamID: UInt64, frameID: UInt64, version: UInt32) {
        guard case let .connected(connectedStreamID) = state,
              connectedStreamID == streamID,
              streamID == activeStreamID,
              frameID > (metrics.lastFrameID ?? 0),
              version == decoder.configVersion,
              decoder.receive(accessUnit: data, frameID: frameID, version: version)
        else { dropFrame(); return }
        latestAccessUnit = data
        metrics.receivedFrames += 1
        metrics.lastFrameID = frameID
    }

    private func dropFrame() {
        metrics.droppedFrames += 1
        if decoder.takeKeyframeRequest() { requestKeyframe() }
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
            awaitingApprovedHostMaterial = false
        } catch {
            publishErrorMessage("The approved host could not be saved securely.")
        }
    }

    private func sendInput(_ input: inout DesklinkInput) {
        guard case .connected = state, let handle = handleOwner.pointer else { return }
        if desklink_send_input(handle, &input) != DESKLINK_OK {
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

    private func publishErrorMessage(_ message: String) {
        let safe = Self.redact(message)
        if handleOwner.pointer != nil {
            disconnect()
        }
        lastError = safe
        state = .failed(safe)
    }

    private static func redact(_ message: String) -> String {
        let lowercase = message.lowercased()
        if lowercase.contains("auth") || lowercase.contains("secret") || lowercase.contains("key") || lowercase.contains("token") {
            return "A secure connection error occurred."
        }
        return message
    }
}

typealias RustBridge = ControllerBridge

private func desklinkEventCallback(_ context: UnsafeMutableRawPointer?, _ event: UnsafePointer<DesklinkEvent>?) {
    guard let context, let event else { return }
    let callbackContext = Unmanaged<ControllerCallbackContext>.fromOpaque(context).takeUnretainedValue()
    guard let bridge = callbackContext.bridge else { return }
    let value = event.pointee
    let data = value.data.map { Data(bytes: $0, count: value.data_len) } ?? Data()
    let generation = callbackContext.generation
    let eventKind = Int(value.kind.rawValue)
    let streamID = value.stream_id
    let frameID = value.frame_id
    let configVersion = value.config_version
    let width = value.width
    let height = value.height
    let stateValue = Int(value.state.rawValue)
    DispatchQueue.main.async {
        guard bridge.acceptsCallback(generation: generation) else { return }
        bridge.consume(
            eventKind: eventKind, data: data, streamID: streamID, frameID: frameID,
            configVersion: configVersion, width: width, height: height, stateValue: stateValue
        )
    }
}

private extension ControllerBridge {
    func acceptsCallback(generation: UInt64) -> Bool {
        callbackGeneration == generation
    }
}
