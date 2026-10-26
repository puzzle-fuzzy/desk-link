import DeskLinkAppleCore
import SwiftUI

struct SessionTransferPanel: View {
    @ObservedObject var media: ControllerMediaBridge
    @ObservedObject var files: MacFileTransferController
    let prepareForDialog: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("文件传输").font(.title2.bold())
                Spacer()
                Button("完成") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text("单个文件最多 256 MB；接收完成后校验内容，同名文件会保留两份。")
                .font(.subheadline).foregroundStyle(.secondary)
            HStack {
                Button("发送文件…") {
                    prepareForDialog()
                    Task { await files.chooseFiles() }
                }
                .disabled(!media.isConnected || files.isPicking)
                Button("获取远端文件…") {
                    prepareForDialog()
                    Task { await files.chooseDestinationAndRequest() }
                }
                .disabled(!media.isConnected || media.isFileBusy || !files.queuedFiles.isEmpty || files.isPicking)
                Spacer()
                if files.isPicking { ProgressView().controlSize(.small) }
            }
            if let transfer = media.fileTransfer { transferRow(transfer, active: true) }
            else {
                Text(media.isConnected ? "选择本机文件发送，或请远端选择文件传回。" : "连接远端后可传输文件。")
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, minHeight: 70)
            }
            if let error = files.localError ?? media.fileError {
                Text(error).font(.callout).foregroundStyle(.red)
            }
            if files.canRetry || files.isQueuePaused || !files.queuedFiles.isEmpty {
                HStack {
                    if files.canRetry { Button("重试此文件") { files.retry() } }
                    if files.isQueuePaused, !files.queuedFiles.isEmpty {
                        Button("继续队列") { files.resumeQueue() }.disabled(media.isFileBusy || !media.isConnected)
                    }
                    Spacer()
                    if !files.queuedFiles.isEmpty { Button("清空等待队列") { files.clearQueue() } }
                }
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    if !files.queuedFiles.isEmpty {
                        Text("等待发送（\(files.queuedFiles.count)）\(files.isQueuePaused ? " · 已暂停" : "")").font(.headline)
                        ForEach(files.queuedFiles) { item in
                            Label(item.url.lastPathComponent, systemImage: "doc").lineLimit(1).truncationMode(.middle)
                        }
                    }
                    let history = files.recentTransfers.filter { $0 != media.fileTransfer }
                    if !history.isEmpty {
                        Text("本次窗口的传输记录").font(.headline)
                        ForEach(Array(history.enumerated()), id: \.offset) { _, transfer in
                            transferRow(transfer, active: false)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 60, maxHeight: 240)
            Text("获取文件时，先选择本机保存文件夹，再由远端选择要发送的文件。")
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(24).frame(width: 570)
    }

    private func transferRow(_ transfer: RemoteFileTransfer, active: Bool) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Image(systemName: transfer.direction == .upload ? "arrow.up.doc" : "arrow.down.doc")
                Text(transfer.name).lineLimit(1).truncationMode(.middle)
                Spacer()
                Text(transfer.state.title).font(.caption).foregroundStyle(.secondary)
            }
            if !transfer.state.isTerminal {
                if transfer.state == .preparing || transfer.state == .waiting { ProgressView().controlSize(.small) }
                else { ProgressView(value: transfer.progress) }
            }
            HStack {
                if transfer.total > 0 {
                    Text("\(ByteCountFormatter.string(fromByteCount: Int64(transfer.transferred), countStyle: .file)) / \(ByteCountFormatter.string(fromByteCount: Int64(transfer.total), countStyle: .file))")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if active && !transfer.state.isTerminal {
                    Button(media.fileCancelPending ? "正在取消…" : "取消传输") { files.cancel() }
                        .disabled(media.fileCancelPending || !media.isConnected)
                }
                if transfer.state == .completed && transfer.savedPath != nil {
                    Button("在访达中显示") { files.reveal(transfer) }
                }
            }
        }
        .padding(12).background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 10))
    }
}
