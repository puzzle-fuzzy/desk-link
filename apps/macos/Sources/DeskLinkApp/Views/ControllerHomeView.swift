import CryptoKit
import DeskLinkAppleCore
import SwiftUI

/// A digest of the public verification key distinguishes legacy records that
/// only contain a relay hostname. Never derive labels from session credentials.
func deskLinkSavedHostIdentity(_ host: SavedHost) -> String {
    SHA256.hash(data: Data(host.hostVerifyKey)).prefix(6)
        .map { String(format: "%02X", $0) }.joined()
}

func deskLinkSavedHostDeviceID(_ host: SavedHost) -> String? {
    guard let value = host.deviceID,
          value.count == 12,
          value.utf8.allSatisfy({ (48...57).contains($0) }) else { return nil }
    return stride(from: 0, to: value.count, by: 3).map { offset in
        let start = value.index(value.startIndex, offsetBy: offset)
        let end = value.index(start, offsetBy: 3)
        return String(value[start..<end])
    }.joined(separator: " ")
}

func deskLinkSavedHostTitle(_ host: SavedHost) -> String {
    if let name = host.displayName?.trimmingCharacters(in: .whitespacesAndNewlines), !name.isEmpty {
        return name
    }
    if let deviceID = deskLinkSavedHostDeviceID(host) { return deviceID }
    return "已保存设备 · \(deskLinkSavedHostIdentity(host))"
}

struct ControllerHomeView: View {
    @ObservedObject var bridge: ControllerBridge
    @ObservedObject var draft: DeskLinkConnectionDraft
    @State private var savedHosts: [SavedHost] = []
    @State private var storeError: String?
    @State private var renamingHost: SavedHost?
    @State private var pendingRemoval: SavedHost?
    @State private var alias = ""
    private let savedHostStore = SavedHostStore()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("连接设备")
                        .font(.system(size: 24, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    Text("输入设备信息，或选择这台 Mac 已连接过的设备。")
                        .font(.system(size: 13))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                }

                ViewThatFits(in: .horizontal) {
                    HStack(alignment: .top, spacing: 20) {
                        ConnectView(bridge: bridge, draft: draft, hasSavedHosts: !savedHosts.isEmpty)
                            .frame(minWidth: 330, maxWidth: .infinity, alignment: .top)
                        savedHostsPane
                            .frame(minWidth: 330, maxWidth: .infinity, alignment: .top)
                    }
                    VStack(alignment: .leading, spacing: 20) {
                        ConnectView(bridge: bridge, draft: draft, hasSavedHosts: !savedHosts.isEmpty)
                        savedHostsPane
                    }
                }

                if let error = bridge.userFacingError.isEmpty ? storeError : bridge.userFacingError {
                    DeskLinkErrorView(message: error)
                }
            }
            .padding(28)
            .frame(maxWidth: 1100, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .background(DeskLinkPalette.background)
        .onAppear(perform: loadSavedHosts)
        .onChange(of: bridge.state) { state in
            if case .connected = state { loadSavedHosts() }
        }
        .sheet(item: $renamingHost) { host in renameSheet(host) }
        .alert(item: $pendingRemoval) { host in
            Alert(
                title: Text("移除最近设备？"),
                message: Text("将从此 Mac 的钥匙串移除“\(deskLinkSavedHostTitle(host))”的连接记录。下次连接需要重新输入设备 ID 和密码，或重新获得批准。"),
                primaryButton: .destructive(Text("移除")) { remove(host) },
                secondaryButton: .cancel(Text("取消"))
            )
        }
    }

    private var savedHostsPane: some View {
        DeskLinkPanel {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 5) {
                    Label("最近设备", systemImage: "clock.arrow.circlepath")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    Text("连接信息由 macOS 钥匙串保护。可以为设备设置便于识别的名称。")
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                        .fixedSize(horizontal: false, vertical: true)
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
                            if host.id != savedHosts.last?.id { Divider().padding(.vertical, 4) }
                        }
                    }
                }
            }
        }
    }

    private func savedHostRow(_ host: SavedHost) -> some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: "desktopcomputer")
                .font(.system(size: 18))
                .foregroundStyle(DeskLinkPalette.secondaryInk)
                .frame(width: 24)
            VStack(alignment: .leading, spacing: 4) {
                Text(deskLinkSavedHostTitle(host))
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)
                    .lineLimit(1)
                    .help(deskLinkSavedHostTitle(host))
                Text(deskLinkSavedHostDeviceID(host).map { "设备 ID \($0)" }
                    ?? "身份摘要 \(deskLinkSavedHostIdentity(host))")
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(DeskLinkPalette.secondaryInk)
                    .textSelection(.enabled)
                if let connectedAt = host.lastConnectedAt {
                    Text("上次连接 \(connectedAt.formatted(date: .abbreviated, time: .shortened))")
                        .font(.system(size: 11))
                        .foregroundStyle(DeskLinkPalette.mutedInk)
                } else if host.deviceID == nil {
                    Text("旧版连接记录 · 可重命名")
                        .font(.system(size: 11))
                        .foregroundStyle(DeskLinkPalette.mutedInk)
                }
            }
            Spacer(minLength: 4)
            Button("连接") { draft.connect(host, using: bridge) }
                .buttonStyle(DeskLinkSecondaryButtonStyle())
                .disabled(isBusy)
            Menu {
                Button("重命名") {
                    alias = host.displayName ?? ""
                    renamingHost = host
                }
                Button("移除设备…", role: .destructive) { pendingRemoval = host }
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.system(size: 16))
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .help("管理 \(deskLinkSavedHostTitle(host))")
            .accessibilityLabel("管理 \(deskLinkSavedHostTitle(host))")
            .disabled(isBusy)
        }
        .padding(.vertical, 8)
    }

    private func renameSheet(_ host: SavedHost) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("重命名设备")
                .font(.system(size: 20, weight: .semibold))
            Text(deskLinkSavedHostDeviceID(host).map { "设备 ID \($0)" }
                ?? "身份摘要 \(deskLinkSavedHostIdentity(host))")
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(.secondary)
            TextField("设备名称，例如 家里的电脑", text: $alias)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("设备名称")
            Text("名称仅保存在此 Mac 上。留空可恢复默认名称，最多 80 个字符。")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            HStack {
                Spacer()
                Button("取消") { renamingHost = nil }
                    .keyboardShortcut(.cancelAction)
                Button("保存") { rename(host) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(alias.count > 80 || alias.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }))
            }
        }
        .padding(24)
        .frame(width: 430)
    }

    private var isBusy: Bool {
        deskLinkConnectionIsWaiting(bridge.state)
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

    private func rename(_ host: SavedHost) {
        do {
            try savedHostStore.rename(id: host.id, displayName: alias)
            renamingHost = nil
            loadSavedHosts()
        } catch {
            renamingHost = nil
            storeError = "无法保存设备名称，请稍后重试。"
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
