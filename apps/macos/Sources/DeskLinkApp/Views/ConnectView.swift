import Foundation
import DeskLinkAppleCore
import DeskLinkC
import SwiftUI

struct DeskLinkConnectionStatus: Equatable {
    let title: String
    let systemImage: String
}

private enum DeskLinkConnectionMethod: String {
    case qr
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
    @State private var deviceID = ""
    @State private var accessPassword = ""
    @State private var connectionMethod: DeskLinkConnectionMethod = .qr
    @State private var isShowingScanner = false
    @State private var scanError: String?

    var body: some View {
        DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 8) {
                    Image(systemName: connectionStatus.systemImage)
                    Text(connectionStatus.title)
                }
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(DeskLinkPalette.ink)

                Picker("连接方式", selection: $connectionMethod) {
                    Text("扫描二维码").tag(DeskLinkConnectionMethod.qr)
                    Text("设备 ID + 密码").tag(DeskLinkConnectionMethod.credentials)
                }
                .pickerStyle(.segmented)

                if connectionMethod == .qr {
                    VStack(alignment: .leading, spacing: 12) {
                        Label("用相机扫描连接二维码", systemImage: "qrcode.viewfinder")
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                        Text("二维码由另一台设备生成，识别后会自动开始连接。连接码内容不会显示在此页面。")
                            .font(.system(size: 12))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                            .fixedSize(horizontal: false, vertical: true)

                        Button {
                            scanError = nil
                            isShowingScanner = true
                        } label: {
                            Label("扫描二维码", systemImage: "camera.viewfinder")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(DeskLinkPrimaryButtonStyle())
                        .disabled(isBusy)

                        if let scanError {
                            Text(scanError)
                                .font(.system(size: 12))
                                .foregroundStyle(DeskLinkPalette.error)
                        }
                    }
                } else {
                    VStack(alignment: .leading, spacing: 12) {
                        Text("适用于支持设备 ID 和密码连接的主机。")
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
        .sheet(isPresented: $isShowingScanner) {
            QRCodeScannerSheet { payload in
                connectFromQRCode(payload)
            }
        }
    }

    private var connectionStatus: DeskLinkConnectionStatus {
        deskLinkConnectionStatus(for: bridge.state)
    }

    private var isBusy: Bool {
        switch bridge.state {
        case .pairing, .connecting, .reconnecting, .recovering: true
        default: false
        }
    }

    private var directoryCredentialsAreComplete: Bool {
        deviceID.filter(\.isNumber).count == 12
            && accessPassword.uppercased().filter {
                "23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains($0)
            }.count == Int(DESKLINK_DIRECTORY_ACCESS_CODE_BYTES)
    }

    private func connectFromQRCode(_ payload: String) {
        let normalized = payload.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let invite = Data(base64Encoded: normalized, options: .ignoreUnknownCharacters) else {
            scanError = "二维码内容无效，请让另一台设备重新生成二维码。"
            return
        }
        scanError = nil
        bridge.connect(invite: invite)
    }
}
