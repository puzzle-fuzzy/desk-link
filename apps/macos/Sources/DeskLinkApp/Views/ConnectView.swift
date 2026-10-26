import DeskLinkAppleCore
import DeskLinkC
import SwiftUI

struct DeskLinkConnectionStatus: Equatable {
    let title: String
    let systemImage: String
}

func deskLinkConnectionIsWaiting(_ state: ConnectionState) -> Bool {
    switch state {
    case .pairing, .connecting, .reconnecting, .recovering: true
    default: false
    }
}

func deskLinkConnectionWaitingDetail(_ state: ConnectionState) -> String? {
    switch state {
    case .connecting: "正在查找设备并建立安全连接。可以取消后更换设备。"
    case .pairing: "请在另一台设备上批准此次连接。可以随时取消。"
    case .reconnecting: "网络暂时中断，正在重新连接。可以取消后更换设备。"
    case .recovering: "正在等待远程画面恢复。可以随时取消。"
    default: nil
    }
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
    @State private var deviceID = ""
    @State private var accessPassword = ""

    var body: some View {
        DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 8) {
                    Image(systemName: connectionStatus.systemImage)
                    Text(connectionStatus.title)
                }
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(DeskLinkPalette.ink)

                if let detail = deskLinkConnectionWaitingDetail(bridge.state) {
                    VStack(alignment: .leading, spacing: 12) {
                        HStack(alignment: .top, spacing: 10) {
                            ProgressView().controlSize(.small)
                            Text(detail)
                                .font(.system(size: 12))
                                .foregroundStyle(DeskLinkPalette.secondaryInk)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Button("取消连接") { bridge.disconnect() }
                            .buttonStyle(DeskLinkSecondaryButtonStyle())
                            .keyboardShortcut(.cancelAction)
                            .accessibilityIdentifier("cancel-controller-connection")
                    }
                }

                VStack(alignment: .leading, spacing: 12) {
                    Text("使用设备 ID 和密码连接")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    Text("输入另一台主机显示的设备 ID 和访问密码。连接成功并获得批准后，设备会保存到右侧列表。")
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                        .fixedSize(horizontal: false, vertical: true)

                    TextField("设备 ID，例如 123 456 789 012", text: $deviceID)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 13, design: .monospaced))
                        .accessibilityLabel("设备 ID")
                        .disabled(isBusy)

                    SecureField("临时密码或固定密码", text: $accessPassword)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 13, design: .monospaced))
                        .accessibilityLabel("访问密码")
                        .disabled(isBusy)

                    Button {
                        bridge.connect(deviceID: deviceID, temporaryPassword: accessPassword)
                    } label: {
                        Text("连接设备")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(DeskLinkPrimaryButtonStyle())
                    .keyboardShortcut(.defaultAction)
                    .disabled(isBusy || !directoryCredentialsAreComplete)
                }
            }
        }
    }

    private var connectionStatus: DeskLinkConnectionStatus {
        deskLinkConnectionStatus(for: bridge.state)
    }

    private var isBusy: Bool {
        deskLinkConnectionIsWaiting(bridge.state)
    }

    private var directoryCredentialsAreComplete: Bool {
        deviceID.filter(\.isNumber).count == 12
            && accessPassword.uppercased().filter {
                "23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains($0)
            }.count == Int(DESKLINK_DIRECTORY_ACCESS_CODE_BYTES)
    }
}
