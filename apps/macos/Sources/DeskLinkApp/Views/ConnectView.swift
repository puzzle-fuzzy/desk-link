import AppKit
import SwiftUI

struct ConnectView: View {
    @ObservedObject var bridge: ControllerBridge
    @State private var inviteDraft = ""
    @State private var manualEntryVisible = false

    var body: some View {
        DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
            VStack(alignment: .leading, spacing: 12) {
                Label(connectionStatus, systemImage: connectionStatusImage)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)

                HStack(spacing: 8) {
                    Button("粘贴连接码") { connectFromPasteboard() }
                        .buttonStyle(DeskLinkPrimaryButtonStyle())
                        .help("从剪贴板读取完整连接码并开始连接")

                    Button("手动输入连接码") {
                        manualEntryVisible.toggle()
                    }
                    .buttonStyle(DeskLinkSecondaryButtonStyle())
                }

                if manualEntryVisible {
                    TextEditor(text: $inviteDraft)
                        .font(.system(size: 12, design: .monospaced))
                        .frame(minHeight: 84)
                        .overlay {
                            RoundedRectangle(cornerRadius: 6)
                                .stroke(DeskLinkPalette.border, lineWidth: 1)
                        }

                    Button("开始连接") {
                        connect(inviteCode: inviteDraft)
                    }
                    .buttonStyle(DeskLinkPrimaryButtonStyle())
                    .disabled(trimmedInviteDraft.isEmpty)
                }
            }
        }
    }

    private var trimmedInviteDraft: String {
        inviteDraft.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var connectionStatus: String {
        switch bridge.state {
        case .idle, .closed: "准备连接"
        case .connected: "已连接"
        case .failed: "需要处理"
        case .pairing, .connecting, .reconnecting, .recovering, .frozen: "连接中"
        }
    }

    private var connectionStatusImage: String {
        switch bridge.state {
        case .idle, .closed: "circle"
        case .connected: "checkmark.circle"
        case .failed: "exclamationmark.circle"
        case .pairing, .connecting, .reconnecting, .recovering, .frozen: "arrow.triangle.2.circlepath"
        }
    }

    private func connectFromPasteboard() {
        let pasted = NSPasteboard.general.string(forType: .string)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        inviteDraft = pasted
        if !pasted.isEmpty {
            connect(inviteCode: pasted)
        }
    }

    private func connect(inviteCode: String) {
        guard let invite = Data(base64Encoded: inviteCode.trimmingCharacters(in: .whitespacesAndNewlines)) else {
            bridge.connect(invite: Data())
            return
        }
        bridge.connect(invite: invite)
    }
}
