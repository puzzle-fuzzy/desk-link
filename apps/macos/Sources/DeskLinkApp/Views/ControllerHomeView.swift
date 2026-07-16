import SwiftUI

struct ControllerHomeView: View {
    @ObservedObject var bridge: ControllerBridge
    @State private var savedHosts: [SavedHost] = []
    @State private var storeError: String?
    private let savedHostStore = SavedHostStore()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                DeskLinkPanel(background: statusBackground) {
                    HStack(alignment: .center, spacing: 24) {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack(spacing: 9) {
                                DeskLinkStatusLight(color: statusColor)
                                Text("控制端")
                                    .font(.system(size: 12, weight: .semibold))
                                    .foregroundStyle(DeskLinkPalette.secondaryInk)
                            }
                            Text(statusTitle)
                                .font(.system(size: 24, weight: .semibold))
                                .foregroundStyle(DeskLinkPalette.ink)
                            Text(statusDetail)
                                .font(.system(size: 14))
                                .foregroundStyle(DeskLinkPalette.secondaryInk)
                        }
                        Spacer(minLength: 12)
                        ConnectView(bridge: bridge)
                    }
                }

                DeskLinkPanel {
                    VStack(alignment: .leading, spacing: 14) {
                        HStack(alignment: .firstTextBaseline) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("已保存的远程设备")
                                    .font(.system(size: 16, weight: .semibold))
                                    .foregroundStyle(DeskLinkPalette.ink)
                                Text("连接信息由 macOS 钥匙串保护，只能在当前设备上使用。")
                                    .font(.system(size: 12))
                                    .foregroundStyle(DeskLinkPalette.mutedInk)
                            }
                            Spacer()
                            Button("重新读取") { loadSavedHosts() }
                                .buttonStyle(DeskLinkSecondaryButtonStyle())
                        }

                        if savedHosts.isEmpty {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("还没有已保存设备")
                                    .font(.system(size: 13, weight: .semibold))
                                    .foregroundStyle(DeskLinkPalette.ink)
                                Text("请从另一台设备复制连接码，首次连接获得批准后会自动保存在这里。")
                                    .font(.system(size: 13))
                                    .foregroundStyle(DeskLinkPalette.secondaryInk)
                            }
                            .padding(.vertical, 6)
                        } else {
                            VStack(spacing: 0) {
                                ForEach(Array(savedHosts.enumerated()), id: \.offset) { index, host in
                                    HStack(spacing: 14) {
                                        Image(systemName: "desktopcomputer")
                                            .font(.system(size: 18))
                                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                                        VStack(alignment: .leading, spacing: 4) {
                                            Text("已批准的远程设备")
                                                .font(.system(size: 13, weight: .semibold))
                                                .foregroundStyle(DeskLinkPalette.ink)
                                            Text(host.serverName)
                                                .font(.system(size: 11, design: .monospaced))
                                                .foregroundStyle(DeskLinkPalette.mutedInk)
                                        }
                                        Spacer()
                                        Button("连接设备") { bridge.connect(savedHost: host) }
                                            .buttonStyle(DeskLinkPrimaryButtonStyle())
                                            .disabled(isBusy)
                                        Button("移除记录") { remove(host) }
                                            .buttonStyle(DeskLinkSecondaryButtonStyle())
                                            .disabled(isBusy)
                                    }
                                    .padding(.vertical, 8)
                                    if index < savedHosts.count - 1 {
                                        Divider()
                                    }
                                }
                            }
                        }
                    }
                }

                if let verifyKey = bridge.controllerVerifyKeyHex {
                    DisclosureGroup("此 Mac 的控制端校验信息") {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("仅在排查设备身份问题时使用。")
                                .font(.system(size: 11))
                                .foregroundStyle(DeskLinkPalette.mutedInk)
                            Text(verifyKey)
                                .font(.system(size: 11, design: .monospaced))
                                .foregroundStyle(DeskLinkPalette.secondaryInk)
                                .textSelection(.enabled)
                        }
                        .padding(.top, 8)
                    }
                    .font(.system(size: 12, weight: .semibold))
                    .padding(14)
                    .background(DeskLinkPalette.subtle, in: RoundedRectangle(cornerRadius: 8))
                }

                DiagnosticsView(bridge: bridge)

                if let error = bridge.lastError ?? storeError {
                    DeskLinkErrorView(message: error)
                }
            }
            .padding(28)
            .frame(maxWidth: 1040, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .background(DeskLinkPalette.surface)
        .onAppear(perform: loadSavedHosts)
    }

    private var isBusy: Bool {
        switch bridge.state {
        case .pairing, .connecting, .reconnecting, .recovering: true
        default: false
        }
    }

    private var statusTitle: String {
        switch bridge.state {
        case .idle, .closed: "选择要控制的设备"
        case .pairing: "等待另一台设备批准"
        case .connecting: "正在建立安全连接"
        case .connected: "远程控制已连接"
        case .reconnecting: "正在恢复连接"
        case .recovering: "正在恢复远程画面"
        case .frozen: "远程画面已暂停"
        case .failed: "连接未建立"
        }
    }

    private var statusDetail: String {
        switch bridge.state {
        case .idle, .closed:
            "粘贴另一台设备创建的连接码，或从已保存列表直接重新连接。"
        case .pairing:
            "请回到被控制设备，核对身份并批准此 Mac。"
        case .connecting:
            "DeskLink 正在连接中继并验证另一台设备的身份。"
        case .connected:
            "远程画面和输入通道已经启用。"
        case .reconnecting:
            "网络连接暂时中断，DeskLink 会自动恢复。"
        case .recovering:
            "连接仍然安全，正在等待新的关键帧。"
        case .frozen:
            "没有收到可显示的新画面，请请求关键帧或重新连接。"
        case .failed(let message):
            deskLinkChineseError(message)
        }
    }

    private var statusColor: Color {
        switch bridge.state {
        case .connected: DeskLinkPalette.success
        case .pairing, .connecting, .reconnecting, .recovering: DeskLinkPalette.info
        case .frozen: DeskLinkPalette.warning
        case .failed: DeskLinkPalette.error
        case .idle, .closed: DeskLinkPalette.success
        }
    }

    private var statusBackground: Color {
        switch bridge.state {
        case .connected, .idle, .closed: DeskLinkPalette.successSurface
        case .pairing, .connecting, .reconnecting, .recovering: DeskLinkPalette.infoSurface
        case .frozen: DeskLinkPalette.warningSurface
        case .failed: DeskLinkPalette.errorSurface
        }
    }

    private func loadSavedHosts() {
        do {
            savedHosts = try savedHostStore.loadAll()
            storeError = nil
        } catch {
            savedHosts = []
            storeError = "无法读取钥匙串中的已保存设备。"
        }
    }

    private func remove(_ host: SavedHost) {
        do {
            try savedHostStore.remove(id: host.id)
            loadSavedHosts()
        } catch {
            storeError = "无法从钥匙串移除此设备。"
        }
    }
}
