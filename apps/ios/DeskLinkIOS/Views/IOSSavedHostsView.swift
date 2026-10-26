import DeskLinkAppleCore
import SwiftUI

struct IOSSavedHostsView: View {
    @ObservedObject var controller: ControllerBridge
    @Binding var destination: IOSRootDestination
    @State private var hosts: [SavedHost] = []
    @State private var loadError: String?
    @State private var renamingHost: SavedHost?
    @State private var aliasDraft = ""

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
                                Text(host.displayTitle).font(.headline)
                                Spacer()
                                Button("重新连接") {
                                    destination = .connect
                                    controller.connect(savedHost: host)
                                }
                                .buttonStyle(.borderless)
                                .disabled(!host.canReconnect || isBusy)
                                Button(role: .destructive) { remove(host) } label: {
                                    Image(systemName: "trash")
                                }
                                .buttonStyle(.borderless)
                                .accessibilityLabel("删除 \(host.displayTitle)")
                            }
                            Text(host.canReconnect ? "认证材料保存在 Keychain" : "连接授权需更新，请重新配对")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 4)
                        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                            Button("删除", role: .destructive) { remove(host) }
                            Button("重命名") {
                                renamingHost = host
                                aliasDraft = host.alias ?? ""
                            }
                        }
                    }
                }
                if let loadError {
                    Text(loadError).foregroundStyle(.red)
                }
            }
            .navigationTitle("已保存设备")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("重新读取") { reload() }
                }
            }
            .task { reload() }
            .alert("重命名设备", isPresented: isRenaming) {
                TextField("设备名称", text: $aliasDraft)
                Button("保存") { saveAlias() }
                Button("取消", role: .cancel) { renamingHost = nil }
            } message: {
                Text("名称只保存在这台 iPhone 上。留空可恢复默认名称。")
            }
        }
    }

    private var isBusy: Bool {
        switch controller.state {
        case .pairing, .connecting, .reconnecting, .recovering: true
        default: false
        }
    }

    private var isRenaming: Binding<Bool> {
        Binding(get: { renamingHost != nil }, set: { if !$0 { renamingHost = nil } })
    }

    private func saveAlias() {
        guard let host = renamingHost else { return }
        do {
            try store.rename(id: host.id, alias: aliasDraft)
            renamingHost = nil
            reload()
        } catch {
            loadError = "无法更新设备名称，请使用 80 字以内的名称。"
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
