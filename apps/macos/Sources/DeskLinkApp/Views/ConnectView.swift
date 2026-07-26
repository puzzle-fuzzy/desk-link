import AppKit
import DeskLinkAppleCore
import DeskLinkC
import SwiftUI

struct DeskLinkConnectionStatus: Equatable {
    let title: String
    let systemImage: String
}

private enum DeskLinkConnectionMethod: String {
    case invite
    case credentials
}

func deskLinkConnectionStatus(for state: ConnectionState) -> DeskLinkConnectionStatus {
    switch state {
    case .idle, .closed:
        DeskLinkConnectionStatus(title: "准备连接", systemImage: "circle")
    case .pairing:
        DeskLinkConnectionStatus(title: "等待确认", systemImage: "person.crop.circle.badge.questionmark")
    case .connecting:
        DeskLinkConnectionStatus(title: "连接中", systemImage: "arrow.triangle.2.circlepath")
    case .connected:
        DeskLinkConnectionStatus(title: "已连接", systemImage: "checkmark.circle")
    case .reconnecting, .recovering:
        DeskLinkConnectionStatus(title: "正在恢复连接", systemImage: "arrow.triangle.2.circlepath")
    case .frozen:
        DeskLinkConnectionStatus(title: "画面暂时冻结", systemImage: "pause.circle")
    case .failed:
        DeskLinkConnectionStatus(title: "连接失败", systemImage: "exclamationmark.circle")
    }
}

struct ConnectView: View {
    @ObservedObject var bridge: ControllerBridge
    @State private var inviteDraft = ""
    @State private var deviceID = ""
    @State private var accessPassword = ""
    @State private var connectionMethod: DeskLinkConnectionMethod = .invite
    @State private var manualEntryVisible = false
    @FocusState private var isManualEntryFocused: Bool

    var body: some View {
        DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
            VStack(alignment: .leading, spacing: 12) {
                Label(connectionStatus.title, systemImage: connectionStatus.systemImage)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)

                Picker("连接方式", selection: $connectionMethod) {
                    Text("连接码").tag(DeskLinkConnectionMethod.invite)
                    Text("设备 ID + 密码").tag(DeskLinkConnectionMethod.credentials)
                }
                .pickerStyle(.segmented)
                .labelsHidden()

                if connectionMethod == .invite {
                    HStack(spacing: 8) {
                        Button("粘贴连接码") { connectFromPasteboard() }
                            .buttonStyle(DeskLinkPrimaryButtonStyle())
                            .help("从剪贴板读取完整连接码并开始连接")

                        Button("手动输入连接码") {
                            manualEntryVisible.toggle()
                            if manualEntryVisible {
                                DispatchQueue.main.async { isManualEntryFocused = true }
                            } else {
                                isManualEntryFocused = false
                            }
                        }
                        .buttonStyle(DeskLinkSecondaryButtonStyle())
                    }

                    if manualEntryVisible {
                        Text("连接码")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)

                        TextEditor(text: $inviteDraft)
                            .font(.system(size: 12, design: .monospaced))
                            .frame(minHeight: 84)
                            .focused($isManualEntryFocused)
                            .accessibilityLabel("连接码")
                            .accessibilityHint("输入或粘贴另一台设备生成的完整连接码")
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
                } else {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("连接 Windows 或其他支持设备密码的主机")
                            .font(.system(size: 12))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)

                        TextField("设备 ID，例如 123 456 789 012", text: $deviceID)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 13, design: .monospaced))
                            .accessibilityLabel("设备 ID")

                        SecureField("临时密码或固定密码", text: $accessPassword)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 13, design: .monospaced))
                            .accessibilityLabel("访问密码")

                        Button("开始连接") {
                            bridge.connect(deviceID: deviceID, temporaryPassword: accessPassword)
                        }
                        .buttonStyle(DeskLinkPrimaryButtonStyle())
                        .disabled(!directoryCredentialsAreComplete)
                    }
                }
            }
        }
    }

    private var trimmedInviteDraft: String {
        inviteDraft.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var connectionStatus: DeskLinkConnectionStatus {
        deskLinkConnectionStatus(for: bridge.state)
    }

    private var directoryCredentialsAreComplete: Bool {
        deviceID.filter(\.isNumber).count == 12
            && accessPassword.uppercased().filter {
                "23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains($0)
            }.count == Int(DESKLINK_DIRECTORY_ACCESS_CODE_BYTES)
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
