import AppKit
import DeskLinkAppleCore

@MainActor
protocol TextClipboard {
    func readText() throws -> String
    func writeText(_ text: String) throws
}

enum TextClipboardError: LocalizedError {
    case noText
    case tooLarge
    case writeFailed

    var errorDescription: String? {
        switch self {
        case .noText: "本机剪贴板中没有纯文本。"
        case .tooLarge: "剪贴板文本不能超过 48 KB，请缩短后重试。"
        case .writeFailed: "无法写入本机剪贴板，请重试。"
        }
    }
}

@MainActor
struct SystemTextClipboard: TextClipboard {
    func readText() throws -> String {
        guard let text = NSPasteboard.general.string(forType: .string) else {
            throw TextClipboardError.noText
        }
        guard text.utf8.count <= 48 * 1024 else { throw TextClipboardError.tooLarge }
        return text
    }

    func writeText(_ text: String) throws {
        guard text.utf8.count <= 48 * 1024 else { throw TextClipboardError.tooLarge }
        NSPasteboard.general.clearContents()
        guard NSPasteboard.general.setString(text, forType: .string) else {
            throw TextClipboardError.writeFailed
        }
    }
}

@MainActor
protocol RemoteClipboardSession: AnyObject {
    var canSendRemoteInput: Bool { get }
    var isClipboardBusy: Bool { get }
    var onRemoteClipboardText: ((String) -> Void)? { get set }
    @discardableResult func sendClipboardText(_ text: String, paste: Bool) -> Bool
    @discardableResult func requestRemoteClipboard() -> Bool
}

extension ControllerBridge: RemoteClipboardSession {}

/// The native clipboard is accessed only as a result of a user action and a
/// matching reply. No clipboard polling or automatic synchronization is used.
@MainActor
final class MacClipboardController: ObservableObject {
    @Published private(set) var localError: String?
    @Published private(set) var localStatus: String?
    private let clipboard: any TextClipboard
    private weak var session: (any RemoteClipboardSession)?
    private var bindingGeneration: UInt64 = 0

    init(clipboard: any TextClipboard = SystemTextClipboard()) {
        self.clipboard = clipboard
    }

    func attach(to session: any RemoteClipboardSession) {
        guard self.session !== session else { return }
        detach()
        self.session = session
        let generation = bindingGeneration
        session.onRemoteClipboardText = { [weak self] text in
            guard let self, self.bindingGeneration == generation, self.session != nil else { return }
            do {
                try self.clipboard.writeText(text)
                self.localError = nil
                self.localStatus = "已将远端文本复制到本机剪贴板。"
            } catch {
                self.localError = error.localizedDescription
                self.localStatus = nil
            }
        }
    }

    func detach() {
        session?.onRemoteClipboardText = nil
        session = nil
        bindingGeneration &+= 1
    }

    func sendLocalText(paste: Bool = false) {
        localError = nil
        localStatus = nil
        guard let session, session.canSendRemoteInput, !session.isClipboardBusy else {
            localError = "当前会话暂时无法传送剪贴板，请稍后重试。"
            return
        }
        do {
            let text = try clipboard.readText()
            guard text.utf8.count <= 48 * 1024 else { throw TextClipboardError.tooLarge }
            if !session.sendClipboardText(text, paste: paste) {
                localError = "剪贴板请求未能发送，请检查连接后重试。"
            }
        } catch {
            localError = error.localizedDescription
        }
    }

    func requestRemoteText() {
        localError = nil
        localStatus = nil
        guard let session, session.canSendRemoteInput, !session.isClipboardBusy,
              session.requestRemoteClipboard() else {
            localError = "当前会话暂时无法取回剪贴板，请稍后重试。"
            return
        }
    }
}
