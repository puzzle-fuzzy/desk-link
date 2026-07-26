import SwiftUI
import DeskLinkAppleCore

struct ControllerHomeView: View {
    @ObservedObject var bridge: ControllerBridge
    @State private var savedHosts: [SavedHost] = []
    @State private var storeError: String?
    private let savedHostStore = SavedHostStore()

    init(bridge: ControllerBridge) {
        self.bridge = bridge
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                pageHeading(
                    "连接设备",
                    detail: "连接未知设备，或从右侧选择这台 Mac 之前使用过的设备。"
                )

                HStack(alignment: .top, spacing: 20) {
                    ConnectView(bridge: bridge)
                        .frame(minWidth: 400, idealWidth: 480, maxWidth: 520, alignment: .top)

                    savedHostsPane
                        .frame(minWidth: 360, idealWidth: 420, maxWidth: 480, alignment: .top)
                }
                .frame(maxWidth: .infinity, alignment: .topLeading)

                if let error = bridge.userFacingError.isEmpty ? storeError : bridge.userFacingError {
                    DeskLinkErrorView(message: error)
                }
            }
            .padding(28)
            .frame(minWidth: 900, maxWidth: 1040, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .background(DeskLinkPalette.background)
        .onAppear(perform: loadSavedHosts)
        .onChange(of: bridge.state) { state in
            if case .connected = state {
                loadSavedHosts()
            }
        }
    }

    private var savedHostsPane: some View {
        DeskLinkPanel {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 5) {
                    Label("已连接过的设备", systemImage: "clock.arrow.circlepath")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    Text("连接信息由 macOS 钥匙串保护，只能在当前 Mac 上使用。")
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                }

                if savedHosts.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Image(systemName: "desktopcomputer.and.arrow.right")
                            .font(.system(size: 24, weight: .medium))
                            .foregroundStyle(DeskLinkPalette.mutedInk)
                        Text("还没有连接记录")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                        Text("第一次连接并获得设备批准后，它会自动出现在这里。")
                            .font(.system(size: 13))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.vertical, 10)
                } else {
                    VStack(spacing: 0) {
                        ForEach(savedHosts) { host in
                            savedHostRow(host)
                            if host.id != savedHosts.last?.id {
                                Divider()
                                    .padding(.vertical, 2)
                            }
                        }
                    }
                }
            }
        }
    }

    private func savedHostRow(_ host: SavedHost) -> some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: "desktopcomputer")
                .font(.system(size: 17, weight: .medium))
                .foregroundStyle(DeskLinkPalette.secondaryInk)
                .frame(width: 24)

            VStack(alignment: .leading, spacing: 4) {
                Text(host.serverName)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)
                    .lineLimit(1)
                Text("已保存到本机")
                    .font(.system(size: 11))
                    .foregroundStyle(DeskLinkPalette.mutedInk)
            }

            Spacer(minLength: 8)

            Button("连接") {
                bridge.connect(savedHost: host)
            }
            .buttonStyle(DeskLinkSecondaryButtonStyle())
            .disabled(isBusy)

            Button("移除", role: .destructive) {
                remove(host)
            }
            .buttonStyle(.borderless)
            .font(.system(size: 12, weight: .medium))
            .disabled(isBusy)
        }
        .padding(.vertical, 8)
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
