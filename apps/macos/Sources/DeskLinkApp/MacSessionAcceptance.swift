#if DEBUG
import AppKit
import CoreVideo
import DeskLinkAppleCore

/// A separate debug bundle can exercise the real session views without a
/// network peer, screen capture, input injection or the user's Keychain.
@MainActor
enum MacSessionAcceptance {
    static var isEnabled: Bool {
        ProcessInfo.processInfo.environment["DESKLINK_SESSION_ACCEPTANCE"] == "1"
    }

    /// Every fixture run owns a fresh directory. The upload fixture is public
    /// sample text; selected user files are never read by the simulated peer.
    static let fileRootURL = FileManager.default.temporaryDirectory.appendingPathComponent(
        "DeskLink-Phase4-Acceptance-\(UUID().uuidString)", isDirectory: true)
    static var outgoingFixtureURL: URL { fileRootURL.appendingPathComponent("DeskLink-送测文件.txt") }
    private static let fileFixtureData = Data("DeskLink macOS phase 4 acceptance fixture.\n中文与 emoji ✅\n".utf8)

    static func controller() -> ControllerBridge {
        let bridge = ControllerBridge.testing(state: .connected(streamID: 42))
        if let frame = testFrame() {
            var cursor = Data()
            for value: UInt64 in [12, 42, 1, 0, 900_000, 700_000, 1, 1] {
                var remaining = value
                repeat {
                    let byte = UInt8(remaining & 0x7f)
                    remaining >>= 7
                    cursor.append(remaining == 0 ? byte : byte | 0x80)
                } while remaining != 0
            }
            bridge.presentFrameForTesting(frame, streamID: 42,
                                          cursor: CursorOverlay(streamID: 42, encodedUpdate: cursor))
        }
        displayList(bridge, selected: 0)
        event(bridge, kind: 13, value: ["kind": "video_quality_state", "quality_preference": "automatic", "applied_preset": "balanced"])
        let mediaPeer = AcceptanceMediaPeer(bridge: bridge)
        bridge.media.performAction = { [mediaPeer] action in mediaPeer.perform(action) }
        event(bridge, kind: 13, value: ["kind": "audio_state", "available": true, "enabled": false])
        bridge.featureRequestForTesting = { [weak bridge] request in
            Task { @MainActor [weak bridge] in
                try? await Task.sleep(for: .milliseconds(350))
                guard let bridge else { return }
                switch request {
                case let .selectDisplay(_, displayID):
                    displayList(bridge, selected: displayID)
                    if let frame = testFrame(label: "DeskLink · 屏幕 \(displayID)") {
                        bridge.presentFrameForTesting(frame, streamID: 42)
                    }
                case let .chooseQuality(_, preference):
                    event(bridge, kind: 13, value: ["kind": "video_quality_state", "quality_preference": preference.rawValue,
                                                   "applied_preset": preference == .automatic ? "balanced" : preference.rawValue])
                case let .sendClipboard(_, requestID, _):
                    event(bridge, kind: 14, value: ["request_id": requestID, "operation": "send", "status": "completed", "result": "completed"])
                case let .requestClipboard(_, requestID):
                    event(bridge, kind: 14, value: ["request_id": requestID, "operation": "request", "status": "completed", "result": "completed", "text": "远端中文剪贴板\nDeskLink 🖥️"])
                case .sendText: break
                }
            }
            return true
        }
        return bridge
    }

    static func clipboard() -> MacClipboardController {
        MacClipboardController(clipboard: AcceptanceClipboard())
    }

    private static func displayList(_ bridge: ControllerBridge, selected: UInt32) {
        event(bridge, kind: 13, value: ["kind": "display_list", "selected_display_id": selected,
            "displays": [["id": 0, "width": 2560, "height": 1440, "is_primary": true],
                         ["id": 1, "width": 1920, "height": 1080, "is_primary": false]]])
    }

    private static func event(_ bridge: ControllerBridge, kind: Int, value: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: value),
              let text = String(data: data, encoding: .utf8) else { return }
        bridge.acceptEventForTesting(eventKind: kind, stateValue: 0, streamID: 42, message: text)
    }

    private final class AcceptanceClipboard: TextClipboard {
        var text = "本机中文剪贴板\nDeskLink ✅"
        func readText() throws -> String { text }
        func writeText(_ text: String) throws { self.text = text }
    }

    /// Exercises the real native picker, UI state and audio output. Rust and
    /// Windows transport acceptance remain separate from this explicit fixture.
    @MainActor
    private final class AcceptanceMediaPeer {
        private weak var bridge: ControllerBridge?
        private var audioToken: UInt64 = 0
        private var fileToken: UInt64 = 0
        private var audioTask: Task<Void, Never>?
        private var fileTask: Task<Void, Never>?
        private struct Transfer {
            let id: String
            let requestID: UInt64
            let direction: String
            let name: String
            let total: UInt64
            let destination: URL?
        }
        private var activeTransfer: Transfer?

        init(bridge: ControllerBridge) {
            self.bridge = bridge
            do {
                try FileManager.default.createDirectory(at: fileRootURL, withIntermediateDirectories: false,
                                                        attributes: [.posixPermissions: 0o700])
                try fileFixtureData.write(to: outgoingFixtureURL, options: .withoutOverwriting)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: outgoingFixtureURL.path)
                FileHandle.standardOutput.write(Data("DeskLinkAcceptance file_root=\(fileRootURL.path)\n".utf8))
            } catch {
                print("DeskLinkAcceptance fixture_file_preparation_failed")
            }
        }

        func perform(_ action: ControllerMediaAction) -> Bool {
            guard let bridge, bridge.media.streamID == 42 else { return false }
            switch action {
            case let .setAudio(streamID, enabled):
                guard streamID == 42 else { return false }
                audioToken &+= 1
                let token = audioToken
                let context = bridge.media.contextID
                audioTask?.cancel()
                audioTask = Task { @MainActor [weak self, weak bridge] in
                    try? await Task.sleep(for: .milliseconds(100))
                    guard let self, let bridge, !Task.isCancelled, self.audioToken == token,
                          bridge.media.contextID == context else { return }
                    event(bridge, kind: 13, value: ["kind": "audio_state", "available": true, "enabled": enabled])
                    guard enabled else { return }
                    for sequence in 0..<RemoteAudioAcceptance.frameCount {
                        guard !Task.isCancelled, self.audioToken == token, bridge.media.contextID == context,
                              bridge.media.audioEnabled else { return }
                        bridge.acceptPCMForTesting(RemoteAudioAcceptance.sineFrame(sequence: UInt64(sequence)),
                                                   streamID: 42, sequence: UInt64(sequence))
                        try? await Task.sleep(for: .milliseconds(10))
                    }
                }
                return true
            case let .sendFile(streamID, transferID, path):
                guard streamID == 42, activeTransfer == nil else { return false }
                let transfer = Transfer(id: transferID.map { String(format: "%02x", $0) }.joined(), requestID: 0,
                                        direction: "upload", name: URL(fileURLWithPath: path).lastPathComponent,
                                        total: UInt64(fileFixtureData.count), destination: nil)
                let isFixture = URL(fileURLWithPath: path).standardizedFileURL == outgoingFixtureURL.standardizedFileURL
                begin(transfer, context: bridge.media.contextID, allowed: isFixture)
                return true
            case let .requestFile(streamID, requestID, directory):
                guard streamID == 42, activeTransfer == nil else { return false }
                let name = "DeskLink-接收验证-\(UUID().uuidString).txt"
                let transfer = Transfer(id: UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased(),
                                        requestID: requestID, direction: "download", name: name,
                                        total: UInt64(fileFixtureData.count),
                                        destination: URL(fileURLWithPath: directory, isDirectory: true).appendingPathComponent(name))
                begin(transfer, context: bridge.media.contextID, allowed: true)
                return true
            case let .cancelFile(streamID):
                guard streamID == 42, let transfer = activeTransfer else { return false }
                fileToken &+= 1
                fileTask?.cancel(); fileTask = nil; activeTransfer = nil
                // Match real FFI delivery: the caller records its request
                // before this terminal result arrives on a later main turn.
                Task { @MainActor [weak self, weak bridge] in
                    guard let self, let bridge, bridge.media.streamID == 42 else { return }
                    self.emit(transfer, bridge: bridge, state: "cancelled", transferred: 0, result: "cancelled")
                }
                return true
            }
        }

        private func begin(_ transfer: Transfer, context: UInt64, allowed: Bool) {
            fileToken &+= 1
            let token = fileToken
            activeTransfer = transfer
            fileTask?.cancel()
            fileTask = Task { @MainActor [weak self, weak bridge] in
                guard let self, let bridge, !Task.isCancelled, self.fileToken == token,
                      bridge.media.contextID == context else { return }
                guard allowed else {
                    self.activeTransfer = nil
                    self.emit(transfer, bridge: bridge, state: "failed", transferred: 0, result: "permission_denied")
                    return
                }
                for step in 0...3 {
                    if step > 0 { try? await Task.sleep(for: .milliseconds(500)) }
                    guard !Task.isCancelled, self.fileToken == token, bridge.media.contextID == context else { return }
                    self.emit(transfer, bridge: bridge, state: transfer.direction == "upload" ? "sending" : "receiving",
                              transferred: transfer.total * UInt64(step) / 3)
                }
                self.emit(transfer, bridge: bridge, state: "verifying", transferred: transfer.total)
                try? await Task.sleep(for: .milliseconds(350))
                guard !Task.isCancelled, self.fileToken == token, bridge.media.contextID == context else { return }
                do {
                    if let destination = transfer.destination {
                        // Public fixture bytes only, with no replacement of
                        // an existing file in the explicitly selected folder.
                        try fileFixtureData.write(to: destination, options: .withoutOverwriting)
                    }
                    self.activeTransfer = nil
                    self.emit(transfer, bridge: bridge, state: "completed", transferred: transfer.total,
                              savedPath: transfer.destination?.path)
                } catch {
                    self.activeTransfer = nil
                    self.emit(transfer, bridge: bridge, state: "failed", transferred: transfer.total, result: "permission_denied")
                }
            }
        }

        private func emit(_ transfer: Transfer, bridge: ControllerBridge, state: String, transferred: UInt64,
                          result: String = "", savedPath: String? = nil) {
            var value: [String: Any] = ["transfer_id": transfer.id, "request_id": transfer.requestID,
                                      "direction": transfer.direction, "name": transfer.name, "state": state,
                                      "transferred": transferred, "total": transfer.total, "result": result]
            if let savedPath { value["saved_path"] = savedPath }
            event(bridge, kind: 16, value: value)
        }
    }

    private static func testFrame(label: String = "DeskLink · 2560 × 1440") -> CVPixelBuffer? {
        let width = 2560
        let height = 1440
        var buffer: CVPixelBuffer?
        let attributes = [kCVPixelBufferMetalCompatibilityKey: true] as CFDictionary
        guard CVPixelBufferCreate(kCFAllocatorDefault, width, height,
                                  kCVPixelFormatType_32BGRA, attributes, &buffer) == kCVReturnSuccess,
              let buffer else { return nil }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let bytes = CVPixelBufferGetBaseAddress(buffer),
              let context = CGContext(data: bytes, width: width, height: height,
                                      bitsPerComponent: 8, bytesPerRow: CVPixelBufferGetBytesPerRow(buffer),
                                      space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGBitmapInfo.byteOrder32Little.rawValue
                                        | CGImageAlphaInfo.premultipliedFirst.rawValue)
        else { return nil }
        context.setFillColor(CGColor(red: 0.08, green: 0.12, blue: 0.18, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        for column in 0..<16 {
            for row in 0..<9 {
                context.setStrokeColor(CGColor(gray: 0.45, alpha: 1))
                context.stroke(CGRect(x: column * 160, y: row * 160, width: 160, height: 160))
            }
        }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
        let labels = [
            (label, CGPoint(x: 110, y: 700)),
            ("TOP LEFT", CGPoint(x: 32, y: height - 80)),
            ("BOTTOM RIGHT", CGPoint(x: width - 490, y: 32)),
        ]
        for (text, point) in labels {
            (text as NSString).draw(at: point, withAttributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 48, weight: .medium),
                .foregroundColor: NSColor.white,
            ])
        }
        NSGraphicsContext.restoreGraphicsState()
        return buffer
    }
}
#endif
