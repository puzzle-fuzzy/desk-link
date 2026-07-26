import DeskLinkAppleCore
import SwiftUI

struct IOSSavedHostsView: View {
    @ObservedObject var controller: ControllerBridge
    @State private var hosts: [SavedHost] = []
    @State private var loadError: String?

    private let store = SavedHostStore()

    var body: some View {
        NavigationStack {
            List {
                if hosts.isEmpty {
                    VStack(spacing: 12) {
                        Image(systemName: "externaldrive")
                            .font(.largeTitle)
                            .foregroundStyle(.secondary)
                        Text("暂无已保存设备")
                            .font(.headline)
                        Text("完成一次配对并获得远端确认后，设备会出现在这里。")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 48)
                } else {
                    ForEach(hosts) { host in
                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Text(host.serverName).font(.headline)
                                Spacer()
                                Button("重新连接") { controller.connect(savedHost: host) }
                                    .buttonStyle(.borderless)
                            }
                            Text("认证材料保存在 Keychain")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 4)
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button("删除", role: .destructive) { remove(host) }
                        }
                    }
                }
                if let loadError {
                    Text(loadError).foregroundStyle(.red)
                }
                if let lastError = controller.lastError {
                    Section("连接状态") {
                        Text(lastError).foregroundStyle(.red)
                        Text("记录仍然保留，你可以重试或删除它。")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("已保存设备")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("重新读取") { reload() }
                }
            }
            .task { reload() }
        }
    }

    private func reload() {
        do {
            hosts = try store.loadAll()
            loadError = nil
        } catch {
            loadError = "无法读取已保存设备。"
        }
    }

    private func remove(_ host: SavedHost) {
        do {
            try store.remove(id: host.id)
            reload()
        } catch {
            loadError = "无法删除设备记录。"
        }
    }
}
