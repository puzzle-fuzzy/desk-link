import SwiftUI

struct ControllerHomeView: View {
    @ObservedObject var bridge: ControllerBridge
    let openSharing: () -> Void
    @State private var savedHosts: [SavedHost] = []
    @State private var storeError: String?
    private let savedHostStore = SavedHostStore()

    init(bridge: ControllerBridge, openSharing: @escaping () -> Void = {}) {
        self.bridge = bridge
        self.openSharing = openSharing
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                pageHeading("连接设备", detail: "输入连接码，开始控制另一台设备")

                ConnectView(bridge: bridge)

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

                DeskLinkPanel {
                    HStack(alignment: .center, spacing: 16) {
                        Image(systemName: "macbook.and.iphone")
                            .font(.system(size: 20))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                        VStack(alignment: .leading, spacing: 4) {
                            Text("允许别人连接此设备")
                                .font(.system(size: 14, weight: .semibold))
                                .foregroundStyle(DeskLinkPalette.ink)
                            Text("需要共享这台 Mac 时，先检查权限并生成连接码。")
                                .font(.system(size: 12))
                                .foregroundStyle(DeskLinkPalette.secondaryInk)
                        }
                        Spacer()
                        Button("共享此设备", action: openSharing)
                            .buttonStyle(DeskLinkSecondaryButtonStyle())
                    }
                }

                DisclosureGroup("连接诊断") {
                    VStack(alignment: .leading, spacing: 12) {
                        DiagnosticsView(bridge: bridge)

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
                        }
                    }
                    .padding(.top, 8)
                }
                .font(.system(size: 12, weight: .semibold))
                .padding(14)
                .background(DeskLinkPalette.subtle, in: RoundedRectangle(cornerRadius: 8))

                if let error = bridge.userFacingError.isEmpty ? storeError : bridge.userFacingError {
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

    private func pageHeading(_ title: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title)
                .font(.system(size: 24, weight: .semibold))
                .foregroundStyle(DeskLinkPalette.ink)
            Text(detail)
                .font(.system(size: 13))
                .foregroundStyle(DeskLinkPalette.secondaryInk)
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
