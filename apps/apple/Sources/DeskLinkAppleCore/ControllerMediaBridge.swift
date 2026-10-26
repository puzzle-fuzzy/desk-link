import Combine
import Foundation

public struct RemoteAudioFrame: Sendable {
    public let streamID: UInt64
    public let sequence: UInt64
    public let pcm: Data
}

public enum ControllerMediaAction: Equatable, Sendable {
    case setAudio(streamID: UInt64, enabled: Bool)
    case sendFile(streamID: UInt64, transferID: [UInt8], path: String)
    case requestFile(streamID: UInt64, requestID: UInt64, directory: String)
    case cancelFile(streamID: UInt64)
}

public struct RemoteFileTransfer: Decodable, Equatable, Sendable {
    public enum State: String, Decodable, Sendable {
        case preparing, waiting, sending, receiving, verifying, completed, failed, rejected, cancelled
        public var isTerminal: Bool { [.completed, .failed, .rejected, .cancelled].contains(self) }
        public var title: String {
            switch self {
            case .preparing: "准备文件"; case .waiting: "等待远端确认"
            case .sending: "正在发送"; case .receiving: "正在接收"; case .verifying: "正在校验"
            case .completed: "已完成"; case .failed: "传输失败"; case .rejected: "远端已拒绝"; case .cancelled: "已取消"
            }
        }
    }
    public enum Direction: String, Decodable, Sendable { case upload, download }
    public let transferID: String
    public let requestID: UInt64
    public let state: State
    public let direction: Direction
    public let name: String
    public let transferred: UInt64
    public let total: UInt64
    public let result: String
    public let savedPath: String?
    public var progress: Double { total == 0 ? (state == .completed ? 1 : 0) : min(1, Double(transferred) / Double(total)) }

    enum CodingKeys: String, CodingKey {
        case state, direction, name, transferred, total, result
        case transferID = "transfer_id", requestID = "request_id", savedPath = "saved_path"
    }

    public var resultMessage: String? {
        guard state.isTerminal, state != .completed else { return nil }
        switch result {
        case "cancelled": return "传输已取消。"
        case "rejected": return "远端未接受本次传输。"
        case "too_large": return "文件超过 256 MB 限制。"
        case "invalid_data": return "文件内容或校验结果无效，请重新发送。"
        case "permission_denied": return "无法访问所选文件或保存位置。"
        case "insufficient_space": return "保存位置的可用空间不足。"
        case "source_changed": return "源文件在传输期间发生了变化，请重新选择。"
        case "unsupported": return "远端版本暂不支持此操作。"
        case "busy", "queue_overflow": return "远端或传输通道正忙，请稍后重试。"
        case "timeout": return "等待远端超时，请检查后重试。"
        case "transport_error", "stale_session": return "连接已变化，请重新操作。"
        default: return "文件未能传输完成，请重试。"
        }
    }
}

/// Metadata and request correlation only. Native dialogs/playback stay in the
/// Mac app; file bytes and disk IO stay in the bounded Rust transfer actor.
@MainActor
public final class ControllerMediaBridge: ObservableObject {
    @Published public private(set) var streamID: UInt64 = 0
    @Published public private(set) var contextID: UInt64 = 0
    @Published public private(set) var audioAvailable: Bool?
    @Published public private(set) var audioEnabled = false
    @Published public private(set) var audioPending = false
    @Published public private(set) var audioError: String?
    @Published public private(set) var fileTransfer: RemoteFileTransfer?
    @Published public private(set) var fileError: String?
    @Published public private(set) var fileCancelPending = false
    public var isConnected: Bool { streamID != 0 }
    public var isFileBusy: Bool { expectedFile != nil }
    public var performAction: ((ControllerMediaAction) -> Bool)?
    public var onAudioFrame: ((RemoteAudioFrame) -> Void)?
    public var onAudioStateChanged: ((UInt64, Bool) -> Void)?

    private var audioTarget: Bool?
    private var userWantsAudio = false
    private var audioTimeout: Task<Void, Never>?
    private var lastAudioSequence: UInt64?
    private var nextRequestID: UInt64 = 0
    private var expectedFile: (direction: RemoteFileTransfer.Direction, id: String, requestID: UInt64)?

    func attach(streamID: UInt64) {
        guard self.streamID != streamID else { return }
        reset()
        self.streamID = streamID
    }

    func reset() {
        audioTimeout?.cancel(); audioTimeout = nil
        audioTarget = nil; userWantsAudio = false; audioPending = false; audioEnabled = false; audioAvailable = nil
        audioError = nil; lastAudioSequence = nil
        expectedFile = nil; fileTransfer = nil; fileError = nil; fileCancelPending = false
        streamID = 0
        contextID &+= 1
        onAudioStateChanged?(0, false)
    }

    @discardableResult public func setAudioEnabled(_ enabled: Bool) -> Bool {
        guard isConnected, (!audioPending || !enabled), !enabled || audioAvailable != false else { return false }
        // Stop local sound immediately on off; only an authenticated matching
        // host acknowledgement may start it again.
        if !enabled { userWantsAudio = false; audioEnabled = false; onAudioStateChanged?(streamID, false) }
        audioTimeout?.cancel(); audioTimeout = nil
        audioTarget = nil; audioPending = false
        guard performAction?(.setAudio(streamID: streamID, enabled: enabled)) == true else {
            audioError = "声音请求未能发送，请重试。"; return false
        }
        userWantsAudio = enabled
        audioError = nil; audioTarget = enabled; audioPending = true
        if !enabled { lastAudioSequence = nil }
        let context = contextID
        audioTimeout?.cancel()
        audioTimeout = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(15))
            guard !Task.isCancelled, let self, self.contextID == context, self.audioPending else { return }
            self.failAudio("远端声音设置未在 15 秒内确认，请重试。")
        }
        return true
    }

    func consumeSession(_ data: Data, streamID: UInt64) {
        guard isConnected, streamID == self.streamID, data.count <= 16_384,
              let event = try? JSONDecoder().decode(AudioStateEvent.self, from: data) else { return }
        if event.kind == "audio_error" { failAudio("远端声音暂时不可用，请重新开启。") ; return }
        guard event.kind == "audio_state", let available = event.available, let enabled = event.enabled else { return }
        let previousAvailability = audioAvailable
        audioAvailable = available
        if available && previousAvailability == false { audioError = nil }
        if !available {
            failAudio("远端暂未提供系统声音。")
            return
        }
        // Ignore an opposite/old state while a new request is pending. Off is
        // always safe to apply. Never start from an unsolicited enabled event.
        if let target = audioTarget, target == enabled {
            audioTimeout?.cancel(); audioTimeout = nil
            audioPending = false; audioTarget = nil; audioEnabled = enabled; audioError = nil
            lastAudioSequence = nil
            onAudioStateChanged?(streamID, enabled)
        } else if enabled, userWantsAudio, audioTarget != false {
            // An initial mute followed quickly by enable may coalesce at the
            // host. A later real true state may restore the user's existing
            // consent, even after an older false acknowledgement stopped it.
            audioEnabled = true; audioError = nil
            onAudioStateChanged?(streamID, true)
        } else if !enabled {
            audioEnabled = false; lastAudioSequence = nil
            onAudioStateChanged?(streamID, false)
        }
    }

    func consumePCM(_ pcm: Data, streamID: UInt64, sequence: UInt64, sampleRate: UInt16, channels: UInt16) {
        guard audioEnabled, streamID == self.streamID, sampleRate == 48_000, channels == 1,
              pcm.count == 960, lastAudioSequence.map({ sequence > $0 }) ?? true else { return }
        lastAudioSequence = sequence
        onAudioFrame?(RemoteAudioFrame(streamID: streamID, sequence: sequence, pcm: pcm))
    }

    public func reportPlaybackFailure() {
        failAudio("本机声音播放失败，请检查输出设备后重新开启。")
        if isConnected { _ = performAction?(.setAudio(streamID: streamID, enabled: false)) }
    }

    private func failAudio(_ message: String) {
        audioTimeout?.cancel(); audioTimeout = nil
        audioTarget = nil; userWantsAudio = false; audioPending = false; audioEnabled = false; audioError = message
        lastAudioSequence = nil; onAudioStateChanged?(streamID, false)
    }

    @discardableResult public func sendFile(_ url: URL) -> Bool {
        guard isConnected, !isFileBusy, validPath(url) else { return false }
        let id = UUID().uuid
        let bytes = withUnsafeBytes(of: id) { Array($0) }
        let hex = bytes.map { String(format: "%02x", $0) }.joined()
        guard performAction?(.sendFile(streamID: streamID, transferID: bytes, path: url.path)) == true else {
            fileError = "文件请求未能发送，请重试。"; return false
        }
        expectedFile = (.upload, hex, 0)
        fileError = nil; fileCancelPending = false
        fileTransfer = RemoteFileTransfer(transferID: hex, requestID: 0, state: .preparing, direction: .upload,
                                         name: url.lastPathComponent, transferred: 0, total: 0, result: "", savedPath: nil)
        return true
    }

    @discardableResult public func requestRemoteFile(to directory: URL) -> Bool {
        guard isConnected, !isFileBusy, validPath(directory), nextRequestID < .max else { return false }
        nextRequestID += 1
        guard performAction?(.requestFile(streamID: streamID, requestID: nextRequestID, directory: directory.path)) == true else {
            fileError = "获取远端文件的请求未能发送，请重试。"; return false
        }
        expectedFile = (.download, "", nextRequestID)
        fileError = nil; fileCancelPending = false
        fileTransfer = RemoteFileTransfer(transferID: "", requestID: nextRequestID, state: .waiting, direction: .download,
                                         name: "等待远端选择文件", transferred: 0, total: 0, result: "", savedPath: nil)
        return true
    }

    @discardableResult public func cancelFileTransfer() -> Bool {
        guard isConnected, isFileBusy, !fileCancelPending else { return false }
        guard performAction?(.cancelFile(streamID: streamID)) == true else {
            fileError = "取消请求未能发送，请重试。"; return false
        }
        fileCancelPending = true
        return true
    }

    func consumeFile(_ data: Data, streamID: UInt64) {
        guard isConnected, streamID == self.streamID, data.count <= 32_768, var expected = expectedFile,
              let event = try? JSONDecoder().decode(RemoteFileTransfer.self, from: data),
              event.direction == expected.direction, event.requestID == expected.requestID,
              event.name.utf8.count <= 1024, event.total <= 256 * 1024 * 1024, event.transferred <= event.total,
              event.savedPath == nil || (event.state == .completed && event.direction == .download
                                        && event.savedPath!.utf8.count <= 4096 && event.savedPath!.hasPrefix("/")) else { return }
        if expected.id.isEmpty {
            if !event.transferID.isEmpty {
                guard validTransferID(event.transferID) else { return }
                expected.id = event.transferID
                expectedFile = expected
            }
        } else { guard event.transferID == expected.id else { return } }
        // Expected request/transfer is cleared before publishing a terminal
        // event, so duplicate replies cannot complete a newly queued task.
        if event.state.isTerminal { expectedFile = nil; fileCancelPending = false }
        fileError = event.resultMessage
        fileTransfer = event
    }

    private func validPath(_ url: URL) -> Bool {
        url.isFileURL && url.path.hasPrefix("/") && !url.path.utf8.contains(0) && url.path.utf8.count <= 4096
    }
    private func validTransferID(_ value: String) -> Bool {
        value.count == 32 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    private struct AudioStateEvent: Decodable { let kind: String; let available: Bool?; let enabled: Bool? }
}
