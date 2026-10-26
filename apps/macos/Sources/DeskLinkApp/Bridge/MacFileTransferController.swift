import AppKit
import Combine
import DeskLinkAppleCore

@MainActor
protocol FileTransferPicker {
    func outgoingFiles() async -> [URL]
    func destinationDirectory() async -> URL?
}

@MainActor
struct NativeFileTransferPicker: FileTransferPicker {
    func outgoingFiles() async -> [URL] {
        let panel = NSOpenPanel()
        panel.title = "选择发送到远端的文件"
        panel.prompt = "加入发送队列"
        panel.canChooseFiles = true; panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        return await present(panel) == .OK ? panel.urls : []
    }
    func destinationDirectory() async -> URL? {
        let panel = NSOpenPanel()
        panel.title = "选择远端文件的保存位置"
        panel.prompt = "保存到此文件夹"
        panel.canChooseFiles = false; panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        return await present(panel) == .OK ? panel.url : nil
    }
    private func present(_ panel: NSOpenPanel) async -> NSApplication.ModalResponse {
        await withCheckedContinuation { continuation in
            if let window = NSApp.keyWindow {
                panel.beginSheetModal(for: window) { continuation.resume(returning: $0) }
            } else { panel.begin { continuation.resume(returning: $0) } }
        }
    }
}

struct QueuedOutgoingFile: Identifiable {
    let id = UUID()
    let url: URL
}

/// UI-only file choices and sequential queue. No file contents are read here.
@MainActor
final class MacFileTransferController: ObservableObject {
    @Published private(set) var queuedFiles: [QueuedOutgoingFile] = []
    @Published private(set) var recentTransfers: [RemoteFileTransfer] = []
    @Published private(set) var isPicking = false
    @Published private(set) var isQueuePaused = false
    @Published private(set) var localError: String?
    private let picker: any FileTransferPicker
    private weak var media: ControllerMediaBridge?
    private var subscriptions = Set<AnyCancellable>()
    private var activeOutgoing: URL?
    private var retryOperation: RetryOperation?
    private var context: UInt64 = 0
    private var bindingGeneration: UInt64 = 0
    private enum RetryOperation { case upload(URL), download(URL) }
    var canRetry: Bool { retryOperation != nil && media?.isConnected == true && media?.isFileBusy != true }

    init(picker: any FileTransferPicker = NativeFileTransferPicker()) { self.picker = picker }

    func attach(to media: ControllerMediaBridge) {
        if self.media === media { return }
        detach()
        self.media = media
        context = media.contextID
        let binding = bindingGeneration
        media.$fileTransfer.dropFirst().sink { [weak self] transfer in
            // Published emits before its storage changes. Defer queue advance
            // until isFileBusy and the record refer to the same terminal state.
            let context = media.contextID
            Task { @MainActor [weak self, weak media] in
                guard let self, let media, self.media === media,
                      self.bindingGeneration == binding, media.contextID == context else { return }
                self.consume(transfer)
            }
        }.store(in: &subscriptions)
        media.$contextID.dropFirst().sink { [weak self] value in
            self?.connectionChanged(value)
        }.store(in: &subscriptions)
    }

    func detach() {
        bindingGeneration &+= 1
        subscriptions.removeAll()
        media = nil; queuedFiles = []; activeOutgoing = nil; retryOperation = nil
        isQueuePaused = false
    }

    func chooseFiles() async {
        guard let media, media.isConnected, !isPicking else { return }
        let context = media.contextID
        let binding = bindingGeneration
        isPicking = true
        let urls = await picker.outgoingFiles()
        isPicking = false
        guard self.media === media, bindingGeneration == binding, media.contextID == context, media.isConnected else {
            if !urls.isEmpty { localError = "连接已变化，请重新选择文件。" }; return
        }
        guard queuedFiles.count + urls.count <= 32 else { localError = "每次队列最多保留 32 个文件。"; return }
        queuedFiles.append(contentsOf: urls.map { QueuedOutgoingFile(url: $0) })
        localError = nil
        startNext()
    }

    func chooseDestinationAndRequest() async {
        guard let media, media.isConnected, !media.isFileBusy, queuedFiles.isEmpty, !isPicking else { return }
        let context = media.contextID
        let binding = bindingGeneration
        isPicking = true
        let directory = await picker.destinationDirectory()
        isPicking = false
        guard let directory else { return }
        guard self.media === media, bindingGeneration == binding, media.contextID == context, media.isConnected else {
            localError = "连接已变化，请重新选择保存位置。"; return
        }
        retryOperation = .download(directory)
        localError = media.requestRemoteFile(to: directory) ? nil : "无法获取远端文件，请稍后重试。"
    }

    func retry() {
        guard canRetry, let media, let retryOperation else { return }
        localError = nil
        switch retryOperation {
        case let .upload(url):
            activeOutgoing = url
            if !media.sendFile(url) { activeOutgoing = nil; localError = "文件未能发送，请重试。" }
        case let .download(directory):
            if !media.requestRemoteFile(to: directory) { localError = "获取请求未能发送，请重试。" }
        }
    }

    func resumeQueue() { isQueuePaused = false; localError = nil; startNext() }
    func clearQueue() { queuedFiles = []; isQueuePaused = false }
    func cancel() {
        isQueuePaused = true
        _ = media?.cancelFileTransfer()
    }
    func reveal(_ transfer: RemoteFileTransfer) {
        guard transfer.state == .completed, transfer.direction == .download, let path = transfer.savedPath else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    private func connectionChanged(_ value: UInt64) {
        guard value != context else { return }
        context = value
        if activeOutgoing != nil || !queuedFiles.isEmpty {
            localError = "连接已变化，未完成的队列已停止，请重新选择文件。"
        }
        queuedFiles = []; activeOutgoing = nil; retryOperation = nil; isQueuePaused = false
    }

    private func startNext() {
        guard let media, media.isConnected, !media.isFileBusy, !isQueuePaused, !queuedFiles.isEmpty else { return }
        let item = queuedFiles.removeFirst()
        activeOutgoing = item.url
        retryOperation = .upload(item.url)
        if !media.sendFile(item.url) {
            activeOutgoing = nil; isQueuePaused = true
            localError = "文件未能发送，队列已暂停。"
        }
    }

    private func consume(_ transfer: RemoteFileTransfer?) {
        guard let transfer, transfer.state.isTerminal else { return }
        let key: (RemoteFileTransfer) -> String = { "\($0.requestID):\($0.transferID)" }
        guard !recentTransfers.contains(where: { key($0) == key(transfer) }) else { return }
        recentTransfers.insert(transfer, at: 0)
        recentTransfers = Array(recentTransfers.prefix(12))
        activeOutgoing = nil
        if transfer.state == .completed {
            retryOperation = nil
            startNext()
        } else { isQueuePaused = true }
    }
}
