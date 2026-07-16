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
            DeskLinkPanel(background: statusBackground) {
                HStack(alignment: .center, spacing: 24) {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack(spacing: 9) {
                            DeskLinkStatusLight(color: statusColor)
                            Text("这台 Mac")
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(DeskLinkPalette.secondaryInk)
                        }
                        Text(statusTitle)
                            .font(.system(size: 24, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                        Text(statusDetail)
                            .font(.system(size: 14))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    Spacer(minLength: 12)
                    HStack(spacing: 8) {
                        if bridge.pairingInvite == nil {
                            Button("创建连接码") { bridge.createInvite() }
                                .buttonStyle(DeskLinkPrimaryButtonStyle())
                                .disabled(!bridge.permissions.canCaptureAndControl)
                        } else {
                            Button("复制连接码") { bridge.copyInviteToPasteboard() }
                                .buttonStyle(DeskLinkPrimaryButtonStyle())
                        }
                        Button("刷新状态") { bridge.refreshPermissions() }
                            .buttonStyle(DeskLinkSecondaryButtonStyle())
                        if hostIsActive {
                            Button("停止共享") { bridge.stop() }
                                .buttonStyle(DeskLinkSecondaryButtonStyle())
                        }
                    }
                }
            }

            if !bridge.permissions.canCaptureAndControl {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "exclamationmark.circle.fill")
                        .foregroundStyle(DeskLinkPalette.warning)
                    VStack(alignment: .leading, spacing: 4) {
                        Text("需要完成 macOS 权限设置")
                            .font(.system(size: 13, weight: .semibold))
                        Text("请在“本机连接”中允许屏幕录制与辅助功能，然后才能创建连接码。")
                            .font(.system(size: 12))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                    }
                    Spacer()
                }
                .padding(14)
                .background(DeskLinkPalette.warningSurface, in: RoundedRectangle(cornerRadius: 8))
            }

            if let approval = bridge.pendingApproval {
                ApprovalView(bridge: bridge, approval: approval)
            }

            HStack(spacing: 0) {
                fact(
                    title: "连接方式",
                    value: "DeskLink 公网中继",
                    detail: "支持不同网络中的两台设备"
                )
                Divider()
                fact(
                    title: "系统权限",
                    value: bridge.permissions.canCaptureAndControl ? "已启用" : "需要设置",
                    detail: "屏幕录制与远程输入"
                )
                Divider()
                fact(
                    title: "已批准设备",
                    value: String(bridge.trustedControllers.count),
                    detail: "可重新连接此 Mac 的设备"
                )
            }
            .frame(minHeight: 94)
            .background(DeskLinkPalette.subtle)
            .overlay(alignment: .top) { Rectangle().fill(DeskLinkPalette.border).frame(height: 1) }
            .overlay(alignment: .bottom) { Rectangle().fill(DeskLinkPalette.border).frame(height: 1) }

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
                    Text("本机运行状态")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    HStack(spacing: 24) {
                        metric("视频数据包", value: bridge.metrics.sentVideoPackets)
                        metric("输入事件", value: bridge.metrics.receivedInputEvents)
                        metric("关键帧请求", value: bridge.metrics.keyframeRequests)
                    }
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
                "本机连接",
                detail: "完成两项 macOS 权限设置，并创建供另一台设备使用的连接码。"
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
                        Button("取消连接码") { bridge.stop() }
                            .buttonStyle(DeskLinkSecondaryButtonStyle())
                    }
                }
            }

            if let approval = bridge.pendingApproval {
                ApprovalView(bridge: bridge, approval: approval)
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
                        Text("请先在“本机连接”创建连接码，然后在另一台设备发起连接并在此 Mac 上批准。")
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

    private func fact(title: String, value: String, detail: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(DeskLinkPalette.mutedInk)
            Text(value)
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(DeskLinkPalette.ink)
            Text(detail)
                .font(.system(size: 11))
                .foregroundStyle(DeskLinkPalette.mutedInk)
        }
        .padding(.horizontal, 18)
        .frame(maxWidth: .infinity, alignment: .leading)
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

    private var statusTitle: String {
        switch bridge.state {
        case .idle, .closed:
            bridge.permissions.canCaptureAndControl ? "可以接收远程连接" : "需要完成系统权限"
        case .connecting: "正在等待另一台设备"
        case .waitingForApproval: "有设备等待批准"
        case .negotiating: "正在建立安全会话"
        case .connected: "此 Mac 正在被控制"
        case .stopping: "正在停止共享"
        case .failed: "本机连接已停止"
        }
    }

    private var statusDetail: String {
        switch bridge.state {
        case .idle, .closed:
            bridge.permissions.canCaptureAndControl
                ? "创建连接码后，可在另一台 Windows 或 Mac 设备上发起连接。"
                : "允许屏幕录制与辅助功能后，DeskLink 才能共享画面和接收输入。"
        case .connecting: "连接码已经创建，DeskLink 正在等待控制端加入中继会话。"
        case .waitingForApproval: "请核对设备身份后决定是否允许此次控制。"
        case .negotiating: "身份已经确认，正在协商画面与输入能力。"
        case .connected: "远程画面和输入通道已启用，可随时停止共享或撤销设备。"
        case .stopping: "DeskLink 正在释放画面、输入和中继连接。"
        case .failed(let message): deskLinkChineseError(message)
        }
    }

    private var statusColor: Color {
        switch bridge.state {
        case .connected: DeskLinkPalette.success
        case .connecting, .waitingForApproval, .negotiating, .stopping: DeskLinkPalette.info
        case .failed: DeskLinkPalette.error
        case .idle, .closed:
            bridge.permissions.canCaptureAndControl ? DeskLinkPalette.success : DeskLinkPalette.warning
        }
    }

    private var statusBackground: Color {
        switch bridge.state {
        case .connected: DeskLinkPalette.successSurface
        case .failed: DeskLinkPalette.errorSurface
        case .connecting, .waitingForApproval, .negotiating, .stopping: DeskLinkPalette.infoSurface
        case .idle, .closed:
            bridge.permissions.canCaptureAndControl ? DeskLinkPalette.successSurface : DeskLinkPalette.warningSurface
        }
    }

    private var hostIsActive: Bool {
        !matchesInactive(bridge.state)
    }

    private func matchesInactive(_ state: HostState) -> Bool {
        switch state {
        case .idle, .closed, .failed: true
        default: false
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
