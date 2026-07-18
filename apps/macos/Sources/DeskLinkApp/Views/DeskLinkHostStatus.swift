import SwiftUI

enum DeskLinkHostStatusTone: Equatable {
    case ready
    case attention
    case idle
    case working
}

struct DeskLinkHostStatus: Equatable {
    let title: String
    let detail: String
    let tone: DeskLinkHostStatusTone

    var systemImage: String {
        switch tone {
        case .ready: "checkmark.circle"
        case .attention: "exclamationmark.circle"
        case .idle: "circle"
        case .working: "arrow.triangle.2.circlepath"
        }
    }
}

func deskLinkHostStatus(
    for state: HostState,
    permissions: MacPermissionSnapshot,
    hasPendingApproval: Bool,
    lastError: String?
) -> DeskLinkHostStatus {
    if lastError != nil {
        return DeskLinkHostStatus(
            title: "需要处理",
            detail: "打开设置检查本机共享权限",
            tone: .attention
        )
    }

    if !permissions.canCaptureAndControl {
        let detail: String
        switch (permissions.screenRecording, permissions.accessibility) {
        case (.denied, .denied):
            detail = "在系统设置中允许屏幕录制与辅助功能"
        case (.denied, .granted):
            detail = "在系统设置中允许屏幕录制"
        case (.granted, .denied):
            detail = "在系统设置中允许辅助功能"
        case (.granted, .granted):
            detail = "打开设置检查本机共享权限"
        }
        return DeskLinkHostStatus(
            title: "需要处理",
            detail: detail,
            tone: .attention
        )
    }

    if hasPendingApproval {
        return DeskLinkHostStatus(
            title: "等待确认",
            detail: "有设备请求控制这台 Mac，请允许或拒绝",
            tone: .working
        )
    }

    switch state {
    case .idle, .closed:
        return DeskLinkHostStatus(
            title: "未开启共享",
            detail: "需要共享这台 Mac 时生成连接码",
            tone: .idle
        )
    case .connecting, .waitingForApproval, .negotiating, .stopping:
        return DeskLinkHostStatus(
            title: "连接中",
            detail: "正在准备本机共享",
            tone: .working
        )
    case .connected:
        return DeskLinkHostStatus(
            title: "正在共享本机",
            detail: "远程设备正在查看并控制这台 Mac",
            tone: .ready
        )
    case .failed:
        return DeskLinkHostStatus(
            title: "需要处理",
            detail: "打开设置查看共享错误",
            tone: .attention
        )
    }
}

func deskLinkHostStatus(for state: HostState, lastError: String?) -> DeskLinkHostStatus {
    deskLinkHostStatus(
        for: state,
        permissions: MacPermissionSnapshot(
            screenRecording: .granted,
            accessibility: .granted,
            screenRecordingSettingsURL: MacPermissions.screenRecordingSettingsURL,
            accessibilitySettingsURL: MacPermissions.accessibilitySettingsURL
        ),
        hasPendingApproval: false,
        lastError: lastError
    )
}

struct DeskLinkHostStatusPopover: View {
    @ObservedObject var host: HostBridge
    let openSettings: () -> Void
    let openSharing: () -> Void

    var body: some View {
        let status = deskLinkHostStatus(
            for: host.state,
            permissions: host.permissions,
            hasPendingApproval: host.pendingApproval != nil,
            lastError: host.lastError
        )

        VStack(alignment: .leading, spacing: 12) {
            Text("本机共享")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(DeskLinkPalette.ink)
            Label(status.title, systemImage: status.systemImage)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(color(for: status.tone))
            Text(status.detail)
                .font(.system(size: 12))
                .foregroundStyle(DeskLinkPalette.secondaryInk)
            Divider()
            Button("打开设置 / 诊断", action: openSettings)
            Button("共享此设备", action: openSharing)
        }
        .padding(16)
        .frame(width: 260, alignment: .leading)
    }

    private func color(for tone: DeskLinkHostStatusTone) -> Color {
        switch tone {
        case .ready: DeskLinkPalette.success
        case .attention: DeskLinkPalette.warning
        case .idle: DeskLinkPalette.mutedInk
        case .working: DeskLinkPalette.info
        }
    }
}
