import Combine
import CoreVideo
import DeskLinkC
import Foundation

private final class DeskLinkHandleOwner: @unchecked Sendable {
    var pointer: OpaquePointer?

    func destroy() {
        guard let pointer else { return }
        self.pointer = nil
        desklink_destroy(pointer)
    }

    deinit { destroy() }
}

@MainActor
final class RustBridge: ObservableObject {
    @Published private(set) var state: ConnectionState = .idle
    @Published private(set) var pairing: PairingInfo?
    @Published private(set) var metrics = Metrics()
    @Published private(set) var lastError: String?
    @Published private(set) var latestAccessUnit: Data?
    @Published private(set) var latestPixelBuffer: CVPixelBuffer?
    @Published private(set) var controllerVerifyKeyHex: String?

    private let relayURL: String
    private let handleOwner = DeskLinkHandleOwner()
    private let decoder = H264Decoder()
    private let identityStore = ControllerIdentityStore()
    private var controllerIdentity: ControllerIdentity?
    private var activeStreamID: UInt64 = 0

    init(
        relayURL: String = ProcessInfo.processInfo.environment["DESKLINK_RELAY_URL"]
            ?? "quic://127.0.0.1:4433"
    ) {
        self.relayURL = relayURL
        decoder.onFrame = { [weak self] pixelBuffer in
            self?.latestPixelBuffer = pixelBuffer
        }
        do {
            _ = try loadIdentityIfNeeded()
        } catch {
            lastError = error.localizedDescription
        }
    }

    func createIfNeeded() {
        guard handleOwner.pointer == nil else { return }
        var createdHandle: OpaquePointer?
        let result = relayURL.withCString { relayPointer in
            var config = DesklinkConfig(relay_url: relayPointer, log_level: 1)
            return desklink_create(
                &config,
                desklinkEventCallback,
                Unmanaged.passUnretained(self).toOpaque(),
                &createdHandle
            )
        }
        guard result == DESKLINK_OK, let createdHandle else {
            lastError = "Unable to create the DeskLink runtime (code \(result.rawValue))."
            state = .failed(lastError ?? "Unable to create runtime")
            return
        }
        handleOwner.pointer = createdHandle
    }

    func startPairing() {
        createIfNeeded()
        guard let handle = handleOwner.pointer else { return }
        var info = DesklinkPairingInfo()
        let result = desklink_start_pairing(handle, &info)
        guard result == DESKLINK_OK else {
            publishError("Pairing could not start (code \(result.rawValue)).")
            return
        }
        pairing = PairingInfo(
            sessionID: UUID(bytes: info.sessionIDBytes),
            code: info.codeString,
            expiresAt: Date(timeIntervalSince1970: TimeInterval(info.expires_at_unix_s))
        )
        state = .pairing
    }

    func connect(code: String) {
        createIfNeeded()
        guard let handle = handleOwner.pointer else { return }
        let result = code.withCString { desklink_connect_with_code(handle, $0) }
        guard result == DESKLINK_OK else {
            publishError("Connection failed (code \(result.rawValue)).")
            return
        }
        state = .connecting
    }

    func connectSecureFromEnvironment() {
        createIfNeeded()
        guard let handle = handleOwner.pointer else { return }
        do {
            if ProcessInfo.processInfo.environment["DESKLINK_PAIRING_INVITE"] != nil {
                try connectPairingInviteFromEnvironment(handle: handle)
                return
            }
            let settings = try SecureConnectionSettings()
            let identity = try loadIdentityIfNeeded()
            var config = DesklinkSecureConnectionConfig()
            withUnsafeMutableBytes(of: &config.session_id) {
                $0.copyBytes(from: settings.sessionID)
            }
            withUnsafeMutableBytes(of: &config.relay_authentication) {
                $0.copyBytes(from: settings.relayAuthentication)
            }
            withUnsafeMutableBytes(of: &config.controller_device_id) {
                $0.copyBytes(from: identity.deviceID)
            }
            withUnsafeMutableBytes(of: &config.controller_secret_key) {
                $0.copyBytes(from: identity.secretKey)
            }
            withUnsafeMutableBytes(of: &config.host_verify_key) {
                $0.copyBytes(from: settings.hostVerifyKey)
            }
            let result = settings.serverName.withCString { serverName in
                config.server_name = serverName
                return desklink_connect_secure(handle, &config)
            }
            guard result == DESKLINK_OK else {
                publishError("Secure connection failed (code \(result.rawValue)).")
                return
            }
            lastError = nil
            state = .connecting
        } catch {
            publishError(error.localizedDescription)
        }
    }

    private func connectPairingInviteFromEnvironment(handle: OpaquePointer) throws {
        let settings = try PairingInviteConnectionSettings()
        let identity = try loadIdentityIfNeeded()
        var config = DesklinkPairingInviteConnectionConfig()
        withUnsafeMutableBytes(of: &config.controller_device_id) {
            $0.copyBytes(from: identity.deviceID)
        }
        withUnsafeMutableBytes(of: &config.controller_secret_key) {
            $0.copyBytes(from: identity.secretKey)
        }
        let result = settings.invite.withUnsafeBufferPointer { invite in
            settings.serverName.withCString { serverName in
                config.server_name = serverName
                config.invite = invite.baseAddress
                config.invite_len = invite.count
                return desklink_connect_pairing_invite(handle, &config)
            }
        }
        guard result == DESKLINK_OK else {
            throw SecureConnectionSettingsError.rejectedPairingInvite(Int32(result.rawValue))
        }
        lastError = nil
        state = .connecting
    }

    func requestKeyframe() {
        guard let handle = handleOwner.pointer else { return }
        let result = desklink_request_keyframe(handle)
        if result != DESKLINK_OK {
            publishError("Keyframe request failed (code \(result.rawValue)).")
        }
    }

    func sendMouseMove(x: Float, y: Float) {
        var input = DesklinkInput(
            kind: DESKLINK_INPUT_MOUSE_MOVE,
            x: x,
            y: y,
            wheel_x: 0,
            wheel_y: 0,
            button: 0,
            key_code: 0,
            character: 0,
            pressed: 0,
            modifiers: 0
        )
        sendInput(&input)
    }

    func sendMouseButton(_ button: UInt32, pressed: Bool) {
        var input = DesklinkInput(
            kind: DESKLINK_INPUT_MOUSE_BUTTON,
            x: 0,
            y: 0,
            wheel_x: 0,
            wheel_y: 0,
            button: button,
            key_code: 0,
            character: 0,
            pressed: pressed ? 1 : 0,
            modifiers: 0
        )
        sendInput(&input)
    }

    func sendMouseWheel(deltaX: Int32, deltaY: Int32) {
        var input = DesklinkInput(
            kind: DESKLINK_INPUT_MOUSE_WHEEL,
            x: 0,
            y: 0,
            wheel_x: deltaX,
            wheel_y: deltaY,
            button: 0,
            key_code: 0,
            character: 0,
            pressed: 0,
            modifiers: 0
        )
        sendInput(&input)
    }

    func releaseAll() {
        guard let handle = handleOwner.pointer else { return }
        _ = desklink_release_all(handle)
    }

    func disconnect() {
        guard let handle = handleOwner.pointer else { return }
        _ = desklink_reject(handle)
        handleOwner.destroy()
        decoder.reset()
        activeStreamID = 0
        latestAccessUnit = nil
        latestPixelBuffer = nil
        state = .closed
    }

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
            switch stateValue {
            case 7:
                if activeStreamID != streamID {
                    decoder.reset()
                    activeStreamID = streamID
                    metrics.lastFrameID = nil
                }
                state = .connected(streamID: streamID)
            case 12:
                decoder.reset()
                activeStreamID = 0
                state = .closed
            case 4: state = .pairing
            case 0: state = .idle
            default: state = .connecting
            }
        case 2:
            publishError(String(decoding: data, as: UTF8.self))
        case 6:
            guard streamID > 0,
                  configVersion > 0,
                  width > 0,
                  height > 0,
                  !data.isEmpty
            else {
                metrics.droppedFrames += 1
                lastError = "Received an invalid H.264 video configuration."
                return
            }
            if activeStreamID == 0 {
                activeStreamID = streamID
            }
            guard streamID == activeStreamID,
                  decoder.configure(
                      sequenceHeader: data,
                      width: width,
                      height: height,
                      version: configVersion
                  )
            else {
                metrics.droppedFrames += 1
                lastError = "VideoToolbox rejected H.264 configuration (configVersion)."
                requestKeyframeIfNeeded()
                return
            }
        case 7:
            guard streamID == activeStreamID,
                  frameID > (metrics.lastFrameID ?? 0)
            else {
                metrics.droppedFrames += 1
                return
            }
            guard decoder.receive(
                accessUnit: data,
                frameID: frameID,
                version: configVersion
            ) else {
                metrics.droppedFrames += 1
                requestKeyframeIfNeeded()
                return
            }
            latestAccessUnit = data
            metrics.receivedFrames += 1
            metrics.lastFrameID = frameID
        default:
            _ = (streamID, configVersion, width, height)
        }
    }

    private func publishError(_ message: String) {
        lastError = message
        state = .failed(message)
    }

    private func requestKeyframeIfNeeded() {
        if decoder.takeKeyframeRequest() {
            requestKeyframe()
        }
    }

    private func sendInput(_ input: inout DesklinkInput) {
        guard case .connected = state,
              let handle = handleOwner.pointer
        else { return }
        let result = desklink_send_input(handle, &input)
        if result != DESKLINK_OK {
            lastError = "Input delivery failed (code \(result.rawValue))."
        }
    }

    private func loadIdentityIfNeeded() throws -> ControllerIdentity {
        if let controllerIdentity { return controllerIdentity }
        let identity = try identityStore.loadOrCreate()
        controllerIdentity = identity
        controllerVerifyKeyHex = identity.verifyKey.hexString
        return identity
    }
}

private func desklinkEventCallback(
    _ context: UnsafeMutableRawPointer?,
    _ event: UnsafePointer<DesklinkEvent>?
) {
    guard let context, let event else { return }
    let eventValue = event.pointee
    let data = eventValue.data.map {
        Data(bytes: $0, count: eventValue.data_len)
    } ?? Data()
    let bridge = Unmanaged<RustBridge>.fromOpaque(context).takeUnretainedValue()
    let eventKind = Int(eventValue.kind.rawValue)
    let streamID = eventValue.stream_id
    let frameID = eventValue.frame_id
    let configVersion = eventValue.config_version
    let width = eventValue.width
    let height = eventValue.height
    let stateValue = Int(eventValue.state.rawValue)
    // The serial main queue preserves the worker's VideoConfig-before-frame order.
    DispatchQueue.main.async {
        bridge.consume(
            eventKind: eventKind,
            data: data,
            streamID: streamID,
            frameID: frameID,
            configVersion: configVersion,
            width: width,
            height: height,
            stateValue: stateValue
        )
    }
}

private extension DesklinkPairingInfo {
    var codeString: String {
        withUnsafeBytes(of: code) { bytes in
            String(decoding: bytes.prefix { $0 != 0 }, as: UTF8.self)
        }
    }

    var sessionIDBytes: [UInt8] {
        withUnsafeBytes(of: session_id) { Array($0) }
    }
}

private extension UUID {
    init(bytes: [UInt8]) {
        self = bytes.withUnsafeBytes { rawBuffer in
            UUID(uuid: rawBuffer.load(as: uuid_t.self))
        }
    }
}

private extension Array where Element == UInt8 {
    var hexString: String {
        map { String(format: "%02x", $0) }.joined()
    }
}
