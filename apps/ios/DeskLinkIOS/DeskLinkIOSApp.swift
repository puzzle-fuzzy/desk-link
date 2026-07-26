import DeskLinkAppleCore
import SwiftUI

@main
struct DeskLinkIOSApp: App {
    @StateObject private var controller: ControllerBridge

    init() {
        _controller = StateObject(
            wrappedValue: ControllerBridge(configuration: IOSRuntimeConfiguration.production)
        )
    }

    var body: some Scene {
        WindowGroup {
            IOSConnectionHomeView(controller: controller)
        }
    }
}

struct IOSConnectionHomeView: View {
    @ObservedObject var controller: ControllerBridge
    @State private var inviteText = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("连接设备") {
                    TextField("粘贴连接码", text: $inviteText, axis: .vertical)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("开始连接") { connect() }
                    Button("断开连接", role: .destructive) { controller.disconnect() }
                }
                Section("状态") {
                    Text(statusText)
                    if let error = controller.lastError {
                        Text(error).foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle("DeskLink")
        }
    }

    private var statusText: String {
        switch controller.state {
        case .idle: "准备连接"
        case .pairing: "等待确认"
        case .connecting: "连接中"
        case .connected: "已连接"
        case .reconnecting: "正在重新连接"
        case .recovering: "正在恢复画面"
        case .frozen: "画面暂时冻结"
        case .closed: "已断开"
        case .failed: "连接失败"
        }
    }

    private func connect() {
        guard let invite = Data(base64Encoded: inviteText.trimmingCharacters(in: .whitespacesAndNewlines)) else {
            controller.connect(invite: Data())
            return
        }
        controller.connect(invite: invite)
    }
}
