import AppKit
import SwiftUI

enum HostHomePage {
    case overview
    case connection
    case devices
}

struct HostHomeView: View {
    @ObservedObject var bridge: HostBridge
    let page: HostHomePage
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ScrollView {
            Group {
                switch page {
                case .overview:
                    overview
                case .connection:
                    connection
                case .devices:
                    devices
                }
            }
            .padding(28)
            .frame(maxWidth: 1040, alignment: .leading)
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .background(DeskLinkPalette.surface)
        .onAppear { bridge.refreshPermissions() }
        .onChange(of: scenePhase) { phase in
            if phase == .active { bridge.refreshPermissions() }
        }
    }

    private var overview: some View {
        VStack(alignment: .leading, spacing: 16) {
            pageHeading(
                "设置 / 诊断",
                detail: "检查本机共享权限、已批准设备和运行信息。"
            )

            DeskLinkPanel {
                VStack(spacing: 0) {
                    permissionRow(
                        title: "屏幕录制",
                        detail: "允许 DeskLink 读取此 Mac 的画面。",
                        granted: bridge.permissions.screenRecording == .granted,
                        request: bridge.requestScreenRecording,
                        settingsURL: bridge.permissions.screenRecordingSettingsURL
                    )
                    Divider().padding(.vertical, 14)
                    permissionRow(
                        title: "辅助功能",
                        detail: "允许已批准设备发送键盘与鼠标输入。",
                        granted: bridge.permissions.accessibility == .granted,
                        request: bridge.requestAccessibility,
                        settingsURL: bridge.permissions.accessibilitySettingsURL
                    )
                }
            }

            DeskLinkPanel {
                VStack(alignment: .leading, spacing: 14) {
                    HStack(alignment: .firstTextBaseline) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("已批准的访问")
                                .font(.system(size: 16, weight: .semibold))
                                .foregroundStyle(DeskLinkPalette.ink)
                            Text("只有在此 Mac 上批准过的控制端才能建立加密会话。")
                                .font(.system(size: 12))
                                .foregroundStyle(DeskLinkPalette.mutedInk)
                        }
                        Spacer()
                        Text("共 \(bridge.trustedControllers.count) 台")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                    }
                    if bridge.trustedControllers.isEmpty {
                        Text("还没有已批准设备。创建连接码，并在另一台设备发起连接后在这里确认。")
                            .font(.system(size: 13))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                    } else {
                        ForEach(Array(bridge.trustedControllers.prefix(3).enumerated()), id: \.offset) { _, controller in
                            trustedControllerSummary(controller)
                        }
                    }
                }
            }

            DeskLinkPanel {
                VStack(alignment: .leading, spacing: 12) {
                    Text("仅在诊断时展开")
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                    DisclosureGroup("运行指标") {
                        HStack(spacing: 24) {
                            metric("视频数据包", value: bridge.metrics.sentVideoPackets)
                            metric("输入事件", value: bridge.metrics.receivedInputEvents)
                            metric("关键帧请求", value: bridge.metrics.keyframeRequests)
                        }
                        .padding(.top, 8)
                    }
                    .font(.system(size: 12, weight: .semibold))
                }
            }

            if let error = bridge.lastError {
                DeskLinkErrorView(message: error)
            }
        }
    }

    private var connection: some View {
        VStack(alignment: .leading, spacing: 16) {
            pageHeading(
                "共享此设备",
                detail: "需要让别人控制这台 Mac 时，先完成权限检查并生成连接码。"
            )

            DeskLinkPanel {
                VStack(spacing: 0) {
                    permissionRow(
                        title: "屏幕录制",
                        detail: "允许 DeskLink 读取此 Mac 的画面。",
                        granted: bridge.permissions.screenRecording == .granted,
                        request: bridge.requestScreenRecording,
                        settingsURL: bridge.permissions.screenRecordingSettingsURL
                    )
                    Divider().padding(.vertical, 14)
                    permissionRow(
                        title: "辅助功能",
                        detail: "允许已批准设备发送键盘与鼠标输入。",
                        granted: bridge.permissions.accessibility == .granted,
                        request: bridge.requestAccessibility,
                        settingsURL: bridge.permissions.accessibilitySettingsURL
                    )
                }
            }

            DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
                HStack(alignment: .center, spacing: 20) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(bridge.pairingInvite == nil ? "创建安全连接码" : "连接码可以使用")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                        if let invite = bridge.pairingInvite {
                            Text("有效期至 \(invite.expiresAt.formatted(date: .omitted, time: .shortened))。连接码只应发送给你正在操作的另一台设备。")
                                .font(.system(size: 12))
                                .foregroundStyle(DeskLinkPalette.secondaryInk)
                        } else {
                            Text("连接码包含一次性加入凭据，不会在此页面明文显示。")
                                .font(.system(size: 12))
                                .foregroundStyle(DeskLinkPalette.secondaryInk)
                        }
                    }
                    Spacer()
                    if bridge.pairingInvite == nil {
                        Button("创建连接码") { bridge.createInvite() }
                            .buttonStyle(DeskLinkPrimaryButtonStyle())
                            .disabled(!bridge.permissions.canCaptureAndControl)
                    } else {
                        Button("复制连接码") { bridge.copyInviteToPasteboard() }
                            .buttonStyle(DeskLinkPrimaryButtonStyle())
                        Button("停止共享") { bridge.stop() }
                            .buttonStyle(DeskLinkSecondaryButtonStyle())
                    }
                }
            }

            if let error = bridge.lastError {
                DeskLinkErrorView(message: error)
            }
        }
    }

    private var devices: some View {
        VStack(alignment: .leading, spacing: 16) {
            pageHeading(
                "已批准设备",
                detail: "撤销后，该设备保存的连接信息将不能再次控制此 Mac。"
            )
            DeskLinkPanel {
                if bridge.trustedControllers.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("还没有已批准设备")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                        Text("请先在“共享此设备”创建连接码，然后在另一台设备发起连接并在此 Mac 上批准。")
                            .font(.system(size: 13))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                    }
                } else {
                    VStack(spacing: 0) {
                        ForEach(Array(bridge.trustedControllers.enumerated()), id: \.offset) { index, controller in
                            HStack(alignment: .center, spacing: 16) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(controller.displayName)
                                        .font(.system(size: 13, weight: .semibold))
                                        .foregroundStyle(DeskLinkPalette.ink)
                                        .lineLimit(1)
                                    Text(deviceID(controller.deviceID))
                                        .font(.system(size: 11, design: .monospaced))
                                        .foregroundStyle(DeskLinkPalette.mutedInk)
                                        .textSelection(.enabled)
                                }
                                Spacer()
                                Text("批准于 \(date(controller.approvedAtUnixSeconds))")
                                    .font(.system(size: 11))
                                    .foregroundStyle(DeskLinkPalette.mutedInk)
                                Button("撤销设备") { bridge.revoke(controller: controller) }
                                    .buttonStyle(DeskLinkSecondaryButtonStyle())
                            }
                            .padding(.vertical, 8)
                            if index < bridge.trustedControllers.count - 1 {
                                Divider()
                            }
                        }
                    }
                }
            }
            if let error = bridge.lastError {
                DeskLinkErrorView(message: error)
            }
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

    private func permissionRow(
        title: String,
        detail: String,
        granted: Bool,
        request: @escaping () -> Void,
        settingsURL: URL
    ) -> some View {
        HStack(alignment: .center, spacing: 16) {
            Image(systemName: granted ? "checkmark.circle.fill" : "circle")
                .font(.system(size: 18))
                .foregroundStyle(granted ? DeskLinkPalette.success : DeskLinkPalette.warning)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)
                Text(granted ? "已允许。\(detail)" : "尚未允许。\(detail)")
                    .font(.system(size: 12))
                    .foregroundStyle(DeskLinkPalette.secondaryInk)
            }
            Spacer()
            if !granted {
                Button("请求权限", action: request)
                    .buttonStyle(DeskLinkPrimaryButtonStyle())
                Button("打开系统设置") { NSWorkspace.shared.open(settingsURL) }
                    .buttonStyle(DeskLinkSecondaryButtonStyle())
            }
        }
    }

    private func trustedControllerSummary(_ controller: TrustedController) -> some View {
        HStack(spacing: 12) {
            Image(systemName: "desktopcomputer")
                .foregroundStyle(DeskLinkPalette.secondaryInk)
            VStack(alignment: .leading, spacing: 3) {
                Text(controller.displayName)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)
                    .lineLimit(1)
                Text("上次记录：\(date(controller.lastSeenAtUnixSeconds))")
                    .font(.system(size: 11))
                    .foregroundStyle(DeskLinkPalette.mutedInk)
            }
            Spacer()
        }
    }

    private func metric(_ title: String, value: Int) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(String(value))
                .font(.system(size: 18, weight: .semibold, design: .rounded))
                .foregroundStyle(DeskLinkPalette.ink)
            Text(title)
                .font(.system(size: 11))
                .foregroundStyle(DeskLinkPalette.mutedInk)
        }
    }

    private func deviceID(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined(separator: ":")
    }

    private func date(_ unixSeconds: UInt64) -> String {
        Date(timeIntervalSince1970: TimeInterval(unixSeconds))
            .formatted(date: .abbreviated, time: .shortened)
    }
}
