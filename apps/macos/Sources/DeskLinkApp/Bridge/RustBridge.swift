import Combine
import CoreVideo
import DeskLinkC
import Foundation

private final class DeskLinkHandleOwner: @unchecked Sendable {
    var pointer: OpaquePointer?

    deinit {
        if let pointer {
            desklink_destroy(pointer)
        }
    }
}

@MainActor
final class RustBridge: ObservableObject {
    @Published private(set) var state: ConnectionState = .idle
    @Published private(set) var pairing: PairingInfo?
    @Published private(set) var metrics = Metrics()
    @Published private(set) var lastError: String?
    @Published private(set) var latestAccessUnit: Data?
    @Published private(set) var latestPixelBuffer: CVPixelBuffer?

    private let relayURL: String
    private let handleOwner = DeskLinkHandleOwner()
    private let decoder = H264Decoder()

    init(relayURL: String = "quic://127.0.0.1:4433") {
        self.relayURL = relayURL
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

    func requestKeyframe() {
        guard let handle = handleOwner.pointer else { return }
        let result = desklink_request_keyframe(handle)
        if result != DESKLINK_OK {
            publishError("Keyframe request failed (code \(result.rawValue)).")
        }
    }

    func releaseAll() {
        guard let handle = handleOwner.pointer else { return }
        _ = desklink_release_all(handle)
    }

    func disconnect() {
        guard let handle = handleOwner.pointer else { return }
        _ = desklink_reject(handle)
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
            case 7: state = .connected(streamID: streamID)
            case 12: state = .closed
            case 4: state = .pairing
            case 0: state = .idle
            default: state = .connecting
            }
        case 2:
            publishError(String(decoding: data, as: UTF8.self))
        case 7:
            guard frameID > (metrics.lastFrameID ?? 0) else { return }
            latestAccessUnit = data
            _ = decoder.receive(accessUnit: data, frameID: frameID, version: configVersion)
            latestPixelBuffer = decoder.latestPixelBuffer
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
    Task { @MainActor in
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
